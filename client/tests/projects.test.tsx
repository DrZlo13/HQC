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

let mock: {
  url: string;
  close(): Promise<void>;
  closeVsCode(id: string): void;
  hideHolder(id: string, hidden: boolean): void;
  finishCreating(): void;
  spawnWorktree(originId: string, title: string): string;
  removedWorkspaces: { path: string; folder_removed?: boolean; forced?: boolean }[];
};
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
    localStorage: w.localStorage,
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

/** The settings screen of the project named `name`. A new project is current in the rail
 *  before the route opens its settings, so the previous project's screen may still be shown. */
const settingsOf = (name: string) =>
  $('.workspaces') && $('.project-settings .bar-title .state')?.textContent === name;

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

  // A session in that workspace, in a subfolder: "+" opens its window at once, the chips over
  // the input choose, the first message creates it.
  location.hash = `#/p/${pid}`;
  await until('project route', () => !$('.workspaces'));
  $<HTMLButtonElement>('.side-head .btn-new-session')!.click();
  await until('new session window', () => location.hash === `#/p/${pid}/new` && $('.chat-new .btn-chip-workspace'));
  await until('workspaces loaded', () => $('.btn-chip-workspace')!.textContent!.startsWith('power-board'));
  $<HTMLButtonElement>('.btn-chip-workspace')!.click();
  await until('workspace menu', () => $('.ctx-menu .btn-pick-workspace'));
  expect($$('.ctx-menu .btn-pick-workspace')).toHaveLength(1);
  expect($('.ctx-menu .btn-pick-workspace')!.textContent).toContain('power-board (default)');
  expect($('.ctx-menu .btn-pick-workspace')!.textContent).toContain('/kicad/power-board');
  $<HTMLButtonElement>('.ctx-menu .btn-pick-workspace')!.click();
  await settle();
  $<HTMLButtonElement>('.btn-chip-folder')!.click();
  await until('folder field', () => $('.ctx-menu input[name=cwd]'));
  type('.ctx-menu input[name=cwd]', 'pcb');
  await settle();
  expect($('.btn-chip-folder')!.textContent).toBe('Folder pcb');
  // T38: Auto from its chip, confirmed (the globals answer every confirm with yes).
  $<HTMLButtonElement>('.btn-chip-permissions')!.click();
  await until('permissions menu', () => $('.ctx-menu .btn-permissions-auto'));
  $<HTMLButtonElement>('.ctx-menu .btn-permissions-auto')!.click();
  await settle();
  expect($('.btn-chip-permissions')!.textContent).toBe('Permissions: Auto');
  type('.chat-new .composer-input', 'Show the markdown sample');
  await settle();
  submit('.chat-new form.composer');
  await until('chat', () => location.hash.startsWith('#/s/') && $('.chat:not(.chat-new) textarea'));
  const sid = location.hash.split('/')[2];
  const s = await app.api.session(sid);
  expect(s).toMatchObject({ project_id: pid, workspace_id: app.workspaces.get(pid)![0].id, cwd: 'pcb' });
  expect(s).toMatchObject({ permission_mode: 'auto', effective_permission_mode: 'auto' });
  expect($('.chat .perm-auto')!.textContent).toBe('Auto');
  await until('the first message in its feed', () => $('.chat .feed')?.textContent?.includes('Show the markdown sample'));
  await until('its turn ended', () => app.sessions.get(sid)?.state === 'idle');
  await until('listed in the project', () => $$('.session-row').find((r) => r.getAttribute('href') === `#/s/${sid}`));
  // The personal project's sessions are not shown here.
  expect($$('.session-row').some((r) => r.textContent?.includes('Empty session'))).toBe(false);

  // Removing its workspace says the session is archived with it; Cancel keeps both.
  location.hash = `#/p/${pid}/settings`;
  (await until('remove button', () => $('.btn-remove-workspace') as HTMLButtonElement | null))!.click();
  const sessions = await until('sessions listed', () => $('.delete-folder .delete-sessions'));
  expect(sessions.textContent).toContain('1 session works in this folder, removing it archives it');
  expect($$('.delete-sessions li').map((l) => l.textContent)).toEqual([s.title]);
  expect($('.delete-what')!.textContent).toBe('Removing it deletes only the record: the folder on the worker stays.');
  expect(button('Delete folder', '.delete-folder')).toBeUndefined();
  button('Cancel', '.delete-folder')!.click();
  await until('closed', () => !$('.delete-folder'));
  expect((await app.api.session(sid)).archived).toBe(false);
});

