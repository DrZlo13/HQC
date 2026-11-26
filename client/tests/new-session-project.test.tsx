// @vitest-environment happy-dom
// The project a new session is quick-started in (SPEC §3 User): picked in Settings and kept on
// the user, the personal project until then and when the picked one has left the bar; opened by
// the page's `window.hqc.newSessionInProject()`, which a desktop wrapper's shortcut calls.

import { afterEach, expect, it, vi } from 'vitest';
import { render } from 'preact';
import { act } from 'preact/test-utils';
import { Api } from '../src/api';
import { AppState } from '../src/app';
import type { Me, Project } from '../src/proto';
import type { HubSocket } from '../src/ws';
import { Settings } from '../src/ui/Settings';

let root: HTMLElement;
afterEach(() => {
  if (root) render(null, root);
  document.body.innerHTML = '';
  location.hash = '';
  vi.resetModules();
  vi.doUnmock('../src/app');
});

const ME: Me = { id: 'u', email: 'me@example.com', personal_project_id: 'p0', theme: 'warm', look: 'spacious', project_sort: 'custom', new_session_project_id: null, quick_messages: [] };
const socket = { onState: () => {}, onRequests: () => {}, onSummary: () => {}, start: () => {} } as unknown as HubSocket;
const project = (id: string, name: string, more: Partial<Project> = {}): Project => ({
  id,
  name,
  slug: name,
  description: '',
  is_personal: id === 'p0',
  quick_messages: [],
  color: 'moss',
  created_at: '2026-10-20T00:00:00Z',
  ...more,
});
const projects = (app: AppState) => {
  app.projects.set('p0', project('p0', 'Personal'));
  app.projects.set('p1', project('p1', 'boards'));
  app.projects.set('p2', project('p2', 'kiln'));
};

it('is the personal project until one is picked, and again when the picked one is not on the bar', () => {
  const app = new AppState({} as Api, socket);
  expect(app.newSessionProject()).toBeUndefined();
  app.me = ME;
  expect(app.newSessionProject()).toBeUndefined();
  projects(app);
  expect(app.newSessionProject()!.id).toBe('p0');
  app.me = { ...ME, new_session_project_id: 'p2' };
  expect(app.newSessionProject()!.id).toBe('p2');
  const kiln = app.projects.get('p2')!;
  app.projects.delete('p2');
  app.archivedProjects.set('p2', kiln);
  expect(app.newSessionProject()!.id).toBe('p0');
});

it('Settings: the select lists the projects, shows the effective one and a pick goes to the hub as `PATCH /api/me {new_session_project_id}`', async () => {
  const sent: { method?: string; url: string; body: unknown }[] = [];
  let me = ME;
  const fetchHub: typeof fetch = async (input, init) => {
    const body = JSON.parse(String(init?.body ?? 'null'));
    sent.push({ method: init?.method, url: String(input), body });
    // The hub refuses a project that is not the user's.
    if (body.new_session_project_id === 'p2') {
      return new Response(JSON.stringify({ error: 'not_found', message: 'refused' }), { status: 404, headers: { 'content-type': 'application/json' } });
    }
    me = { ...me, ...body };
    return new Response(JSON.stringify(me), { status: 200, headers: { 'content-type': 'application/json' } });
  };
  const app = new AppState(Object.assign(new Api('', fetchHub), { workers: async () => [], mcpServers: async () => ({ servers: [], local: [] }), skills: async () => [], me: async () => app.me! }), socket);
  app.me = ME;
  projects(app);
  root = document.createElement('div');
  document.body.appendChild(root);
  await act(async () => render(<Settings app={app} />, root));
  const heads = [...root.querySelectorAll('.page-body h2')].map((h) => h.textContent);
  expect(heads.slice(0, 3)).toEqual(['Theme', 'Look', 'New session']);
  const select = root.querySelector<HTMLSelectElement>('select.settings-new-session')!;
  expect([...select.options].map((o) => o.textContent)).toEqual(['Personal', 'boards', 'kiln']);
  expect(select.value).toBe('p0');

  select.value = 'p1';
  await act(async () => void select.dispatchEvent(new Event('change', { bubbles: true })));
  for (let i = 0; i < 20 && app.me?.new_session_project_id !== 'p1'; i++) await act(() => new Promise((r) => setTimeout(r, 10)));
  expect(sent).toEqual([{ method: 'PATCH', url: '/api/me', body: { new_session_project_id: 'p1' } }]);
  expect(app.newSessionProject()!.id).toBe('p1');
  await act(async () => render(<Settings app={app} />, root));
  expect(root.querySelector<HTMLSelectElement>('select.settings-new-session')!.value).toBe('p1');

  // A refusal is shown under the select.
  select.value = 'p2';
  await act(async () => void select.dispatchEvent(new Event('change', { bubbles: true })));
  for (let i = 0; i < 20 && !root.querySelector('.error-text'); i++) await act(() => new Promise((r) => setTimeout(r, 10)));
  expect(root.querySelector('.error-text')!.textContent).toContain('refused');
  expect(app.newSessionProject()!.id).toBe('p1');
});

