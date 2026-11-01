// T14 through the UI against the mock hub: projects in the left column, a project's
// Workspaces screen with the folder tree of the worker, the conflict when a folder is in
// another project, and a new session in a chosen workspace with a cwd.

import { afterAll, beforeAll, expect, it } from 'vitest';
import { render } from 'preact';
import { Window } from 'happy-dom';
// @ts-expect-error plain JS module without types
import { startMock } from '../mock/server.mjs';
import { Api } from '../src/api';
import { AppState } from '../src/app';
import { App } from '../src/ui/App';
import { HubSocket } from '../src/ws';

let mock: { url: string; close(): Promise<void>; closeVsCode(id: string): void; hideHolder(id: string, hidden: boolean): void };
let win: Window;
let app: AppState;
let root: HTMLElement;
let promptAnswer = '';
const saved: Record<string, PropertyDescriptor | undefined> = {};

function installGlobals(w: Window) {
  const g = globalThis as Record<string, unknown>;
  const map: Record<string, unknown> = {
    window: w,
    document: w.document,
    location: w.location,
    addEventListener: w.addEventListener.bind(w),
    removeEventListener: w.removeEventListener.bind(w),
    prompt: () => promptAnswer,
    confirm: () => true,
    requestAnimationFrame: w.requestAnimationFrame.bind(w),
    cancelAnimationFrame: w.cancelAnimationFrame.bind(w),
  };
  for (const [k, v] of Object.entries(map)) {
    saved[k] = Object.getOwnPropertyDescriptor(g, k);
    Object.defineProperty(g, k, { value: v, configurable: true, writable: true });
  }
}

function restoreGlobals() {
  const g = globalThis as Record<string, unknown>;
  for (const [k, d] of Object.entries(saved)) {
    if (d) Object.defineProperty(g, k, d);
    else delete g[k];
  }
}

async function until<T>(what: string, fn: () => T | null | undefined | false, ms = 5000): Promise<T> {
  const end = Date.now() + ms;
  for (;;) {
    const v = fn();
    if (v) return v;
    if (Date.now() > end) throw new Error(`timeout waiting for ${what}\n${root.textContent}`);
    await new Promise((r) => setTimeout(r, 10));
  }
}

const $ = <E extends Element = HTMLElement>(sel: string) => root.querySelector<E>(sel);
const $$ = (sel: string) => [...root.querySelectorAll<HTMLElement>(sel)];
const button = (text: string, scope = '') =>
  $$(`${scope} button`).find((b) => b.textContent?.trim() === text) as HTMLButtonElement | undefined;

/** A project in the rail, by its name (the rail shows initials, the name is the label). */
const projectRow = (name: string) => $$('.project-row').find((r) => r.getAttribute('aria-label')?.includes(name));

/** An item of the chat's "More" menu, opening the menu first. */
async function menuItem(cls: string): Promise<HTMLButtonElement | null> {
  if (!$('.menu')) {
    $<HTMLButtonElement>('.btn-more')!.click();
    await until('menu', () => $('.menu'));
  }
  return $<HTMLButtonElement>(`.menu ${cls}`);
}

function type(sel: string, value: string) {
  const el = $<HTMLInputElement>(sel)!;
  el.value = value;
  el.dispatchEvent(new win.Event('input', { bubbles: true }) as unknown as Event);
}

function choose(sel: string, value: string) {
  const el = $<HTMLSelectElement>(sel)!;
  el.value = value;
  el.dispatchEvent(new win.Event('change', { bubbles: true }) as unknown as Event);
}

/** Lets Preact re-render after an input, so handlers see the new state. */
const settle = () => new Promise((r) => setTimeout(r, 30));

function submit(sel: string) {
  $(sel)!.dispatchEvent(new win.Event('submit', { bubbles: true, cancelable: true }) as unknown as Event);
}