it('switches projects from the left column and deletes a project', async () => {
  const personal = projectRow('Personal')!;
  location.hash = personal.getAttribute('href')!;
  await until('personal sessions', () => $$('.session-row').find((r) => r.textContent?.includes('Empty session')));
  // The personal project has no delete button.
  location.hash = `#/p/${app.me!.personal_project_id}/settings`;
  await until('personal workspaces', () => $$('.workspace-row').length === 1);
  expect($('.btn-delete-project')).toBeNull();
  // Its name stays: the dialog has no name field.
  button('Change icon and color')!.click();
  await until('dialog', () => $('[role=dialog][aria-label="Project appearance"]'));
  expect($('input[name=project-name]')).toBeNull();
  button('Cancel', '[role=dialog]')!.click();

  // A project is renamed in the appearance dialog.
  const kicad = app.sortedProjects().find((p) => p.name === 'KiCad')!;
  location.hash = `#/p/${kicad.id}/settings`;
  (await until('edit button', () => button('Change name, icon and color')))!.click();
  expect((await until('name field', () => $<HTMLInputElement>('input[name=project-name]'))).value).toBe('KiCad');
  type('input[name=project-name]', 'KiCad boards');
  await until('preview', () => $('.preview-name')?.textContent === 'KiCad boards');
  button('Save', '[role=dialog]')!.click();
  await until('renamed', () => projectRow('KiCad boards'));
  expect((await app.api.projects()).find((p) => p.id === kicad.id)!.slug).toBe('kicad');
  $<HTMLButtonElement>('.btn-delete-project')!.click();
  await until('gone', () => !projectRow('KiCad'));
  expect((await app.api.projects()).some((p) => p.id === kicad.id)).toBe(false);
});

