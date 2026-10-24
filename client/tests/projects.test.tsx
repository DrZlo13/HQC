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

/** An item of the chat's «Ещё» menu, opening the menu first. */
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
  await until('personal project', () => projectRow('Личный'));
  expect($('.project-row.is-current')!.getAttribute('aria-label')).toBe('Личный');
  expect($('.project-row.is-current')!.textContent).toBe('Л');
  await until('its sessions', () => $$('.session-row').find((r) => r.textContent?.includes('Пустая сессия')));
});

it('creates a project, adds a workspace from the folder tree and starts a session there', async () => {
  promptAnswer = 'KiCad';
  $<HTMLButtonElement>('button[aria-label="Новый проект"]')!.click();
  await until('workspaces screen', () => $('.workspaces'));
  expect(location.hash).toMatch(/^#\/p\/[^/]+\/workspaces$/);
  const pid = location.hash.split('/')[2];
  await until('current project', () => $('.project-row.is-current')?.getAttribute('aria-label') === 'KiCad');
  expect($('.project-row.is-current')!.textContent).toBe('KI');
  expect($('.side-head .project-title')!.textContent).toBe('KiCad');
  expect($$('.session-row')).toHaveLength(0);
  await until('no workspaces yet', () => $('.workspaces .empty-inline'));

  // Folder tree of the online worker: roots, then down to kicad/power-board.
  button('Добавить workspace')!.click();
  const entry = (name: string) => $$('.dir-entry').find((b) => b.textContent?.includes(name));
  (await until('roots', () => entry('work')))!.click();
  (await until('work/', () => entry('kicad')))!.click();
  (await until('kicad/', () => entry('power-board')))!.click();
  await until('power-board/', () => $('.dir-browser')?.textContent?.includes('Подпапок нет'));
  button('Выбрать эту папку')!.click();
  await until('path picked', () => $<HTMLInputElement>('input[name=path]')?.value.endsWith('/kicad/power-board'));
  button('↑ Выше')!.click();
  await until('back up', () => entry('power-board'));
  await settle();
  submit('.add-workspace');
  await until('workspace listed', () => $$('.workspace-row').find((r) => r.textContent?.includes('/kicad/power-board')));
  expect($('.workspace-row .tag')!.textContent).toBe('по умолчанию');
  expect(app.projects.get(pid)!.default_workspace_id).toBe(app.workspaces.get(pid)![0].id);

  // A folder that is already in the personal project: the message names it, with a link.
  button('Добавить workspace')!.click();
  await until('add form', () => $('.add-workspace input[name=path]'));
  type('.add-workspace input[name=path]', '/Users/home-mac/work/hqc/crates');
  await until('add enabled', () => !button('Добавить', '.add-workspace')?.disabled);
  submit('.add-workspace');
  const err = await until('conflict', () => $('.add-workspace .error-text'));
  expect(err.textContent).toContain('Эта папка уже в проекте «Личный»');
  expect(err.querySelector('a')!.getAttribute('href')).toBe(`#/p/${app.me!.personal_project_id}/workspaces`);
  button('Отмена', '.add-workspace')!.click();

  // A session in that workspace, in a subfolder.
  location.hash = `#/p/${pid}`;
  await until('project route', () => !$('.workspaces'));
  button('Новая')!.click();
  await until('new session form', () => $('.new-session select[name=workspace]'));
  await until('workspaces loaded', () => $$('.new-session option').length === 1);
  expect($('.new-session option')!.textContent).toContain('(по умолчанию)');
  choose('.new-session select[name=workspace]', app.workspaces.get(pid)![0].id);
  type('.new-session input[name=cwd]', 'pcb');
  await settle();
  submit('.new-session');
  await until('chat', () => $('.chat textarea'));
  const sid = location.hash.split('/')[2];
  const s = await app.api.session(sid);
  expect(s).toMatchObject({ project_id: pid, workspace_id: app.workspaces.get(pid)![0].id, cwd: 'pcb' });
  await until('listed in the project', () => $$('.session-row').find((r) => r.getAttribute('href') === `#/s/${sid}`));
  // The personal project's sessions are not shown here.
  expect($$('.session-row').some((r) => r.textContent?.includes('Пустая сессия'))).toBe(false);

  // Its workspace stays while the session lives.
  location.hash = `#/p/${pid}/workspaces`;
  (await until('remove button', () => $('.btn-remove-workspace') as HTMLButtonElement | null))!.click();
  await until('in use', () => $('.workspaces .error-text')?.textContent?.includes('архивируйте'));
});

it('switches projects from the left column and deletes a project', async () => {
  const personal = projectRow('Личный')!;
  location.hash = personal.getAttribute('href')!;
  await until('personal sessions', () => $$('.session-row').find((r) => r.textContent?.includes('Пустая сессия')));
  // The personal project has no delete button.
  location.hash = `#/p/${app.me!.personal_project_id}/workspaces`;
  await until('personal workspaces', () => $$('.workspace-row').length === 1);
  expect($('.btn-delete-project')).toBeNull();

  expect($('.btn-rename-project')).toBeNull();

  const kicad = app.sortedProjects().find((p) => p.name === 'KiCad')!;
  location.hash = `#/p/${kicad.id}/workspaces`;
  promptAnswer = 'KiCad boards';
  (await until('rename button', () => $('.btn-rename-project') as HTMLButtonElement | null))!.click();
  await until('renamed', () => projectRow('KiCad boards'));
  expect((await app.api.projects()).find((p) => p.id === kicad.id)!.slug).toBe('kicad');
  $<HTMLButtonElement>('.btn-delete-project')!.click();
  await until('gone', () => !projectRow('KiCad'));
  expect((await app.api.projects()).some((p) => p.id === kicad.id)).toBe(false);
});

it("lists the worker's service session apart: no input, request cards, «только запросы» (T17)", async () => {
  const personal = projectRow('Личный')!;
  location.hash = personal.getAttribute('href')!;
  const row = await until('workers group', () =>
    $$('.worker-list .worker-row').find((r) => r.textContent?.includes('home-mac')),
  );
  expect($('.worker-list .group-title')!.textContent).toBe('Воркеры');
  expect(row.textContent).toContain('ждёт ответа');
  // Not among the sessions of the personal project, where it lives.
  await until('personal sessions', () => $$('.session-row').find((r) => r.textContent?.includes('Пустая сессия')));
  expect($$('.session-row').some((r) => r.getAttribute('href') === row.getAttribute('href'))).toBe(false);

  location.hash = row.getAttribute('href')!;
  await until('log lines', () => $$('.line-worker-log').length === 3);
  expect($('.chat textarea')).toBeNull();
  expect($('.chat .chat-system-note')!.textContent).toBe('служебная сессия воркера');
  expect($('.line-worker-log.level-error .log-text')!.textContent).toContain('known folders not saved');
  button('Только запросы')!.click();
  await until('filtered', () => $$('.line-worker-log').length === 0 && $('.chat .perm'));
  expect($('.perm-title')!.textContent).toBe('Воркер спрашивает разрешения');
  $<HTMLButtonElement>('.perm button[data-canonical="once"]')!.click();
  await until('answered', () => $('.perm-done-row .outcome')?.textContent?.includes('разрешено'));
  button('Только запросы')!.click();
  await until('unfiltered', () => $$('.line-worker-log').length === 3);
});

// T15 (SPEC §9): import a folder with Claude Code sessions, see the session read-only as
// «ведётся в VS Code», take it over and write.
it('imports from Claude Code and takes a session over', async () => {
  promptAnswer = 'Платы';
  $<HTMLButtonElement>('button[aria-label="Новый проект"]')!.click();
  await until('workspaces screen', () => $('.workspaces'));
  const pid = location.hash.split('/')[2];
  button('Импорт из Claude Code')!.click();
  const row = await until('candidates', () =>
    $$('.candidate').find((r) => r.textContent?.includes('/kicad/power-board')),
  );
  expect(row.textContent).toContain('сессий: 1');
  expect($$('.candidate')).toHaveLength(2);
  expect(button('Импортировать')!.disabled).toBe(true);
  row.querySelector<HTMLInputElement>('input[type=checkbox]')!.click();
  await until('import enabled', () => !button('Импортировать')?.disabled);
  button('Импортировать')!.click();
  const ws = await until('workspace listed', () =>
    $$('.workspace-row').find((r) => r.textContent?.includes('/kicad/power-board')),
  );
  const toggle = ws.querySelector<HTMLInputElement>('.mirror-toggle input')!;
  expect(ws.querySelector('.mirror-toggle')!.textContent).toBe('Сессии VS Code');
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
    $$('.session-row').find((r) => r.textContent?.includes('Из VS Code: power-board')),
  );
  location.hash = link.getAttribute('href')!;
  await until('history', () => $('.chat')?.textContent?.includes('VSCODE-DONE'));
  // Last driven by VS Code, nobody holds it now: the banner instead of the composer, with
  // «Забрать сюда».
  const banner = await until('locked', () => $('.handover'));
  expect(banner.classList.contains('handover-held')).toBe(false);
  expect(banner.querySelector('.handover-title')!.textContent).toBe('Последней сессию вела VS Code на home-mac.');
  expect(banner.textContent).not.toContain('только для чтения');
  expect($('.chat .state')!.textContent).toBe('Ведётся в VS Code');
  expect($('.chat textarea')).toBeNull();

  button('Забрать сюда')!.click();
  await until('taken over', () => !$('.handover'));
  await until('system line', () => $('.chat')?.textContent?.includes('Подхвачена из VS Code'));
  expect($<HTMLTextAreaElement>('.chat textarea')!.disabled).toBe(false);
});

