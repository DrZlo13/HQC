// @vitest-environment happy-dom
// The keys of the actions (`keys.ts`), pressed on the whole page: each acts by its default key and
// by one picked in Settings, with the focus in the message field too; a cleared key is the page's.
// A file of its own: a component another test leaves mounted keeps its window key listener, which
// would act on the keys of this one.

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { render } from 'preact';
import { act } from 'preact/test-utils';
import type { Api } from '../src/api';
import { AppState } from '../src/app';
import type { Key } from '../src/keys';
import type { SessionInfo, SessionState } from '../src/proto';
import type { HubSocket } from '../src/ws';
import { App } from '../src/ui/App';
import { ProjectRail } from '../src/ui/ProjectRail';

const NOW = Date.parse('2026-10-24T19:50:00');
const at = (hoursAgo: number) => new Date(NOW - hoursAgo * 3_600_000).toISOString();

function sess(id: string, title: string, state: SessionState, updated: string, more: Partial<SessionInfo> = {}): SessionInfo {
  return {
    id,
    project_id: 'p1',
    workspace_id: 'w1',
    worker_id: 'wk1',
    cwd: '.',
    title,
    slug: id,
    state,
    queue_len: 0,
    unread: 0,
    last_seq: 1,
    last_read_seq: 1,
    archived: state === 'archived',
    updated_at: updated,
    system: false,
    ...more,
  };
}

const LIST = [
  sess('stones', 'Stone textures', 'running', at(0.01)),
  sess('det', 'Step determinism', 'running', at(0.1), { worker_id: 'wk2', workspace_id: 'w2' }),
  sess('net', 'Network sync', 'waiting', at(0.2), { cwd: 'net' }),
  sess('prof', 'Collision profiling', 'idle', at(22)),
  sess('cave', 'Cave scene port', 'idle', at(60)),
];

function hubApi() {
  return {
    workspaces: async () => [],
    archivedProjects: vi.fn(async () => []),
    search: async () => ({ results: [] }),
    sessionOrder: vi.fn(async () => ({ mode: 'state', session_ids: [] })),
    archive: vi.fn(async (id: string) => ({ ...sess(id, id, 'archived', at(0)), archived_at: at(0) })),
    workers: async () => [],
    pushKey: async () => null,
  };
}

function makeApp(list = LIST) {
  const api = { ...hubApi(), eventsBefore: vi.fn(async () => ({ events: [], invalid: [], maxSeq: 0, count: 0 })), readCursor: vi.fn(async () => {}) };
  const socket = { onState: () => () => {}, subscribe: vi.fn(() => () => {}) } as unknown as HubSocket;
  const app = new AppState(api as unknown as Api, socket);
  app.me = { id: 'u', email: 'who@example.com', personal_project_id: 'p0', theme: 'warm', look: 'spacious', project_sort: 'custom', new_session_project_id: null, quick_messages: [] };
  app.projects.set('p0', { id: 'p0', name: 'Personal', slug: 'personal', description: '', is_personal: true, color: 'slate', quick_messages: [], created_at: at(100) });
  app.projects.set('p1', { id: 'p1', name: 'physics-mp', slug: 'physics-mp', description: '', is_personal: false, color: 'blue', quick_messages: [], created_at: at(90) });
  app.currentProjectId = 'p1';
  app.conn = 'open';
  for (const s of list) app.sessions.set(s.id, s);
  app.sessions.set('sys1', sess('sys1', 'home-mac', 'idle', at(1), { system: true, project_id: 'p0', workspace_id: undefined }));
  return app;
}

let root: HTMLElement;
beforeEach(() => vi.useFakeTimers({ toFake: ['Date'] }));
afterEach(() => {
  if (root) render(null, root);
  document.body.innerHTML = '';
  localStorage.clear();
  vi.useRealTimers();
});
async function mount(node: preact.ComponentChild) {
  root = document.createElement('div');
  document.body.appendChild(root);
  await act(async () => render(node, root));
}
/** The window's width for the app's media queries: happy-dom's own 1024 px is the narrow
 *  layout (900–1079 px), a desktop is wider. */