beforeAll(async () => {
  mock = await startMock({ speed: 0.05 });
  win = new Window({ url: 'http://hqc.test/' });
  installGlobals(win);
  app = new AppState(
    new Api(mock.url, fetch),
    new HubSocket({ url: mock.url.replace('http', 'ws') + '/api/ws', WebSocketImpl: WebSocket }),
  );
  root = win.document.createElement('div') as unknown as HTMLElement;
  win.document.body.appendChild(root as never);
  render(<App app={app} />, root);
  await app.init();
});

afterAll(async () => {
  render(null, root);
  // Effects scheduled before the unmount run on the next frame: still with the globals.
  await new Promise((r) => setTimeout(r, 50));
  app.socket.stop();
  await mock.close();
  await win.happyDOM.close();
  restoreGlobals();
});

it('lists projects and the sessions of the current one', async () => {
  await until('personal project', () => projectRow('Personal'));
  expect($('.project-row.is-current')!.getAttribute('aria-label')).toBe('Personal');
  expect($('.project-row.is-current')!.textContent).toBe('P');
  await until('its sessions', () => $$('.session-row').find((r) => r.textContent?.includes('Empty session')));
});

it('creates a project, adds a workspace from the folder tree and starts a session there', async () => {
  promptAnswer = 'KiCad';
  $<HTMLButtonElement>('button[aria-label="New project"]')!.click();
  await until('workspaces screen', () => $('.workspaces'));
  expect(location.hash).toMatch(/^#\/p\/[^/]+\/settings$/);
  const pid = location.hash.split('/')[2];
  await until('current project', () => $('.project-row.is-current')?.getAttribute('aria-label') === 'KiCad');
  expect($('.project-row.is-current')!.textContent).toBe('KI');
  expect($('.side-head .project-title')!.textContent).toBe('KiCad');
  expect($$('.session-row')).toHaveLength(0);
  await until('no workspaces yet', () => $('.workspaces .empty-inline'));

  // Folder tree of the online worker: roots, then down to kicad/power-board.
  button('Add a workspace')!.click();
  const entry = (name: string) => $$('.dir-entry').find((b) => b.textContent?.includes(name));
  (await until('roots', () => entry('work')))!.click();
  (await until('work/', () => entry('kicad')))!.click();
  (await until('kicad/', () => entry('power-board')))!.click();
  await until('power-board/', () => $('.dir-browser')?.textContent?.includes('No subfolders'));
  button('Pick this folder')!.click();
  await until('path picked', () => $<HTMLInputElement>('input[name=path]')?.value.endsWith('/kicad/power-board'));
  button('↑ Up')!.click();
  await until('back up', () => entry('power-board'));
  await settle();
  submit('.add-workspace');
  await until('workspace listed', () => $$('.workspace-row').find((r) => r.textContent?.includes('/kicad/power-board')));
  expect($('.workspace-row .tag')!.textContent).toBe('default');
  expect(app.projects.get(pid)!.default_workspace_id).toBe(app.workspaces.get(pid)![0].id);

  // A folder that is already in the personal project: the message names it, with a link.
  button('Add a workspace')!.click();
  await until('add form', () => $('.add-workspace input[name=path]'));
  type('.add-workspace input[name=path]', '/Users/home-mac/work/hqc/crates');
  await until('add enabled', () => !button('Add', '.add-workspace')?.disabled);
  submit('.add-workspace');
  const err = await until('conflict', () => $('.add-workspace .error-text'));
  expect(err.textContent).toContain('This folder is already in project “Personal”');
  expect(err.querySelector('a')!.getAttribute('href')).toBe(`#/p/${app.me!.personal_project_id}/settings`);
  button('Cancel', '.add-workspace')!.click();

  // A session in that workspace, in a subfolder.
  location.hash = `#/p/${pid}`;
  await until('project route', () => !$('.workspaces'));
  button('New')!.click();
  await until('new session form', () => $('.new-session select[name=workspace]'));
  await until('workspaces loaded', () => $$('.new-session select[name=workspace] option').length === 1);
  expect($('.new-session select[name=workspace] option')!.textContent).toContain('(default)');
  choose('.new-session select[name=workspace]', app.workspaces.get(pid)![0].id);
  type('.new-session input[name=cwd]', 'pcb');
  // T38: Auto in the form, confirmed (the globals answer every confirm with yes).
  expect($('.new-session select[name=permission_mode]')!.textContent).toBe("The worker's defaultAskAuto");
  choose('.new-session select[name=permission_mode]', 'auto');
  await settle();
  submit('.new-session');
  await until('chat', () => $('.chat textarea'));
  const sid = location.hash.split('/')[2];
  const s = await app.api.session(sid);
  expect(s).toMatchObject({ project_id: pid, workspace_id: app.workspaces.get(pid)![0].id, cwd: 'pcb' });
  expect(s).toMatchObject({ permission_mode: 'auto', effective_permission_mode: 'auto' });
  expect($('.chat .perm-auto')!.textContent).toBe('Auto');
  await until('listed in the project', () => $$('.session-row').find((r) => r.getAttribute('href') === `#/s/${sid}`));
  // The personal project's sessions are not shown here.
  expect($$('.session-row').some((r) => r.textContent?.includes('Empty session'))).toBe(false);

  // Its workspace stays while the session lives.
  location.hash = `#/p/${pid}/settings`;
  (await until('remove button', () => $('.btn-remove-workspace') as HTMLButtonElement | null))!.click();
  await until('in use', () => $('.workspaces .error-text')?.textContent?.includes('Archive these sessions first'));
});

it('switches projects from the left column and deletes a project', async () => {
  const personal = projectRow('Personal')!;
  location.hash = personal.getAttribute('href')!;
  await until('personal sessions', () => $$('.session-row').find((r) => r.textContent?.includes('Empty session')));
  // The personal project has no delete button.
  location.hash = `#/p/${app.me!.personal_project_id}/settings`;
  await until('personal workspaces', () => $$('.workspace-row').length === 1);
  expect($('.btn-delete-project')).toBeNull();

  expect($('.btn-rename-project')).toBeNull();

  const kicad = app.sortedProjects().find((p) => p.name === 'KiCad')!;
  location.hash = `#/p/${kicad.id}/settings`;
  promptAnswer = 'KiCad boards';
  (await until('rename button', () => $('.btn-rename-project') as HTMLButtonElement | null))!.click();
  await until('renamed', () => projectRow('KiCad boards'));
  expect((await app.api.projects()).find((p) => p.id === kicad.id)!.slug).toBe('kicad');
  $<HTMLButtonElement>('.btn-delete-project')!.click();
  await until('gone', () => !projectRow('KiCad'));
  expect((await app.api.projects()).some((p) => p.id === kicad.id)).toBe(false);
});

it(`lists the worker's service session apart: no input, request cards, "requests only" (T17)`, async () => {
  const personal = projectRow('Personal')!;
  location.hash = personal.getAttribute('href')!;
  const row = await until('workers group', () =>
    $$('.worker-list .worker-row').find((r) => r.textContent?.includes('home-mac')),
  );
  expect($('.worker-list .group-title')!.textContent).toBe('Workers');
  expect(row.textContent).toContain('waiting for reply');
  // Not among the sessions of the personal project, where it lives.
  await until('personal sessions', () => $$('.session-row').find((r) => r.textContent?.includes('Empty session')));
  expect($$('.session-row').some((r) => r.getAttribute('href') === row.getAttribute('href'))).toBe(false);

  location.hash = row.getAttribute('href')!;
  await until('log lines', () => $$('.line-worker-log').length === 3);
  expect($('.chat textarea')).toBeNull();
  expect($('.chat .chat-system-note')!.textContent).toBe("the worker's service session");
  expect($('.line-worker-log.level-error .log-text')!.textContent).toContain('known folders not saved');
  button('Requests only')!.click();
  await until('filtered', () => $$('.line-worker-log').length === 0 && $('.chat .perm'));
  expect($('.perm-title')!.textContent).toBe('The worker asks for permission');
  $<HTMLButtonElement>('.perm button[data-canonical="once"]')!.click();
  await until('answered', () => $('.perm-done-row .outcome')?.textContent?.includes('allowed'));
  button('Requests only')!.click();
  await until('unfiltered', () => $$('.line-worker-log').length === 3);
});

// T15 (SPEC §9): import a folder with Claude Code sessions, see the session read-only as
// "In VS Code", take it over and write.
it('imports from Claude Code and takes a session over', async () => {
  // A project named in Russian, as users may name theirs.
  promptAnswer = 'Платы';
  $<HTMLButtonElement>('button[aria-label="New project"]')!.click();
  await until('workspaces screen', () => $('.workspaces'));
  const pid = location.hash.split('/')[2];
  button('Import from Claude Code')!.click();
  const row = await until('candidates', () =>
    $$('.candidate').find((r) => r.textContent?.includes('/kicad/power-board')),
  );
  expect(row.textContent).toContain('sessions: 1');
  expect($$('.candidate')).toHaveLength(2);
  expect(button('Import')!.disabled).toBe(true);
  row.querySelector<HTMLInputElement>('input[type=checkbox]')!.click();
  await until('import enabled', () => !button('Import')?.disabled);
  button('Import')!.click();
  const ws = await until('workspace listed', () =>
    $$('.workspace-row').find((r) => r.textContent?.includes('/kicad/power-board')),
  );
  const toggle = ws.querySelector<HTMLInputElement>('.mirror-toggle input')!;
  expect(ws.querySelector('.mirror-toggle')!.textContent).toBe('VS Code sessions');
  expect(toggle.checked).toBe(true);
  expect(app.workspaces.get(pid)![0].mirror_enabled).toBe(true);
  // T21: the switch goes to the hub (PATCH) and back.
  toggle.click();
  await until('mirror off', () => app.workspaces.get(pid)![0].mirror_enabled === false);
  await until('unchecked', () => !$<HTMLInputElement>('.workspace-row .mirror-toggle input')!.checked);
  $<HTMLInputElement>('.workspace-row .mirror-toggle input')!.click();
  await until('mirror on', () => app.workspaces.get(pid)![0].mirror_enabled === true);

  location.hash = `#/p/${pid}`;
  const link = await until('imported session', () =>
    $$('.session-row').find((r) => r.textContent?.includes('From VS Code: power-board')),
  );
  location.hash = link.getAttribute('href')!;
  await until('history', () => $('.chat')?.textContent?.includes('VSCODE-DONE'));
  // Last driven by VS Code, nobody holds it now: the banner instead of the composer, with
  // "Take over here".
  const banner = await until('locked', () => $('.handover'));
  expect(banner.classList.contains('handover-held')).toBe(false);
  expect(banner.querySelector('.handover-title')!.textContent).toBe('VS Code was the last to run this session on home-mac.');
  expect(banner.textContent).not.toContain('read-only');
  expect($('.chat .state')!.textContent).toBe('In VS Code');
  expect($('.chat textarea')).toBeNull();

  button('Take over here')!.click();
  await until('taken over', () => !$('.handover'));
  await until('system line', () => $('.chat')?.textContent?.includes('Taken over from VS Code'));
  expect($<HTMLTextAreaElement>('.chat textarea')!.disabled).toBe(false);
});

// T20 (SPEC §9.3): the import brought a session that is open in VS Code right now and whose
// transcript forked. No "Take over" until it is closed there; a takeover the hub refuses
// (`409 vscode_open`) shows its text; closed in VS Code, it is free at once.
it('does not take over a session open in VS Code', async () => {
  const link = await until('open session', () =>
    $$('.session-row').find((r) => r.textContent?.includes('Open in VS Code: power-board')),
  );
  location.hash = link.getAttribute('href')!;
  const sid = location.hash.split('/').pop()!;
  // Open in VS Code right now: no button, close it there first.
  const banner = await until('open banner', () => $('.handover.handover-held'));
  expect(banner.querySelector('.handover-title')!.textContent).toBe('The session is open in VS Code on home-mac.');
  expect(banner.querySelector('.handover-sub')!.textContent).toBe('Close it there to write from here.');
  expect(button('Take over here')).toBeUndefined();
  expect($('.chat textarea')).toBeNull();
  const fork = await until('fork line', () => $('.line-system-session_forked'));
  expect(fork.querySelector('.warn-icon')).not.toBeNull();
  expect(fork.textContent).toContain('is not in its context');

  // The client has not heard of the holder yet (its summaries do not mention it): "Take over"
  // is offered and the hub refuses it with its text.
  mock.hideHolder(sid, true);
  await until('plain banner', () => $('.handover') && !$('.handover-held'));
  button('Take over here')!.click();
  const notice = await until('notice', () => $('.notice'));
  expect(notice.textContent).toBe('the session is open in VS Code: close it there and take it over again');
  mock.hideHolder(sid, false);
  await until('open banner again', () => $('.handover.handover-held'));

  mock.closeVsCode(sid);
  await until('free', () => !$('.handover'));
  expect($<HTMLTextAreaElement>('.chat textarea')!.disabled).toBe(false);
});

// T50: archive from the row (Undo first, then for real by leaving the page), the archived
// session read-only, restore and delete from archive mode.
it('archives from the row, restores and deletes from the archive', async () => {
  const personal = projectRow('Personal')!;
  location.hash = personal.getAttribute('href')!;
  const row = await until('a session', () => $$('.session-row').find((r) => r.textContent?.includes('Empty session')));
  const href = row.getAttribute('href')!;
  const sid = href.split('/').pop()!;
  const pid = app.sessions.get(sid)!.project_id;
  location.hash = href;
  await until('chat with the menu', () => $('.chat') && $('.btn-more'));
  // Not in the header any more.
  expect(await menuItem('.btn-archive')).toBeNull();
  expect(await menuItem('.btn-delete')).toBeNull();
  $<HTMLButtonElement>('.btn-more')!.click();
  const item = () => $$('.session-item').find((r) => r.querySelector('.session-row')?.getAttribute('href') === href);
  const archiveRow = () => item()!.querySelector<HTMLButtonElement>('.btn-row-archive')!.click();

  archiveRow();
  await until('hidden with Undo', () => !item() && $('.toast-archive'));
  button('Undo')!.click();
  await until('back', () => item() && !$('.toast-archive'));
  expect(app.sessions.get(sid)!.archived).toBe(false);

  archiveRow();
  await until('hidden', () => !item());
  win.dispatchEvent(new win.Event('pagehide'));
  await until('archived on the hub', () => app.sessions.get(sid)!.archived_at);
  await until('read-only', () => $('.chat .state')?.textContent === 'Archived' && $('.archived-band') && !$('.chat textarea'));
  expect($('.archived-band')!.textContent).toMatch(/^Archived today at \d\d:\d\d · read-only\./);

  $<HTMLButtonElement>('.btn-archive-open')!.click();
  const archived = () => $$('.archive-row').find((r) => r.querySelector('a')?.getAttribute('href') === href);
  await until('listed in the archive', () => $('.sessions-archive') && archived());
  archived()!.querySelector<HTMLButtonElement>('.btn-restore')!.click();
  await until('restored', () => $('.chat .state')?.textContent === 'Idle' && !archived());
  expect($('.chat textarea')).not.toBeNull();
  $<HTMLButtonElement>('.btn-archive-back')!.click();
  await until('back in the list', () => item());

  archiveRow();
  win.dispatchEvent(new win.Event('pagehide'));
  await until('archived again', () => app.sessions.get(sid)!.archived_at);
  $<HTMLButtonElement>('.btn-archive-open')!.click();
  await until('in the archive', () => archived());
  archived()!.querySelector<HTMLButtonElement>('.btn-delete')!.click();
  await until('confirm', () => archived()!.querySelector('.btn-confirm-delete'));
  archived()!.querySelector<HTMLButtonElement>('.btn-confirm-delete')!.click();
  await until('deleted', () => !app.sessions.has(sid));
  expect(location.hash).toBe(`#/p/${pid}`);
  $<HTMLButtonElement>('.btn-archive-back')?.click();
});