// T20 (SPEC §9.3): the import brought a session that is open in VS Code right now and whose
// transcript forked. No «Подхватить» until it is closed there; a takeover the hub refuses
// (`409 vscode_open`) shows its text; closed in VS Code, it is free at once.
it('does not take over a session open in VS Code', async () => {
  const link = await until('open session', () =>
    $$('.session-row').find((r) => r.textContent?.includes('Открыта в VS Code: power-board')),
  );
  location.hash = link.getAttribute('href')!;
  const sid = location.hash.split('/').pop()!;
  // Open in VS Code right now: no button, close it there first.
  const banner = await until('open banner', () => $('.handover.handover-held'));
  expect(banner.querySelector('.handover-title')!.textContent).toBe('Сессия открыта в VS Code на home-mac.');
  expect(banner.querySelector('.handover-sub')!.textContent).toBe('Закройте её там, чтобы писать отсюда.');
  expect(button('Забрать сюда')).toBeUndefined();
  expect($('.chat textarea')).toBeNull();
  const fork = await until('fork line', () => $('.line-system-session_forked'));
  expect(fork.querySelector('.warn-icon')).not.toBeNull();
  expect(fork.textContent).toContain('не попал в его контекст');

  // The client has not heard of the holder yet (its summaries do not mention it): «Подхватить»
  // is offered and the hub refuses it with its text.
  mock.hideHolder(sid, true);
  await until('plain banner', () => $('.handover') && !$('.handover-held'));
  button('Забрать сюда')!.click();
  const notice = await until('notice', () => $('.notice'));
  expect(notice.textContent).toBe('сессия открыта в VS Code: закройте её там и подхватите снова');
  mock.hideHolder(sid, false);
  await until('open banner again', () => $('.handover.handover-held'));

  mock.closeVsCode(sid);
  await until('free', () => !$('.handover'));
  expect($<HTMLTextAreaElement>('.chat textarea')!.disabled).toBe(false);
});