const windowWidth = (width: number) =>
  (window as unknown as { happyDOM: { setViewport(v: { width: number }): void } }).happyDOM.setViewport({ width });
const titles = () => [...root.querySelectorAll('.session-title')].map((t) => t.textContent);
const click = (el: Element | null) =>
  act(async () => {
    (el as HTMLElement).click();
    for (let i = 0; i < 5; i++) await Promise.resolve();
  });
/** Reorder, from the header's project menu. */
async function startReorder() {
  await click(root.querySelector('.side-head .btn-project-menu'));
  await click(root.querySelector('.menu .btn-reorder'));
}

describe('the keys of the actions', () => {
  const key = (code: string, mods: Partial<Omit<Key, 'code'>> = {}): Key => ({ code, meta: false, ctrl: false, alt: false, shift: false, ...mods });
  type Mods = Partial<Record<'metaKey' | 'ctrlKey' | 'altKey' | 'shiftKey', boolean>>;
  // The defaults of a Mac.
  const NEW = { code: 'KeyN', metaKey: true };
  const ARCHIVE = { code: 'KeyA', ctrlKey: true, metaKey: true };
  const PREVIOUS_SESSION = { code: 'ArrowUp', altKey: true, metaKey: true };
  const NEXT_SESSION = { code: 'ArrowDown', altKey: true, metaKey: true };
  const PREVIOUS_PROJECT = { code: 'ArrowUp', ctrlKey: true, metaKey: true };
  const NEXT_PROJECT = { code: 'ArrowDown', ctrlKey: true, metaKey: true };
  let platform: ReturnType<typeof vi.spyOn>;

  beforeEach(() => {
    platform = vi.spyOn(navigator, 'platform', 'get').mockReturnValue('MacIntel');
    windowWidth(1400);
  });
  afterEach(() => {
    platform.mockRestore();
    vi.restoreAllMocks();
    location.hash = '';
    windowWidth(1024);
  });

  const settle = () =>
    act(async () => {
      for (let i = 0; i < 4; i++) await new Promise((r) => setTimeout(r, 0));
    });
  const go = async (hash: string) => {
    await act(async () => void (location.hash = hash));
    await settle();
  };
  async function open(app: AppState, hash: string) {
    location.hash = hash;
    await mount(<App app={app} />);
    await settle();
  }
  /** A key pressed where the focus is; whether the page took it (prevented it). */
  async function press(k: { code: string } & Mods): Promise<boolean> {
    const { code, ...mods } = k;
    const e = new KeyboardEvent('keydown', { code, key: code, bubbles: true, cancelable: true, ...mods });
    await act(async () => void (document.activeElement ?? document.body).dispatchEvent(e));
    await settle();
    return e.defaultPrevented;
  }
  const hrefs = (sel: string) => [...root.querySelectorAll(sel)].map((a) => a.getAttribute('href'));

  it('New session: the new-session screen of the current project, from any screen and from the message field; none before the user is known', async () => {
    const app = makeApp();
    await open(app, '#/s/prof');
    const field = root.querySelector<HTMLTextAreaElement>('textarea')!;
    await act(async () => field.focus());
    expect(document.activeElement).toBe(field);
    expect(await press(NEW)).toBe(true);
    expect(location.hash).toBe('#/p/p1/new');
    await go('#/settings');
    expect(await press(NEW)).toBe(true);
    expect(location.hash).toBe('#/p/p1/new');
    // Off a Mac the default is Alt+N.
    await go('#/s/prof');
    platform.mockReturnValue('Linux x86_64');
    expect(await press(NEW)).toBe(false);
    expect(await press({ code: 'KeyN', altKey: true })).toBe(true);
    expect(location.hash).toBe('#/p/p1/new');
    platform.mockReturnValue('MacIntel');
    // A key picked in Settings: the old one is the page's again.
    await go('#/s/prof');
    app.setKey('newSession', key('KeyJ', { ctrl: true }));
    expect(await press(NEW)).toBe(false);
    expect(location.hash).toBe('#/s/prof');
    expect(await press({ code: 'KeyJ', ctrlKey: true })).toBe(true);
    expect(location.hash).toBe('#/p/p1/new');
    // Without a key: nothing is taken.
    await go('#/s/prof');
    app.setKey('newSession', null);
    expect(await press({ code: 'KeyJ', ctrlKey: true })).toBe(false);
    expect(location.hash).toBe('#/s/prof');
    // The user not known yet.
    app.setKey('newSession', undefined);
    app.me = null;
    expect(await press(NEW)).toBe(true);
    expect(location.hash).toBe('#/s/prof');
  });

  it('Archive session: the open session, with the Undo toast, where its row is in the list', async () => {
    const app = makeApp();
    await open(app, '#/s/prof');
    const field = root.querySelector<HTMLTextAreaElement>('textarea')!;
    await act(async () => field.focus());
    expect(await press(ARCHIVE)).toBe(true);
    expect(app.pendingArchive?.id).toBe('prof');
    expect(root.querySelector('.toast-archive .btn-undo')).not.toBeNull();
    expect(titles()).not.toContain('Collision profiling');
    await click(root.querySelector('.toast-archive .btn-undo'));
    expect(app.pendingArchive).toBeNull();
    // A key picked in Settings.
    app.setKey('archiveSession', key('KeyZ', { alt: true }));
    expect(await press(ARCHIVE)).toBe(false);
    expect(app.pendingArchive).toBeNull();
    expect(await press({ code: 'KeyZ', altKey: true })).toBe(true);
    expect(app.pendingArchive?.id).toBe('prof');
    await click(root.querySelector('.toast-archive .btn-undo'));
    // Without a key, nothing is taken.
    app.setKey('archiveSession', null);
    expect(await press({ code: 'KeyZ', altKey: true })).toBe(false);
    expect(app.pendingArchive).toBeNull();
    app.setKey('archiveSession', undefined);
    // No row to archive: not the project's screen, not a service session.
    await go('#/p/p1');
    expect(await press(ARCHIVE)).toBe(true);
    expect(app.pendingArchive).toBeNull();
    await go('#/s/sys1');
    expect(await press(ARCHIVE)).toBe(true);
    expect(app.pendingArchive).toBeNull();
    // A narrow window shows no list and so no Undo: the key archives nothing.
    windowWidth(1000);
    await go('#/s/prof');
    expect(root.querySelector('.session-row')).toBeNull();
    expect(await press(ARCHIVE)).toBe(false);
    expect(app.pendingArchive).toBeNull();
  });

  it('Previous and next session: the rows as the list shows them, from the current one; the ends, no current row, the draft', async () => {
    const app = makeApp(LIST.map((s) => (s.id === 'prof' ? { ...s, favorite_at: at(1) } : s)));
    await open(app, '#/s/stones');
    // The favorites first, then the groups.
    const rows = ['#/s/prof', '#/s/net', '#/s/stones', '#/s/det', '#/s/cave'];
    expect(hrefs('aside.sidebar a.session-row')).toEqual(rows);
    expect(await press(NEXT_SESSION)).toBe(true);
    expect(location.hash).toBe('#/s/det');
    expect(await press(PREVIOUS_SESSION)).toBe(true);
    expect(await press(PREVIOUS_SESSION)).toBe(true);
    expect(location.hash).toBe('#/s/net');
    // From the message field too.
    const field = root.querySelector<HTMLTextAreaElement>('textarea');
    if (field) await act(async () => field.focus());
    expect(await press(PREVIOUS_SESSION)).toBe(true);
    expect(location.hash).toBe('#/s/prof');
    // Past either end: nothing.
    expect(await press(PREVIOUS_SESSION)).toBe(true);
    expect(location.hash).toBe('#/s/prof');
    await go('#/s/cave');
    expect(await press(NEXT_SESSION)).toBe(true);
    expect(location.hash).toBe('#/s/cave');
    // No current row: the last, the first.
    await go('#/p/p1');
    expect(await press(PREVIOUS_SESSION)).toBe(true);
    expect(location.hash).toBe('#/s/cave');
    await go('#/p/p1');
    expect(await press(NEXT_SESSION)).toBe(true);
    expect(location.hash).toBe('#/s/prof');
    // The new session's draft row is the first.
    await go('#/p/p1/new');
    expect(root.querySelector('aside.sidebar a.is-draft')).not.toBeNull();
    expect(await press(PREVIOUS_SESSION)).toBe(true);
    expect(location.hash).toBe('#/p/p1/new');
    expect(await press(NEXT_SESSION)).toBe(true);
    expect(location.hash).toBe('#/s/prof');
    // Keys picked in Settings.
    await go('#/s/net');
    app.setKey('nextSession', key('KeyJ', { ctrl: true }));
    app.setKey('previousSession', key('KeyH', { ctrl: true }));
    expect(await press(NEXT_SESSION)).toBe(false);
    expect(await press(PREVIOUS_SESSION)).toBe(false);
    expect(location.hash).toBe('#/s/net');
    expect(await press({ code: 'KeyJ', ctrlKey: true })).toBe(true);
    expect(location.hash).toBe('#/s/stones');
    expect(await press({ code: 'KeyH', ctrlKey: true })).toBe(true);
    expect(location.hash).toBe('#/s/net');
    // Without a key: the page's.
    app.setKey('nextSession', null);
    expect(await press({ code: 'KeyJ', ctrlKey: true })).toBe(false);
    expect(location.hash).toBe('#/s/net');
  });

  it('Previous and next session do nothing in Reorder, and are not taken where the list is not shown', async () => {
    const app = makeApp();
    await open(app, '#/s/stones');
    await startReorder();
    expect(root.querySelector('.is-reordering')).not.toBeNull();
    await press(NEXT_SESSION);
    expect(location.hash).toBe('#/s/stones');
    await press(PREVIOUS_SESSION);
    expect(location.hash).toBe('#/s/stones');
    await click(root.querySelector('.btn-reorder-cancel'));
    expect(await press(NEXT_SESSION)).toBe(true);
    expect(location.hash).toBe('#/s/det');
    // A key a field has taken is the field's.
    const taker = document.createElement('input');
    taker.addEventListener('keydown', (e) => e.preventDefault());
    root.appendChild(taker);
    await act(async () => taker.focus());
    await press(NEXT_SESSION);
    expect(location.hash).toBe('#/s/det');
    taker.remove();
    // A narrow window has no column.
    windowWidth(1000);
    await go('#/s/stones');
    expect(root.querySelector('.session-row')).toBeNull();
    expect(await press(NEXT_SESSION)).toBe(false);
    expect(location.hash).toBe('#/s/stones');
  });

  it('Previous and next project: the tiles of the bar, from the current one; the ends, no current project, Reorder', async () => {
    const app = makeApp();
    app.projects.set('p2', { id: 'p2', name: 'kiln', slug: 'kiln', description: '', is_personal: false, color: 'coral', quick_messages: [], created_at: at(80) });
    await open(app, '#/p/p1');
    const tiles = hrefs('.rail-project');
    expect(tiles).toHaveLength(3);
    await go(tiles[1]!);
    expect(await press(NEXT_PROJECT)).toBe(true);
    expect(location.hash).toBe(tiles[2]);
    expect(await press(NEXT_PROJECT)).toBe(true);
    expect(location.hash).toBe(tiles[2]);
    expect(await press(PREVIOUS_PROJECT)).toBe(true);
    expect(await press(PREVIOUS_PROJECT)).toBe(true);
    expect(location.hash).toBe(tiles[0]);
    expect(await press(PREVIOUS_PROJECT)).toBe(true);
    expect(location.hash).toBe(tiles[0]);
    // Keys picked in Settings.
    app.setKey('nextProject', key('KeyJ', { ctrl: true }));
    app.setKey('previousProject', null);
    expect(await press(NEXT_PROJECT)).toBe(false);
    expect(location.hash).toBe(tiles[0]);
    expect(await press({ code: 'KeyJ', ctrlKey: true })).toBe(true);
    expect(location.hash).toBe(tiles[1]);
    expect(await press(PREVIOUS_PROJECT)).toBe(false);
    expect(location.hash).toBe(tiles[1]);
    // Reorder: the tiles are being arranged.
    app.setKey('nextProject', undefined);
    await click(root.querySelector('.rail-order'));
    await click(root.querySelector('.menu .btn-reorder-projects'));
    expect(root.querySelector('.rail.is-reordering')).not.toBeNull();
    await press(NEXT_PROJECT);
    expect(location.hash).toBe(tiles[1]);
    // No current project.
    render(null, root);
    app.currentProjectId = null;
    await mount(<ProjectRail app={app} />);
    expect(await press(NEXT_PROJECT)).toBe(true);
    expect(location.hash).toBe(tiles[1]);
  });

  it('Back and Forward: the history, from the message field too', async () => {
    const back = vi.spyOn(history, 'back').mockImplementation(() => {});
    const forward = vi.spyOn(history, 'forward').mockImplementation(() => {});
    const app = makeApp();
    await open(app, '#/s/prof');
    await act(async () => root.querySelector<HTMLTextAreaElement>('textarea')!.focus());
    expect(await press({ code: 'BracketLeft', metaKey: true })).toBe(true);
    expect([back.mock.calls.length, forward.mock.calls.length]).toEqual([1, 0]);
    expect(await press({ code: 'BracketRight', metaKey: true })).toBe(true);
    expect([back.mock.calls.length, forward.mock.calls.length]).toEqual([1, 1]);
    // Keys picked in Settings; a cleared one is the page's.
    app.setKey('back', key('ArrowLeft', { alt: true }));
    app.setKey('forward', null);
    expect(await press({ code: 'BracketLeft', metaKey: true })).toBe(false);
    expect(await press({ code: 'BracketRight', metaKey: true })).toBe(false);
    expect(await press({ code: 'ArrowLeft', altKey: true })).toBe(true);
    expect([back.mock.calls.length, forward.mock.calls.length]).toEqual([2, 1]);
  });

  it('the project search and the sessions of a narrow window follow their keys', async () => {
    const app = makeApp();
    await open(app, '#/s/prof');
    const field = () => root.querySelector('input[aria-label="Search projects"]');
    const SEARCH = { code: 'KeyK', metaKey: true };
    app.setKey('projectSearch', key('KeyJ', { ctrl: true }));
    expect(await press(SEARCH)).toBe(false);
    expect(field()).toBeNull();
    expect(await press({ code: 'KeyJ', ctrlKey: true })).toBe(true);
    expect(field()).not.toBeNull();
    await press({ code: 'Escape' });
    expect(root.querySelector('.rail-search')!.getAttribute('title')).toBe('Search projects (⌃J)');
    await act(async () => app.setKey('projectSearch', null));
    expect(root.querySelector('.rail-search')!.getAttribute('title')).toBe('Search projects');
    expect(await press({ code: 'KeyJ', ctrlKey: true })).toBe(false);
    expect(field()).toBeNull();
    // The sessions of a narrow window.
    windowWidth(1000);
    await go('#/s/prof');
    const slid = () => root.querySelector('.layout > .sidebar-slide');
    const SESSIONS = { code: 'Backslash', metaKey: true };
    expect(await press(SESSIONS)).toBe(true);
    expect(slid()).not.toBeNull();
    await press(SESSIONS);
    expect(slid()).toBeNull();
    app.setKey('sessions', key('KeyG', { alt: true }));
    expect(await press(SESSIONS)).toBe(false);
    expect(slid()).toBeNull();
    expect(await press({ code: 'KeyG', altKey: true })).toBe(true);
    expect(slid()).not.toBeNull();
    app.setKey('sessions', null);
    expect(await press({ code: 'KeyG', altKey: true })).toBe(false);
    expect(slid()).not.toBeNull();
  });
});