it("Settings on a phone: the user's quick messages take the phone look of the project settings' editor", async () => {
  const fetchHub: typeof fetch = async () => new Response('{}', { status: 200, headers: { 'content-type': 'application/json' } });
  const app = new AppState(Object.assign(new Api('', fetchHub), { workers: async () => [], mcpServers: async () => ({ servers: [], local: [] }), skills: async () => [], me: async () => app.me! }), socket);
  app.me = { ...ME, quick_messages: ['Merge'] };
  projects(app);
  root = document.createElement('div');
  document.body.appendChild(root);
  // The rules `.is-phone .quick-row …` reach the list only under an `.is-phone` box.
  await act(async () => render(<Settings app={app} />, root));
  expect(root.querySelector('.settings-quick .quick-row')).not.toBeNull();
  expect(root.querySelector('.is-phone .quick-row')).toBeNull();
  await act(async () => render(<Settings app={app} phone />, root));
  expect(root.querySelector('.is-phone .quick-row')).not.toBeNull();
});

it("Settings: a page that does not know the user adds to the hub's list of the user's quick messages, not to an empty one", async () => {
  let hub: Me = { ...ME, quick_messages: ['A'] };
  const sent: unknown[] = [];
  const fetchHub: typeof fetch = async (_input, init) => {
    if (init?.method === 'PATCH') {
      const body = JSON.parse(String(init.body));
      sent.push(body);
      hub = { ...hub, ...body };
    }
    return new Response(JSON.stringify(hub), { status: 200, headers: { 'content-type': 'application/json' } });
  };
  const app = new AppState(Object.assign(new Api('', fetchHub), { workers: async () => [], mcpServers: async () => ({ servers: [], local: [] }), skills: async () => [] }), socket);
  // `me` stays null, as after a first `/api/me` that failed.
  projects(app);
  root = document.createElement('div');
  document.body.appendChild(root);
  await act(async () => render(<Settings app={app} />, root));
  await act(async () => root!.querySelector<HTMLButtonElement>('.settings-quick .btn-quick-add')!.click());
  const field = root.querySelector<HTMLTextAreaElement>('.settings-quick textarea')!;
  field.value = 'B';
  await act(async () => void field.dispatchEvent(new Event('input', { bubbles: true })));
  await act(async () => root!.querySelector<HTMLButtonElement>('.settings-quick .btn-quick-save')!.click());
  for (let i = 0; i < 20 && sent.length === 0; i++) await act(() => new Promise((r) => setTimeout(r, 10)));
  expect(sent).toEqual([{ quick_messages: ['A', 'B'] }]);
});

it("`openNewSessionInProject` opens the project's new-session window, once `me` has arrived when it has not yet", async () => {
  const app = new AppState({} as Api, socket);
  projects(app);
  app.openNewSessionInProject();
  expect(location.hash).toBe('');
  app.me = { ...ME, new_session_project_id: 'p2' };
  (app as unknown as { changed(): void }).changed();
  expect(location.hash).toBe('#/p/p2/new');
  // It waited once: a later change does not open it again.
  location.hash = '#/';
  (app as unknown as { changed(): void }).changed();
  expect(location.hash).toBe('#/');
  app.openNewSessionInProject();
  expect(location.hash).toBe('#/p/p2/new');
});

it('the page exposes `window.hqc.newSessionInProject`, which a desktop wrapper calls', async () => {
  const app = new AppState({} as Api, socket);
  app.me = { ...ME, new_session_project_id: 'p1' };
  projects(app);
  vi.doMock('../src/app', async (orig) => ({ ...(await orig<typeof import('../src/app')>()), createApp: () => app }));
  vi.doMock('../src/push', () => ({ listenForNavigation: () => {}, registerServiceWorker: async () => {} }));
  vi.doMock('../src/favicon', () => ({ watchTabIcon: () => {} }));
  vi.doMock('../src/ui/App', () => ({ App: () => null }));
  app.init = async () => {};
  root = document.createElement('div');
  root.id = 'app';
  document.body.appendChild(root);
  await import('../src/main');
  window.hqc.newSessionInProject();
  expect(location.hash).toBe('#/p/p1/new');
});