// Archive from the chat header, find the session under «Архив», bring it back, delete it.
it('archives, restores and deletes a session', async () => {
  const personal = projectRow('Личный')!;
  location.hash = personal.getAttribute('href')!;
  const row = await until('a session', () => $$('.session-row').find((r) => r.textContent?.includes('Пустая сессия')));
  const href = row.getAttribute('href')!;
  const sid = href.split('/').pop()!;
  const pid = app.sessions.get(sid)!.project_id;
  location.hash = href;
  await until('chat with the menu', () => $('.chat') && $('.btn-more'));
  // «В архив», «Вернуть», «Удалить» live in the «Ещё» menu (T26).
  expect(await menuItem('.btn-unarchive')).toBeNull();
  (await menuItem('.btn-archive'))!.click();
  await until('archived', () => $('.chat .state')?.textContent?.includes('В архиве'));
  await until('gone from the list', () => !$$('.session-row').some((r) => r.getAttribute('href') === href));
  expect($('.menu')).toBeNull();
  expect(await menuItem('.btn-archive')).toBeNull();

  $<HTMLButtonElement>('.btn-archive-toggle')!.click();
  await until('listed under Архив', () => $$('.session-row').some((r) => r.getAttribute('href') === href));

  (await menuItem('.btn-unarchive'))!.click();
  await until('restored', () => $('.chat .state')?.textContent === 'Свободна');
  $<HTMLButtonElement>('.btn-archive-toggle')!.click();
  await until('back in the list', () => $$('.session-row').some((r) => r.getAttribute('href') === href));

  (await menuItem('.btn-archive'))!.click();
  await until('archived again', () => $('.chat .state')?.textContent?.includes('В архиве'));
  (await menuItem('.btn-delete'))!.click(); // confirm() is stubbed to true
  await until('deleted', () => !app.sessions.has(sid));
  expect(location.hash).toBe(`#/p/${pid}`);
});