it(`lists the worker's service session apart: no input, request cards, "requests only" (T17)`, async () => {
  const personal = projectRow('Personal')!;
  location.hash = personal.getAttribute('href')!;
  // The workers are one line at the bottom of the column; their list opens above it.
  (await until('workers line', () => $<HTMLButtonElement>('.wline')))!.click();
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
  // Last driven by VS Code, nobody holds it now: the strip instead of the composer, with
  // "Take over here"; where, the header's pill.
  const banner = await until('locked', () => $('.lockbar'));
  expect(banner.classList.contains('lockbar-held')).toBe(false);
  expect(banner.querySelector('.lockbar-text')!.textContent).toBe('VS Code was the last to run this session.');
  expect(banner.textContent).not.toContain('read-only');
  expect($('.chat .state')!.textContent).toBe('In VS Code on home-mac');
  expect($('.chat textarea')).toBeNull();

  button('Take over here')!.click();
  await until('taken over', () => !$('.lockbar'));
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
  const banner = await until('open banner', () => $('.lockbar.lockbar-held'));
  expect(banner.textContent).toBe('Close the session in VS Code to write from here.');
  expect($('.chat .state')!.textContent).toBe('In VS Code on home-mac');
  expect(button('Take over here')).toBeUndefined();
  expect($('.chat textarea')).toBeNull();
  const fork = await until('fork line', () => $('.line-system-session_forked'));
  expect(fork.querySelector('.warn-icon')).not.toBeNull();
  expect(fork.textContent).toContain('is not in its context');

  // The client has not heard of the holder yet (its summaries do not mention it): "Take over"
  // is offered and the hub refuses it with its text.
  mock.hideHolder(sid, true);
  await until('plain banner', () => $('.lockbar') && !$('.lockbar-held'));
  button('Take over here')!.click();
  const notice = await until('notice', () => $('.notice'));
  expect(notice.textContent).toBe('the session is open in VS Code: close it there and take it over again');
  mock.hideHolder(sid, false);
  await until('open banner again', () => $('.lockbar.lockbar-held'));

  mock.closeVsCode(sid);
  await until('free', () => !$('.lockbar'));
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
  // From the row's menu (a right click): its Archive.
  const archiveRow = async () => {
    item()!.dispatchEvent(new win.MouseEvent('contextmenu', { bubbles: true, cancelable: true }) as unknown as Event);
    (await until('row menu', () => $<HTMLButtonElement>('.ctx-menu .btn-archive')))!.click();
  };

  await archiveRow();
  await until('hidden with Undo', () => !item() && $('.toast-archive'));
  button('Undo')!.click();
  await until('back', () => item() && !$('.toast-archive'));
  expect(app.sessions.get(sid)!.archived).toBe(false);

  await archiveRow();
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

  await archiveRow();
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

it('clones a repository into a folder of the tree, follows it on the row and deletes it with its folder (T45)', async () => {
  promptAnswer = 'Clones';
  $<HTMLButtonElement>('button[aria-label="New project"]')!.click();
  await until('new project settings', () => settingsOf('Clones'));
  await until('no workspaces yet', () => $('.workspaces .empty-inline'));

  button('Clone a repository')!.click();
  await until('clone form', () => $('.clone-repository input[name=url]'));
  type('.clone-repository input[name=url]', 'git@github.com:drzlo/hold-c6.git');
  await settle();
  expect($<HTMLInputElement>('.clone-repository input[name=name]')!.value).toBe('hold-c6');
  const entry = (name: string) => $$('.clone-repository .dir-entry').find((b) => b.textContent?.includes(name));
  (await until('roots', () => entry('work')))!.click();
  (await until('work/', () => entry('kicad')))!.click();
  await until('kicad/', () => entry('power-board'));
  button('Pick this folder', '.clone-repository')!.click();
  await until('parent picked', () => $<HTMLInputElement>('.clone-repository input[name=parent]')?.value === '/Users/home-mac/work/kicad');
  expect($('.clone-target')!.textContent).toBe('Clones into /Users/home-mac/work/kicad/hold-c6');
  await settle();
  submit('.clone-repository');

  // The row shows the clone while the worker runs it, without actions, then the workspace.
  const row = (end: string) => $$('.workspace-row').find((r) => r.querySelector('.workspace-path')?.textContent?.endsWith(end));
  await until('cloning on the row', () => row('/kicad/hold-c6')?.querySelector('.workspace-progress'));
  expect($('.clone-repository')).toBeNull();
  expect(row('/kicad/hold-c6')!.querySelector('.btn-remove-workspace')).toBeNull();
  mock.finishCreating();
  await until('ready', () => !row('/kicad/hold-c6')!.querySelector('.workspace-progress') && row('/kicad/hold-c6')!.querySelector('.btn-rules'));

  // A folder that exists already is refused in the form.
  button('Clone a repository')!.click();
  await until('clone form', () => $('.clone-repository input[name=url]'));
  type('.clone-repository input[name=url]', 'https://github.com/drzlo/kicad.git');
  type('.clone-repository input[name=parent]', '/Users/home-mac/work');
  await settle();
  submit('.clone-repository');
  await until('exists', () => $('.clone-repository .error-text')?.textContent?.includes('already exists'));

  // A clone that fails: the reason on the row, and Remove takes the record away.
  type('.clone-repository input[name=url]', 'https://github.com/drzlo/missing-repo.git');
  await settle();
  submit('.clone-repository');
  await until('failed on the row', () => row('/work/missing-repo')?.querySelector('.workspace-error')?.textContent?.includes('repository not found'));
  row('/work/missing-repo')!.querySelector<HTMLButtonElement>('.btn-remove-workspace')!.click();
  await until('failed one removed', () => !row('/work/missing-repo'));

  // Deleting the clone: the dialog shows what the folder would lose, then the worker deletes it.
  row('/kicad/hold-c6')!.querySelector<HTMLButtonElement>('.btn-remove-workspace')!.click();
  await until('dialog checked', () => $('.delete-folder .delete-dirty'));
  expect($('.delete-what')!.textContent).toBe('A clone that hqc made.');
  expect($('.delete-dirty')!.textContent).toContain('Uncommitted changes (2), Delete folder discards them');
  expect($$('.delete-dirty li').map((l) => l.textContent)).toEqual(['src/panel.rs', 'notes.txt']);
  expect($('.delete-unpushed')!.textContent).toBe('3 commits not pushed on hqc/fix-backlight. Delete folder deletes them with the folder.');
  button('Delete folder', '.delete-folder')!.click();
  await until('deleted', () => !row('/kicad/hold-c6') && !$('.delete-folder'));
  const gone = mock.removedWorkspaces.find((w) => w.path.endsWith('/kicad/hold-c6'));
  expect(gone?.folder_removed).toBe(true);
  expect(mock.removedWorkspaces.find((w) => w.path.endsWith('/missing-repo'))?.folder_removed).toBeUndefined();
});

it('makes a worktree of a workspace, shows it under its origin and deletes it through the dialog (T46)', async () => {
  promptAnswer = 'Worktrees';
  $<HTMLButtonElement>('button[aria-label="New project"]')!.click();
  await until('new project settings', () => settingsOf('Worktrees'));
  await until('no workspaces yet', () => $('.workspaces .empty-inline'));
  const row = (end: string) => $$('.workspace-row').find((r) => r.querySelector('.workspace-path')?.textContent?.endsWith(end));
  // The origin: a clone, ready.
  button('Clone a repository')!.click();
  await until('clone form', () => $('.clone-repository input[name=url]'));
  type('.clone-repository input[name=url]', 'git@github.com:drzlo/hold-wt.git');
  type('.clone-repository input[name=parent]', '/Users/home-mac/work');
  await settle();
  submit('.clone-repository');
  await until('origin cloning', () => row('/work/hold-wt'));
  expect(row('/work/hold-wt')!.querySelector('.btn-new-worktree')).toBeNull();
  mock.finishCreating();
  await until('origin ready', () => row('/work/hold-wt')?.querySelector('.btn-new-worktree'));

  // A new branch: the folder follows the branch until it is edited.
  row('/work/hold-wt')!.querySelector<HTMLButtonElement>('.btn-new-worktree')!.click();
  await until('worktree form', () => $('.new-worktree input[name=branch]'));
  type('.new-worktree input[name=branch]', 'hqc/hold-fix');
  await settle();
  expect($<HTMLInputElement>('.new-worktree input[name=path]')!.value).toBe('/Users/home-mac/work/hold-wt-hqc-hold-fix');
  submit('.new-worktree');
  await until('worktree adding', () => row('/work/hold-wt-hqc-hold-fix')?.querySelector('.workspace-progress'));
  expect($('.new-worktree')).toBeNull();
  const wt = row('/work/hold-wt-hqc-hold-fix')!;
  expect(wt.classList.contains('is-worktree')).toBe(true);
  expect(wt.querySelector('.workspace-progress')!.textContent).toBe('Adding worktree…');
  expect(wt.querySelector('.workspace-branch')!.textContent).toBe('hqc/hold-fix');
  // Under its origin, before the rows that came after the origin.
  const paths = () => $$('.workspace-row .workspace-path').map((p) => p.textContent);
  mock.finishCreating();
  await until('worktree ready', () => row('/work/hold-wt-hqc-hold-fix')?.querySelector('.btn-rules'));
  expect(row('/work/hold-wt-hqc-hold-fix')!.querySelector('.btn-new-worktree')).toBeNull();

  // An existing branch with an edited folder; another one checked out elsewhere fails.
  row('/work/hold-wt')!.querySelector<HTMLButtonElement>('.btn-new-worktree')!.click();
  await until('worktree form', () => $('.new-worktree input[name=branch]'));
  type('.new-worktree input[name=branch]', 'dirty');
  $<HTMLInputElement>('.new-worktree input[value=existing]')!.click();
  type('.new-worktree input[name=path]', '/Users/home-mac/work/wt-dirty');
  await settle();
  submit('.new-worktree');
  await until('edited folder', () => row('/work/wt-dirty')?.querySelector('.btn-rules'));
  row('/work/hold-wt')!.querySelector<HTMLButtonElement>('.btn-new-worktree')!.click();
  await until('worktree form', () => $('.new-worktree input[name=branch]'));
  type('.new-worktree input[name=branch]', 'taken');
  $<HTMLInputElement>('.new-worktree input[value=existing]')!.click();
  await settle();
  submit('.new-worktree');
  await until('failed on the row', () => row('/work/hold-wt-taken')?.querySelector('.workspace-error')?.textContent?.includes('already checked out'));
  expect(row('/work/hold-wt-taken')!.querySelector('.tag-failed')!.textContent).toBe('Worktree failed');
  expect(paths().slice(0, 4).map((p) => p?.split('/').pop())).toEqual(['hold-wt', 'hold-wt-hqc-hold-fix', 'wt-dirty', 'hold-wt-taken']);
  row('/work/hold-wt-taken')!.querySelector<HTMLButtonElement>('.btn-remove-workspace')!.click();
  await until('failed one removed', () => !row('/work/hold-wt-taken'));

  // The origin stays while its worktrees exist: the dialog lists them, nothing to press.
  row('/work/hold-wt')!.querySelector<HTMLButtonElement>('.btn-remove-workspace')!.click();
  const wts = await until('origin dialog', () => $('.delete-folder .delete-worktrees'));
  expect(wts.textContent).toContain('2 worktrees of this folder, delete them first');
  expect(button('Remove from project only', '.delete-folder')!.disabled).toBe(true);
  expect(button('Delete folder', '.delete-folder')!.disabled).toBe(true);
  $<HTMLButtonElement>('.delete-folder button[aria-label=Close]')!.click();
  await until('closed', () => !$('.delete-folder'));

  // Deleting a worktree: its own branch with the unpushed commit, which stays; no force
  // without uncommitted files.
  row('/work/hold-wt-hqc-hold-fix')!.querySelector<HTMLButtonElement>('.btn-remove-workspace')!.click();
  await until('dialog checked', () => $('.delete-folder .delete-unpushed'));
  expect($('.delete-what')!.textContent).toMatch(/^A worktree of .+ that hqc made\.$/);
  expect($('.delete-unpushed')!.textContent).toMatch(/^1 commit not pushed on hqc\/hold-fix\. Delete folder keeps it: the branch stays in .+, only the folder is removed\.$/);
  expect($('.delete-dirty')).toBeNull();
  button('Delete folder', '.delete-folder')!.click();
  await until('deleted', () => !row('/work/hold-wt-hqc-hold-fix') && !$('.delete-folder'));
  const fix = mock.removedWorkspaces.find((w) => w.path.endsWith('/hold-wt-hqc-hold-fix')) as { folder_removed?: boolean; forced?: boolean };
  expect(fix).toMatchObject({ folder_removed: true });
  expect(fix.forced).toBeUndefined();
  // Uncommitted files shown in the dialog: deleting them is confirming them (force).
  row('/work/wt-dirty')!.querySelector<HTMLButtonElement>('.btn-remove-workspace')!.click();
  await until('dirty dialog', () => $('.delete-folder .delete-dirty'));
  button('Delete folder', '.delete-folder')!.click();
  await until('dirty deleted', () => !row('/work/wt-dirty') && !$('.delete-folder'));
  expect(mock.removedWorkspaces.find((w) => w.path.endsWith('/wt-dirty'))).toMatchObject({ folder_removed: true, forced: true });
});

it('shows a spawned session’s worktree for deletion once its sessions are archived (T47)', async () => {
  const pid = app.currentProjectId!;
  const row = (end: string) => $$('.workspace-row').find((r) => r.querySelector('.workspace-path')?.textContent?.endsWith(end));
  const origin = app.workspaces.get(pid)!.find((w) => w.path.endsWith('/work/hold-wt'))!;
  // What hqc_spawn with a worktree leaves: the worktree under its origin, and its session.
  const sid = mock.spawnWorktree(origin.id, 'Session 5');
  await app.loadWorkspaces(pid);
  const end = '/work/hold-wt-hqc-session-5';
  await until('spawned worktree', () => row(end));
  expect(row(end)!.querySelector('.workspace-ephemeral')!.textContent).toBe('spawned');
  expect(row(end)!.querySelector('.workspace-branch')!.textContent).toBe('hqc/session-5');
  expect(row(end)!.querySelector('.workspace-marked')).toBeNull();
  expect(row(end)!.querySelector('.btn-remove-workspace')!.textContent).toBe('Remove');

  // Its last session archived: marked, with the deletion at hand; unarchived: not any more.
  await app.api.archive(sid);
  await app.loadWorkspaces(pid);
  await until('marked', () => row(end)?.querySelector('.workspace-marked'));
  expect(row(end)!.querySelector('.workspace-marked')!.textContent).toBe('for deletion');
  expect(row(end)!.querySelector('.workspace-note')!.textContent).toContain('Its sessions are archived');
  expect(row(end)!.querySelector('.btn-remove-workspace')!.textContent).toBe('Delete');
  await app.api.unarchive(sid);
  await app.loadWorkspaces(pid);
  await until('unmarked', () => row(end) && !row(end)!.querySelector('.workspace-marked'));
  await app.api.archive(sid);
  await app.loadWorkspaces(pid);
  await until('marked again', () => row(end)?.querySelector('.workspace-marked'));

  // Deleting it is the worktree's dialog; nothing went without it.
  expect(mock.removedWorkspaces.find((w) => w.path.endsWith(end))).toBeUndefined();
  row(end)!.querySelector<HTMLButtonElement>('.btn-remove-workspace')!.click();
  await until('dialog checked', () => $('.delete-folder .delete-unpushed'));
  expect($('.delete-what')!.textContent).toContain('for a session an agent started');
  expect($('.delete-unpushed')!.textContent).toContain('1 commit not pushed on hqc/session-5.');
  button('Delete folder', '.delete-folder')!.click();
  await until('deleted', () => !row(end) && !$('.delete-folder'));
  expect(mock.removedWorkspaces.find((w) => w.path.endsWith(end))).toMatchObject({ folder_removed: true });
});

it('opens the session last open in a project', async () => {
  const personal = app.me!.personal_project_id;
  const s = app.sessions.get([...app.sessions.values()].find((x) => x.project_id === personal && !x.archived && !x.system)!.id)!;
  location.hash = `#/s/${s.id}`;
  await until('chat', () => $('.chat'));
  const other = app.sortedProjects().find((p) => p.id !== personal)!;
  location.hash = `#/p/${other.id}`;
  // It opens its own last session, if it has one.
  await until('other project', () => app.currentProject()?.id === other.id);
  location.hash = `#/p/${personal}`;
  await until('reopened', () => location.hash === `#/s/${s.id}`);
});

it('orders the project bar by name or by hand, and keeps the order in the hub', async () => {
  const names = () => $$('.rail .project-row').map((r) => r.getAttribute('title'));
  const before = names();
  expect(before.length).toBeGreaterThan(1);
  /** An item of the bar's order menu, opening the menu first. */
  const pick = async (cls: string) => {
    $<HTMLButtonElement>('.rail-order')!.click();
    (await until('order menu', () => $<HTMLButtonElement>(`.menu ${cls}`)))!.click();
  };
  // Name: kept on the user in the hub.
  await pick('.btn-sort-name');
  await until('by name', () => app.projectSort() === 'name');
  expect((await app.api.me()).project_sort).toBe('name');
  expect(names()).toEqual(app.sortedProjects('name').map((p) => p.name));

  await pick('.btn-reorder-projects');
  const last = await until('reorder mode', () => $$('.rail .rail-drag').at(-1));
  // Reorder arranges the user's own order, not the names'.
  expect($$('.rail .rail-drag').map((r) => r.getAttribute('title'))).toEqual(before);
  expect($('.rail a.project-row')).toBeNull();
  last.dispatchEvent(new win.KeyboardEvent('keydown', { key: 'ArrowUp', bubbles: true }) as unknown as Event);
  const moved = [...before];
  moved.splice(-2, 2, before.at(-1)!, before.at(-2)!);
  await until('moved', () => names().join() === moved.join());
  // Cancel drops it, and the bar stays by name.
  $<HTMLButtonElement>('.rail button[aria-label=Cancel]')!.click();
  await until('cancelled', () => $('.rail a.project-row'));
  expect(app.projectSort()).toBe('name');
  expect(names()).toEqual(app.sortedProjects('name').map((p) => p.name));

  await pick('.btn-reorder-projects');
  const first = await until('reorder again', () => $$('.rail .rail-drag').at(-1));
  first.dispatchEvent(new win.KeyboardEvent('keydown', { key: 'ArrowUp', bubbles: true }) as unknown as Event);
  await until('moved again', () => names().join() === moved.join());
  // Done keeps the order and makes the bar Custom.
  $<HTMLButtonElement>('.rail-done')!.click();
  await until('saved', () => $('.rail a.project-row') && app.projectSort() === 'custom' && names().join() === moved.join());
  expect((await app.api.projects()).map((p) => p.name)).toEqual(moved);
  expect((await app.api.me()).project_sort).toBe('custom');
});

it('makes a missing folder for a workspace when asked', async () => {
  const pid = app.me!.personal_project_id;
  location.hash = `#/p/${pid}/settings`;
  (await until('add button', () => button('Add a workspace')))!.click();
  await until('add form', () => $('.add-workspace input[name=path]'));
  type('.add-workspace input[name=path]', '/Users/home-mac/work/notes/new');
  await until('add enabled', () => !button('Add', '.add-workspace')?.disabled);
  submit('.add-workspace');
  await until('missing', () => $('.add-workspace .error-text')?.textContent?.includes('does not exist on the worker'));
  // Editing the path takes the offer back.
  type('.add-workspace input[name=path]', '/Users/home-mac/work/notes/new2');
  await until('offer gone', () => !$('.btn-create-folder'));
  submit('.add-workspace');
  (await until('offer', () => $<HTMLButtonElement>('.btn-create-folder')))!.click();
  await until('workspace listed', () => $$('.workspace-row').find((r) => r.textContent?.includes('/notes/new2')));
  expect($('.add-workspace')).toBeNull();
});

it('moves a session from its chat into a new project, and into a folder of another project', async () => {
  const personal = app.me!.personal_project_id;
  const s = await app.api.createSession(personal, 'Move me');
  await until('known', () => app.sessions.get(s.id));
  location.hash = `#/s/${s.id}`;
  await until('chat', () => $('.chat'));
  (await menuItem('.btn-move'))!.click();
  await until('move form', () => $('.move-session input[name=path]'));
  type('.move-session input[name=path]', '/Users/home-mac/work/moved');
  await settle();
  submit('.move-session');
  await until('missing', () => $('.move-session .error-text')?.textContent?.includes('does not exist on the worker'));
  $<HTMLButtonElement>('.move-session .btn-create-folder')!.click();
  // A new project named after the folder joins the rail; the session opens there.
  await until('new project', () => projectRow('moved'));
  await until('gone from the form', () => !$('.move-session'));
  const moved = app.sessions.get(s.id)!;
  expect(moved.project_id).not.toBe(personal);
  expect(app.projects.get(moved.project_id)?.name).toBe('moved');
  await until('current', () => app.currentProject()?.id === moved.project_id);
  await until('its row', () => $$('.session-row').find((r) => r.textContent?.includes('Move me')));
  await until('the feed says so', () => root.textContent?.includes('The session moved from'));

  // Back into a folder of the personal project, picked from its list.
  (await menuItem('.btn-move'))!.click();
  await until('move form', () => $('.move-session select[name=project]'));
  choose('.move-session select[name=project]', personal);
  const hqc = await until('its folders', () =>
    $$('.move-session .move-folders button').find((b) => b.textContent?.trim().endsWith('/work/hqc')),
  );
  hqc.click();
  await settle();
  expect($<HTMLInputElement>('.move-session input[name=path]')!.value).toBe('/Users/home-mac/work/hqc');
  submit('.move-session');
  await until('back', () => app.sessions.get(s.id)?.project_id === personal);
  await until('personal current', () => app.currentProject()?.id === personal);
});

it('takes a session moved elsewhere out of its list and puts its new project on the rail', async () => {
  const personal = app.me!.personal_project_id;
  const s = await app.api.createSession(personal, 'Moved elsewhere');
  location.hash = `#/p/${personal}`;
  await until('its row', () => $$('.session-row').find((r) => r.textContent?.includes('Moved elsewhere')));
  // As the agent's tool or another tab would: past this client's state.
  await app.api.moveSession(s.id, { path: '/Users/home-mac/work/elsewhere', create: true });
  await until('new project', () => projectRow('elsewhere'));
  await until('row gone', () => !$$('.session-row').find((r) => r.textContent?.includes('Moved elsewhere')));
  expect(app.projects.get(app.sessions.get(s.id)!.project_id)?.name).toBe('elsewhere');
});
