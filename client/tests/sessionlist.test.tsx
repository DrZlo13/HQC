// @vitest-environment happy-dom
// T26: the session list — the "State" and "Recent" orders, the idle sessions by recency, the
// magnifier that opens the search page, the workers line with the running sessions of the open session's
// machine, the project rail with its "needs you" badge. T50: the order on the hub (State /
// Recent / Custom, Reorder by the keyboard), the delayed archive with Undo, archive mode. The
// one row menu (a right click; a long press on a phone), the row's archive and favorites
// buttons, the one-row header with the order and project menus, the pinned favorites, the
// column's width.

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { render } from 'preact';
import { act } from 'preact/test-utils';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { Api } from '../src/api';
import { ARCHIVE_UNDO_MS, AppState } from '../src/app';
import { parseEvent, type ListMode, type Project, type ProjectSort, type SessionInfo, type SessionState } from '../src/proto';
import { trackHistory } from '../src/nav';
import { applyLook } from '../src/theme';
import { Timeline } from '../src/timeline';
import type { ConnState, HubSocket } from '../src/ws';
import { App } from '../src/ui/App';
import { ChatItems, type ChatActions } from '../src/ui/cards';
import { Chat } from '../src/ui/Chat';
import { PhoneHome } from '../src/ui/PhoneHome';
import { ProjectRail, onMac, runSpeed, runningTitle } from '../src/ui/ProjectRail';
import { ProjectSettings } from '../src/ui/ProjectSettings';
import { LIST_WIDTH_MAX, LIST_WIDTH_MIN, LIST_WIDTH_STEP, SessionList, WidthGrip } from '../src/ui/SessionList';
import {
  arrange,
  bucketOf,
  groupSessions,
  matchesQuery,
  moveTo,
  projectInitials,
  recencyOf,
  runningLabel,
  subLabel,
} from '../src/ui/sessions';

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

let root: HTMLElement;
beforeEach(() => {
  vi.useFakeTimers({ toFake: ['Date'] });
  vi.setSystemTime(NOW);
});
afterEach(() => {
  if (root) render(null, root);
  document.body.innerHTML = '';
  // Leaving the page and refreshing keep the list there (`hqc.list`): the next app would load it.
  localStorage.clear();
  vi.useRealTimers();
});

describe('list logic', () => {
  it('buckets: a request or an error waits for you, a turn or a busy VS Code works', () => {
    expect(bucketOf(sess('a', 'a', 'waiting', at(1)))).toBe('wait');
    expect(bucketOf(sess('a', 'a', 'error', at(1)))).toBe('wait');
    expect(bucketOf(sess('a', 'a', 'queued', at(1)))).toBe('run');
    expect(bucketOf(sess('a', 'a', 'locked_vscode', at(1), { held_by: { entrypoint: 'claude-vscode', status: 'busy' } }))).toBe('run');
    expect(bucketOf(sess('a', 'a', 'locked_vscode', at(1)))).toBe('idle');
    expect(bucketOf(sess('a', 'a', 'worker_offline', at(1)))).toBe('idle');
    expect(bucketOf(sess('a', 'a', 'worker_offline', at(1), { subagents: 2 }))).toBe('idle');
  });

  it('an idle session whose background subagent still works is in Running and says so', () => {
    const busy = sess('bg', 'Background build', 'idle', at(1), { subagents: 1 });
    const free = sess('free', 'Nothing left', 'idle', at(2), { subagents: 0 });
    expect(groupSessions([busy, free], 'state', NOW).map((g) => [g.label, g.items.map((s) => s.id)])).toEqual([
      ['Running', ['bg']],
      ['Idle · Today', ['free']],
    ]);
    expect(subLabel(busy)).toBe('Running');
    expect(subLabel(free)).toBe('Idle');
  });

  it('an idle session whose background task still runs, with no subagent, is in Running and says so', () => {
    const busy = sess('make', 'Two conversion runs', 'idle', at(1), { background_tasks: 1 });
    const free = sess('free', 'Nothing left', 'idle', at(2));
    expect(groupSessions([busy, free], 'state', NOW).map((g) => [g.label, g.items.map((s) => s.id)])).toEqual([
      ['Running', ['make']],
      ['Idle · Today', ['free']],
    ]);
    expect(subLabel(busy)).toBe('Running');
  });

  it('groups by state and by recency, newest first, empty groups left out', () => {
    expect(groupSessions(LIST, 'state', NOW).map((g) => [g.label, g.items.map((s) => s.id)])).toEqual([
      ['Needs you', ['net']],
      ['Running', ['stones', 'det']],
      ['Idle · Yesterday', ['prof']],
      ['Idle · Last 7 days', ['cave']],
    ]);
    expect(groupSessions(LIST, 'recent', NOW).map((g) => [g.label, g.items.map((s) => s.id)])).toEqual([
      ['Today', ['stones', 'det', 'net']],
      ['Yesterday', ['prof']],
      ['Last 7 days', ['cave']],
    ]);
    expect(groupSessions(LIST.slice(3), 'state', NOW).map((g) => g.label)).toEqual(['Idle · Yesterday', 'Idle · Last 7 days']);
  });

  it('the idle sessions by recency: today, yesterday, the last 7 days, this month, earlier', () => {
    // NOW is Saturday 2026-10-24, 19:50.
    const idle = [
      sess('a', 'a', 'idle', at(1)),
      sess('b', 'b', 'idle', at(22)),
      sess('c', 'c', 'idle', at(24 * 6)),
      sess('d', 'd', 'idle', at(24 * 7)),
      sess('e', 'e', 'idle', new Date(2026, 9, 1, 0, 30).toISOString()),
      sess('f', 'f', 'idle', new Date(2026, 8, 30, 23, 30).toISOString()),
      sess('g', 'g', 'worker_offline', at(24 * 400)),
    ];
    expect(groupSessions(idle, 'state', NOW).map((g) => [g.id, g.label, g.items.map((s) => s.id)])).toEqual([
      ['idle-today', 'Idle · Today', ['a']],
      ['idle-yesterday', 'Idle · Yesterday', ['b']],
      ['idle-week', 'Idle · Last 7 days', ['c']],
      ['idle-month', 'Idle · This month', ['d', 'e']],
      ['idle-earlier', 'Idle · Earlier', ['f', 'g']],
    ]);
    // The same groups in Recent, for every session.
    expect(groupSessions(idle, 'recent', NOW).map((g) => g.label)).toEqual(['Today', 'Yesterday', 'Last 7 days', 'This month', 'Earlier']);
    // The last 7 days reach into the month before; a week back there is "Earlier".
    const nov3 = new Date(2026, 10, 3, 10, 0).getTime();
    expect(recencyOf(new Date(2026, 9, 29, 12, 0).toISOString(), nov3)).toBe('week');
    expect(recencyOf(new Date(2026, 9, 25, 12, 0).toISOString(), nov3)).toBe('earlier');
    expect(recencyOf('not a date', nov3)).toBe('earlier');
  });

  it('searches every word in the title or the path', () => {
    const s = LIST[2];
    expect(matchesQuery(s, '~/work/games/phys-mp/net', 'network')).toBe(true);
    expect(matchesQuery(s, '~/work/games/phys-mp/net', 'PHYS-MP/NET')).toBe(true);
    expect(matchesQuery(s, '~/work/games/phys-mp/net', 'sync net')).toBe(true);
    expect(matchesQuery(s, '~/work/games/phys-mp/net', 'sync render')).toBe(false);
    expect(matchesQuery(s, '', '  ')).toBe(true);
  });

  it('names: initials, counts', () => {
    expect(projectInitials('physics-mp')).toBe('PM');
    expect(projectInitials('HQC')).toBe('HQ');
    expect(projectInitials('Personal', true)).toBe('P');
    // Projects may be named in Russian.
    expect(projectInitials('Личный', true)).toBe('Л');
    expect(projectInitials('платы питания')).toBe('ПП');
    expect([0, 1, 2, 21].map(runningLabel)).toEqual(['none running', '1 running', '2 running', '21 running']);
  });

  it('custom order: the saved one, a session missing from it on top; moves clamp at the ends', () => {
    const ids = (l: SessionInfo[]) => l.map((s) => s.id);
    expect(ids(arrange(LIST, ['cave', 'net', 'gone', 'stones']))).toEqual(['det', 'prof', 'cave', 'net', 'stones']);
    expect(groupSessions(LIST, 'custom', NOW, ['cave']).map((g) => [g.label, g.plain, ids(g.items)])).toEqual([
      ['Custom order', true, ['stones', 'det', 'net', 'prof', 'cave']],
    ]);
    const o = ['a', 'b', 'c'];
    expect(moveTo(o, 'a', 1)).toEqual(['b', 'a', 'c']);
    expect(moveTo(o, 'c', 9)).toBe(o);
    expect(moveTo(o, 'b', -3)).toEqual(['b', 'a', 'c']);
  });
});

/** The hub's side of the list (T50): the order record, archive, restore, delete, rename. */
function hubApi(order: { mode: ListMode; session_ids: string[] } = { mode: 'state', session_ids: [] }) {
  return {
    workspaces: async () => [],
    archivedProjects: vi.fn(async (): Promise<Project[]> => []),
    search: async () => ({ results: [] }),
    sessionOrder: vi.fn(async () => order),
    putSessionOrder: vi.fn(async (_p: string, b: { mode?: ListMode; session_ids?: string[] }) => ({
      mode: b.mode ?? order.mode,
      session_ids: b.session_ids ?? order.session_ids,
    })),
    archive: vi.fn(async (id: string) => ({ ...sess(id, id, 'archived', at(0)), archived_at: at(0) })),
    unarchive: vi.fn(async (id: string) => sess(id, id, 'idle', at(0))),
    deleteSession: vi.fn(async () => {}),
    patchSession: vi.fn(async (id: string, p: { title?: string }) => sess(id, p.title ?? id, 'idle', at(0))),
    createSession: vi.fn(async (_p: string, title: string) => sess('new', title, 'running', at(0))),
    // PUT answers the time it became a favorite, DELETE nothing.
    setSessionFavorite: vi.fn(async (_id: string, on: boolean) => (on ? at(0) : undefined)),
    setProjectFavorite: vi.fn(async (_id: string, on: boolean) => (on ? at(0) : undefined)),
  };
}

function makeApp(list = LIST, api = hubApi()) {
  const socket = { onState: () => () => {} } as unknown as HubSocket;
  const app = new AppState(api as unknown as Api, socket);
  app.me = { id: 'u', email: 'who@example.com', personal_project_id: 'p0', theme: 'warm', look: 'spacious', project_sort: 'custom' };
  app.projects.set('p0', { id: 'p0', name: 'Personal', slug: 'personal', description: '', is_personal: true, color: 'slate', created_at: at(100) });
  app.projects.set('p1', { id: 'p1', name: 'physics-mp', slug: 'physics-mp', description: '', is_personal: false, color: 'blue', icon: { kind: 'emoji', value: '⚛️' }, created_at: at(90) });
  app.currentProjectId = 'p1';
  app.conn = 'open';
  const ws = (id: string, worker: string, path: string) => ({
    id,
    project_id: 'p1',
    worker_id: worker,
    path,
    label: '',
    environment: 'default',
    kind: 'dir',
    ephemeral: false,
    marked_for_deletion: false,
    created_by_hqc: false,
    mirror_enabled: true,
    hqc_rules: 0,
    state: 'ready' as const,
    created_at: at(90),
  });
  app.workspaces.set('p1', [ws('w1', 'wk1', '/Users/me/work/games/phys-mp'), ws('w2', 'wk2', '/Users/me/work/games/phys-det')]);
  for (const s of list) app.sessions.set(s.id, s);
  app.sessions.set('sys1', sess('sys1', 'home-mac', 'idle', at(1), { system: true, project_id: 'p0', workspace_id: undefined }));
  app.sessions.set('sys2', sess('sys2', 'drzlo-mac', 'worker_offline', at(1), { system: true, worker_id: 'wk2', project_id: 'p0', workspace_id: undefined }));
  return app;
}

async function mount(node: preact.ComponentChild) {
  root = document.createElement('div');
  document.body.appendChild(root);
  await act(async () => render(node, root));
}

/** The window's width for the app's media queries: happy-dom's own 1024 px is the narrow
 *  layout (900–1079 px), a desktop is wider. */
const windowWidth = (width: number) =>
  (window as unknown as { happyDOM: { setViewport(v: { width: number }): void } }).happyDOM.setViewport({ width });

const groups = () =>
  [...root.querySelectorAll('.session-group')].map((g) => [
    g.querySelector('.group-label')!.textContent,
    // A custom order has no count.
    g.querySelector('.group-count')?.textContent,
    [...g.querySelectorAll('.session-title')].map((t) => t.textContent),
  ]);

/** A phone's first screen, the Projects view: the names in a group (`wait`, `run`,
 *  `favorites`, `all`), a project's row (in one group) and its marks — how many of its
 *  sessions run, wait for the user and have unread events. */
const phoneNames = (group: string) =>
  [...root.querySelectorAll(`.phone-home .group-${group} a.phone-project .phone-project-name`)].map((n) => n.textContent);
/** Every project on the screen, group by group. */
const phoneAll = () => [...root.querySelectorAll('.phone-home a.phone-project .phone-project-name')].map((n) => n.textContent);
const phoneRow = (name: string) =>
  [...root.querySelectorAll<HTMLElement>('.phone-home a.phone-project')].find(
    (r) => r.querySelector('.phone-project-name')!.textContent === name,
  )!;
const phoneMarks = (name: string) => [...phoneRow(name).querySelectorAll('[aria-label]')].map((m) => m.getAttribute('aria-label'));

/** A right click on a row (a long press on a phone does the same): its menu. */
const menuOf = (title: string, x = 40, y = 120) =>
  act(async () => {
    rowOf(title).dispatchEvent(new MouseEvent('contextmenu', { bubbles: true, cancelable: true, clientX: x, clientY: y }));
  });
const menuItems = () => [...root.querySelectorAll('.menu .menu-item')].map((b) => b.textContent);
const SESSION_MENU = ['Open in new tab', 'Add to favorites', 'Rename', 'Move to project…', 'Folder rules', 'Restart session', 'Archive', 'Delete…'];
/** Archives a row from its menu's item. */
async function archiveRow(title: string) {
  await menuOf(title);
  await click(root.querySelector('.menu .btn-archive'));
}
/** Reorder, from the header's order menu. */
async function startReorder() {
  await click(root.querySelector('.side-head .btn-mode'));
  await click(root.querySelector('.menu .btn-reorder'));
}
/** A key pressed where the focus is (in a menu: its item). */
const pressKey = (key: string) =>
  act(async () => {
    (document.activeElement ?? document).dispatchEvent(new KeyboardEvent('keydown', { key, bubbles: true, cancelable: true }));
  });

describe('SessionList (desktop column)', () => {
  it('a header in one row: the name, search, the order, the project menu, a new session; then the groups', async () => {
    await mount(<SessionList app={makeApp()} current="net" />);
    const head = root.querySelector('.side-head')!;
    expect(head.querySelector('.project-title')!.textContent).toBe('physics-mp');
    expect([...head.children].map((c) => c.getAttribute('aria-label') ?? c.textContent)).toEqual([
      'physics-mp',
      'Search sessions and messages',
      'Session order: State',
      'Project: settings, favorites, archive',
      'New session',
    ]);
    // No tile, no "1 needs you · 2 running", no gear, no row of State / Recent / Custom.
    expect(root.querySelector('.side-tile, .side-counts, .btn-project-settings, .seg-group')).toBeNull();
    expect(groups()).toEqual([
      ['Needs you', '1', ['Network sync']],
      ['Running', '2', ['Stone textures', 'Step determinism']],
      ['Idle · Yesterday', '1', ['Collision profiling']],
      ['Idle · Last 7 days', '1', ['Cave scene port']],
    ]);
    const net = root.querySelector('.session-row.is-current')!;
    expect(net.getAttribute('aria-current')).toBe('page');
    // Two lines: the title, the state under it; no worker or folder.
    expect(net.querySelector('.session-sub')!.textContent).toBe('Waiting for reply');
    expect(net.querySelector('.session-where')).toBeNull();
    expect(net.querySelector('.dot-wait')).not.toBeNull();
    expect(root.querySelector('.session-when')).toBeNull();
    expect(net.getAttribute('title')).toBeNull();
    // No buttons on a row.
    expect(root.querySelector('.session-item button')).toBeNull();
  });

  it('the compact look: a line per row, the subagents after the title, no tooltip (the Compact look)', async () => {
    const app = makeApp(LIST.map((s) => (s.id === 'stones' ? { ...s, subagents: 1 } : s)));
    app.me = { ...app.me!, look: 'compact' };
    await mount(<SessionList app={app} current="net" />);
    expect(root.querySelector('.session-row.is-current')!.getAttribute('title')).toBeNull();
    expect(rowOf('Stone textures').querySelector('.session-ag')!.textContent).toBe('1 agent');
    expect(rowOf('Network sync').querySelector('.session-ag')).toBeNull();
    expect(root.querySelector('.session-when')).toBeNull();
  });

  it('a session open in VS Code is marked </>, one working there is running; in both looks', async () => {
    const list = [
      ...LIST,
      sess('vs', 'Fork in VS Code', 'locked_vscode', at(1), { held_by: { entrypoint: 'claude-vscode', status: 'idle' } }),
      sess('vsbusy', 'Busy in VS Code', 'locked_vscode', at(2), { held_by: { entrypoint: 'claude-vscode', status: 'busy' } }),
    ];
    for (const look of ['spacious', 'compact'] as const) {
      const app = makeApp(list);
      app.me = { ...app.me!, look };
      await mount(<SessionList app={app} />);
      expect(rowOf('Fork in VS Code').querySelector('.dot-vs svg')).not.toBeNull();
      expect(rowOf('Fork in VS Code').querySelector('.dot')).toBeNull();
      expect(rowOf('Busy in VS Code').querySelector('.dot-run')).not.toBeNull();
      expect(groups()[1]).toEqual(['Running', '3', ['Stone textures', 'Step determinism', 'Busy in VS Code']]);
      render(null, root);
    }
  });

  it("the row under the mouse: round archive and favorites buttons past the column's edge, level with the row", async () => {
    vi.useFakeTimers({ toFake: ['Date', 'setTimeout', 'clearTimeout'] });
    vi.setSystemTime(NOW);
    // The column ends at 300 px; every row is 40 px high at y = 100, inside the column.
    const box = (left: number, top: number, width: number, height: number) =>
      ({ left, top, width, height, right: left + width, bottom: top + height, x: left, y: top, toJSON: () => ({}) }) as DOMRect;
    const rect = vi.spyOn(HTMLElement.prototype, 'getBoundingClientRect').mockImplementation(function (this: HTMLElement) {
      if (this.classList.contains('sessions')) return box(0, 0, 300, 800);
      if (this.classList.contains('session-item')) return box(8, 100, 284, 40);
      return box(0, 0, 0, 0);
    });
    try {
      const api = hubApi();
      const app = makeApp(LIST, api);
      await mount(<SessionList app={app} />);
      const ev = (el: Element, type: string, pointerType = 'mouse') =>
        act(async () => {
          el.dispatchEvent(new PointerEvent(type, { pointerType }));
        });
      const wait = (ms: number) =>
        act(async () => {
          vi.advanceTimersByTime(ms);
        });
      const button = () => root.querySelector<HTMLElement>('.btn-row-archive');
      const fav = () => root.querySelector<HTMLElement>('.btn-row-favorite');
      const bar = () => root.querySelector<HTMLElement>('.row-buttons');
      expect(bar()).toBeNull();

      await ev(rowOf('Network sync'), 'pointerenter');
      const b = bar()!;
      expect(button()!.getAttribute('aria-label')).toBe('Archive «Network sync»');
      // Archive, then the star.
      expect([...b.children]).toEqual([button(), fav()]);
      // Outside the row and past the width grip (9 px over the edge), centred on the row.
      expect([b.style.left, b.style.top]).toEqual(['312px', '105px']);
      expect(rowOf('Network sync').contains(b)).toBe(false);
      // The way from the row to the buttons crosses the column's edge: they stay.
      await ev(rowOf('Network sync'), 'pointerleave');
      await wait(200);
      await ev(b, 'pointerenter');
      await wait(1000);
      expect(bar()).toBe(b);
      // Off both: they go.
      await ev(b, 'pointerleave');
      await wait(300);
      expect(bar()).toBeNull();
      // No card in the place of a tooltip: the mouse resting on a row shows only the buttons.
      await ev(rowOf('Cave scene port'), 'pointerenter');
      await wait(1000);
      expect(root.querySelector('[role="tooltip"]')).toBeNull();

      // A click archives the row, with the Undo toast.
      await click(button());
      expect(button()).toBeNull();
      expect(titles()).not.toContain('Cave scene port');
      expect(root.querySelector('.toast-text')!.textContent).toBe('Archived «Cave scene port»');
      await click(root.querySelector('.btn-undo'));
      expect(titles()).toContain('Cave scene port');
      expect(api.archive).not.toHaveBeenCalled();

      // The star adds the row to the favorites and, filled on a favorite, takes it out.
      await ev(rowOf('Cave scene port'), 'pointerenter');
      expect(fav()!.getAttribute('aria-label')).toBe('Add to favorites «Cave scene port»');
      expect(fav()!.classList.contains('is-on')).toBe(false);
      await click(fav());
      expect(api.setSessionFavorite).toHaveBeenLastCalledWith('cave', true);
      expect(root.querySelector('.fav-pinned')!.textContent).toContain('Cave scene port');
      expect(bar()).toBeNull();
      await ev(rowOf('Cave scene port'), 'pointerenter');
      expect(fav()!.getAttribute('aria-label')).toBe('Remove from favorites «Cave scene port»');
      expect(fav()!.classList.contains('is-on')).toBe(true);
      await click(fav());
      expect(api.setSessionFavorite).toHaveBeenLastCalledWith('cave', false);
      expect(root.querySelector('.fav-pinned')).toBeNull();

      // Not for a touch, not while the row's menu is open.
      await ev(rowOf('Stone textures'), 'pointerenter', 'touch');
      expect(bar()).toBeNull();
      await ev(rowOf('Stone textures'), 'pointerenter');
      expect(bar()).not.toBeNull();
      await menuOf('Stone textures');
      expect(bar()).toBeNull();
    } finally {
      rect.mockRestore();
    }
  });

  it("the row's buttons follow their row under a still mouse, and go with it", async () => {
    const box = (left: number, top: number, width: number, height: number) =>
      ({ left, top, width, height, right: left + width, bottom: top + height, x: left, y: top, toJSON: () => ({}) }) as DOMRect;
    let rowTop = 100;
    const rect = vi.spyOn(HTMLElement.prototype, 'getBoundingClientRect').mockImplementation(function (this: HTMLElement) {
      if (this.classList.contains('sessions')) return box(0, 0, 300, 800);
      if (this.classList.contains('session-item')) return box(8, rowTop, 284, 40);
      return box(0, 0, 0, 0);
    });
    try {
      const app = makeApp();
      await mount(<SessionList app={app} />);
      const button = () => root.querySelector<HTMLElement>('.row-buttons');
      await act(async () => {
        rowOf('Step determinism').dispatchEvent(new PointerEvent('pointerenter', { pointerType: 'mouse' }));
      });
      expect(button()!.style.top).toBe('105px');
      // The rows above it changed: it stands 60 px lower now, the mouse did not move.
      rowTop = 160;
      await act(async () => app.setDevice({}));
      expect(button()!.style.top).toBe('165px');
      // It went to another group (Running → Idle): its element is gone, the button too.
      const det = app.sessions.get('det')!;
      await act(async () =>
        app.mergeSummary([
          {
            session_id: 'det',
            project_id: 'p1',
            title: det.title,
            state: 'idle',
            queue_len: 0,
            unread: 0,
            last_seq: det.last_seq,
            last_read_seq: det.last_read_seq,
            permission_mode: det.permission_mode,
            effective_permission_mode: det.effective_permission_mode,
          },
        ]),
      );
      expect(groups().find((g) => g[0] === 'Running')![2]).not.toContain('Step determinism');
      expect(button()).toBeNull();
    } finally {
      rect.mockRestore();
    }
  });

  it('shows the subagents running in the row, next to the state', async () => {
    const list = LIST.map((s) => (s.id === 'stones' ? { ...s, subagents: 2 } : s.id === 'net' ? { ...s, subagents: 1 } : s));
    await mount(<SessionList app={makeApp(list)} current="net" />);
    const sub = (title: string) =>
      [...root.querySelectorAll('.session-row')].find((r) => r.querySelector('.session-title')!.textContent === title)!.querySelector('.session-sub')!;
    expect(sub('Stone textures').textContent).toBe('Running · 2 agents');
    expect(sub('Network sync').textContent).toBe('Waiting for reply · 1 agent');
    expect(sub('Step determinism').textContent).toBe('Running');
    expect(sub('Stone textures').querySelector('.session-agents')!.textContent).toBe(' · 2 agents');
  });

  it('marks a session with an unsent message as a draft, not the open one', async () => {
    localStorage.clear();
    const app = makeApp(LIST);
    app.setDraft('stones', 'half a thought');
    app.setDraft('net', 'typed here');
    await mount(<SessionList app={app} current="net" />);
    const row = (title: string) =>
      [...root.querySelectorAll('.session-row')].find((r) => r.querySelector('.session-title')!.textContent === title)!;
    expect(row('Stone textures').querySelector('.session-draft')!.textContent).toBe('Draft');
    expect(row('Network sync').querySelector('.session-draft')).toBeNull();
    expect(row('Step determinism').querySelector('.session-draft')).toBeNull();
    await act(async () => app.setDraft('stones', '  '));
    expect(row('Stone textures').querySelector('.session-draft')).toBeNull();
    localStorage.clear();
  });

  it('switches to "Recent" from the order menu, no time in a row, and keeps the choice on the hub (T50)', async () => {
    const api = hubApi();
    await mount(<SessionList app={makeApp(LIST, api)} />);
    expect(api.sessionOrder).toHaveBeenCalledWith('p1');
    await click(root.querySelector('.side-head .btn-mode'));
    expect(menuItems()).toEqual(['State', 'Recent', 'Custom', 'Reorder…']);
    expect(root.querySelector('.menu .btn-mode-state')!.getAttribute('aria-checked')).toBe('true');
    expect(root.querySelector('.menu .btn-mode-recent')!.getAttribute('aria-checked')).toBe('false');
    await click(root.querySelector('.menu .btn-mode-recent'));
    expect(root.querySelector('.ctx-menu')).toBeNull();
    expect(groups().map((g) => g[0])).toEqual(['Today', 'Yesterday', 'Last 7 days']);
    expect(root.querySelector('.session-when')).toBeNull();
    expect(api.putSessionOrder).toHaveBeenCalledWith('p1', { mode: 'recent' });
    expect(root.querySelector('.side-head .btn-mode')!.textContent).toBe('Recent');
    // The button toggles its menu; Esc closes it.
    await click(root.querySelector('.side-head .btn-mode'));
    await click(root.querySelector('.side-head .btn-mode'));
    expect(root.querySelector('.ctx-menu')).toBeNull();
    await click(root.querySelector('.side-head .btn-mode'));
    expect(document.activeElement).toBe(root.querySelector('.menu .btn-mode-state'));
    await pressKey('Escape');
    expect(root.querySelector('.ctx-menu')).toBeNull();
    // The focus goes back to the button, not to the page.
    expect(document.activeElement).toBe(root.querySelector('.side-head .btn-mode'));
    render(null, root);
    // Another device: the hub's record.
    await mount(<SessionList app={makeApp(LIST, hubApi({ mode: 'recent', session_ids: [] }))} />);
    expect(root.querySelector('.side-head .btn-mode')!.textContent).toBe('Recent');
    expect(groups()[0][0]).toBe('Today');
  });

  it('the magnifier opens the search page; the column keeps its name and every group', async () => {
    await mount(<SessionList app={makeApp()} />);
    const open = root.querySelector<HTMLAnchorElement>('.side-head .btn-search')!;
    expect(open.tagName).toBe('A');
    expect(open.getAttribute('href')).toBe('#/search');
    expect(root.querySelector('.side-head input')).toBeNull();
    expect(root.querySelector('.side-head .project-title')!.textContent).toBe('physics-mp');
    expect(groups()).toHaveLength(4);
  });

  it("the workers are one line: how many are online, the open session's machine and what runs there", async () => {
    await mount(<SessionList app={makeApp()} current="net" />);
    expect(root.querySelector('.worker-list')).toBeNull();
    const line = root.querySelector<HTMLElement>('.wline')!;
    expect(line.querySelector('.wline-count')!.textContent).toBe('1 worker online');
    expect(line.querySelector('.wline-here')!.textContent).toBe('home-mac · 1 running');
    expect(line.querySelector('.dot-online')).not.toBeNull();
    // A click opens the workers' list above it, offline ones grey.
    await click(line);
    const rows = [...root.querySelectorAll('.wline-pop .worker-list .worker-row')];
    expect(rows.map((r) => [r.querySelector('.worker-name')!.textContent, r.querySelector('.worker-state')!.textContent])).toEqual([
      ['drzlo-mac', 'offline'],
      ['home-mac', '1 running'],
    ]);
    expect(rows[0].querySelector('.dot-offline')).not.toBeNull();
    expect(rows[1].getAttribute('href')).toBe('#/s/sys1');
    await pressKey('Escape');
    expect(root.querySelector('.worker-list')).toBeNull();
    // No session open: no machine.
    render(null, root);
    await mount(<SessionList app={makeApp()} />);
    expect(root.querySelector('.wline-here')).toBeNull();
  });

  it("a worker counts its sessions by the list's Running rule: busy in VS Code and idle with subagents or a background task at work yes, waiting no", async () => {
    const list = [
      ...LIST.map((s) =>
        s.id === 'prof'
          ? { ...s, state: 'locked_vscode' as const, held_by: { entrypoint: 'claude-vscode', status: 'busy' as const } }
          : s.id === 'cave'
            ? { ...s, subagents: 2 }
            : s,
      ),
      sess('bake', 'Glaze bake', 'idle', at(30), { background_tasks: 1 }),
    ];
    const app = makeApp(list);
    await mount(<SessionList app={app} current="net" />);
    const running = groups().find((g) => g[0] === 'Running')![2] as string[];
    expect(running).toEqual(['Stone textures', 'Step determinism', 'Collision profiling', 'Glaze bake', 'Cave scene port']);
    // On home-mac (wk1): the four of the Running group that run there.
    expect(app.workerRunning('wk1')).toBe(running.filter((t) => list.find((s) => s.title === t)!.worker_id === 'wk1').length);
    expect(root.querySelector('.wline-here')!.textContent).toBe('home-mac · 4 running');
  });
});

const titles = () => [...root.querySelectorAll('.session-title')].map((t) => t.textContent);
const rowOf = (title: string) =>
  [...root.querySelectorAll<HTMLElement>('.session-item')].find((r) => r.querySelector('.session-title')!.textContent === title)!;
/** A click, and the hub's (mocked) replies it waits for. */
const click = (el: Element | null) =>
  act(async () => {
    (el as HTMLElement).click();
    for (let i = 0; i < 5; i++) await Promise.resolve();
  });

describe('archive from the row (T50)', () => {
  beforeEach(() => vi.useFakeTimers({ toFake: ['Date', 'setTimeout', 'clearTimeout'] }));

  it('hides the row at once and archives only when the toast ends; Undo sends nothing', async () => {
    const api = hubApi();
    const app = makeApp(LIST, api);
    await mount(<SessionList app={app} />);
    // The menu stands at the click, the row under it lit; the archive is an item in it, with
    // no button beside the menu.
    await menuOf('Network sync', 40, 120);
    const menu = root.querySelector<HTMLElement>('.ctx-menu')!;
    expect([menu.style.left, menu.style.top]).toEqual(['40px', '120px']);
    expect(menu.getAttribute('aria-label')).toBe('Session «Network sync»');
    expect(rowOf('Network sync').querySelector('.session-row.is-menu')).not.toBeNull();
    expect(menuItems()).toEqual(SESSION_MENU);
    expect(menu.querySelectorAll('.menu-sep')).toHaveLength(2);
    expect([...menu.children].every((c) => c.matches('.menu-item, .menu-sep'))).toBe(true);
    // The first item has the focus; ↓ goes on, ↑ from the first goes round to the last.
    expect(document.activeElement!.textContent).toBe('Open in new tab');
    await pressKey('ArrowDown');
    expect(document.activeElement!.textContent).toBe('Add to favorites');
    await pressKey('ArrowUp');
    await pressKey('ArrowUp');
    expect(document.activeElement!.textContent).toBe('Delete…');

    await click(menu.querySelector('.btn-archive'));
    expect(root.querySelector('.ctx-menu')).toBeNull();
    expect(titles()).not.toContain('Network sync');
    expect(root.querySelector('.toast')!.textContent).toBe('Archived «Network sync»Undo');
    await act(async () => {
      vi.advanceTimersByTime(ARCHIVE_UNDO_MS - 1);
    });
    expect(api.archive).not.toHaveBeenCalled();
    await click(root.querySelector('.btn-undo'));
    expect(root.querySelector('.toast')).toBeNull();
    expect(groups()[0]).toEqual(['Needs you', '1', ['Network sync']]);
    await act(async () => {
      vi.advanceTimersByTime(ARCHIVE_UNDO_MS * 2);
    });
    expect(api.archive).not.toHaveBeenCalled();

    // Again, from the menu's item, and the toast runs out: now the hub, and the session is in
    // the archive.
    await menuOf('Network sync');
    await click(root.querySelector('.menu .btn-archive'));
    await act(async () => {
      vi.advanceTimersByTime(ARCHIVE_UNDO_MS);
    });
    expect(api.archive).toHaveBeenCalledTimes(1);
    expect(api.archive).toHaveBeenCalledWith('net', false);
    expect(root.querySelector('.toast')).toBeNull();
    expect(app.sessions.get('net')!.archived).toBe(true);
    expect(root.querySelector('.btn-archive-open')!.textContent).toBe('Archive1');
  });

  it('archiving another row sends the previous one first; leaving the page sends with keepalive', async () => {
    const api = hubApi();
    const app = makeApp(LIST, api);
    const win = new EventTarget() as unknown as Window;
    const doc = Object.assign(new EventTarget(), { visibilityState: 'visible' });
    Object.defineProperty(win, 'document', { value: doc });
    app.watchPageLeave(win);
    await mount(<SessionList app={app} />);
    await archiveRow('Network sync');
    await archiveRow('Cave scene port');
    expect(api.archive.mock.calls).toEqual([['net', false]]);
    expect(root.querySelector('.toast-text')!.textContent).toBe('Archived «Cave scene port»');

    // The list kept for the next load has the session archived on leaving.
    const kept = () =>
      (JSON.parse(localStorage.getItem('hqc.list')!).sessions as SessionInfo[]).filter((s) => s.archived).map((s) => s.id);
    await act(async () => {
      doc.visibilityState = 'hidden';
      doc.dispatchEvent(new Event('visibilitychange'));
    });
    expect(api.archive.mock.calls).toEqual([
      ['net', false],
      ['cave', true],
    ]);
    expect(kept()).toContain('cave');
    expect(root.querySelector('.toast')).toBeNull();
    await archiveRow('Stone textures');
    await act(async () => {
      win.dispatchEvent(new Event('pagehide'));
    });
    expect(api.archive.mock.calls.at(-1)).toEqual(['stones', true]);
    expect(kept()).toContain('stones');
  });

  it('forgets the kept list when it cannot be written', () => {
    const app = makeApp();
    const win = new EventTarget() as unknown as Window;
    Object.defineProperty(win, 'document', { value: Object.assign(new EventTarget(), { visibilityState: 'visible' }) });
    app.watchPageLeave(win);
    // A full storage: the write throws, the stale list must not stay.
    const full = {
      getItem: () => null,
      setItem: () => {
        throw new Error('QuotaExceededError');
      },
      removeItem: vi.fn(),
    };
    vi.stubGlobal('localStorage', full);
    try {
      win.dispatchEvent(new Event('pagehide'));
    } finally {
      vi.unstubAllGlobals();
    }
    expect(full.removeItem).toHaveBeenCalledWith('hqc.list');
  });

  it('returns an undone session to its place in Custom; a new session goes on top', async () => {
    const order = { mode: 'custom' as const, session_ids: ['cave', 'net', 'stones', 'det', 'prof'] };
    const app = makeApp(LIST, hubApi(order));
    await mount(<SessionList app={app} />);
    expect(groups()).toEqual([['Custom order', undefined, ['Cave scene port', 'Network sync', 'Stone textures', 'Step determinism', 'Collision profiling']]]);
    await archiveRow('Stone textures');
    expect(titles()).toEqual(['Cave scene port', 'Network sync', 'Step determinism', 'Collision profiling']);
    await click(root.querySelector('.btn-undo'));
    expect(titles()).toEqual(['Cave scene port', 'Network sync', 'Stone textures', 'Step determinism', 'Collision profiling']);
    await act(async () => void (await app.createSession('New session', {}, 'p1')));
    expect(titles()[0]).toBe('New session');
  });

  it('renames from the menu, and deletes after a second step in it', async () => {
    const api = hubApi();
    const app = makeApp(LIST, api);
    vi.stubGlobal('prompt', () => 'Net sync');
    await mount(<SessionList app={app} />);
    await menuOf('Network sync');
    await click(root.querySelector('.menu .btn-rename'));
    expect(api.patchSession).toHaveBeenCalledWith('net', { title: 'Net sync' });
    expect(root.querySelector('.ctx-menu')).toBeNull();
    vi.unstubAllGlobals();

    await menuOf('Cave scene port');
    await click(root.querySelector('.menu .btn-delete'));
    const menu = root.querySelector('.ctx-menu')!;
    expect(menu.querySelector('.menu-note')!.textContent).toBe(
      'Delete this session forever? Messages and history go too. Files on disk stay.',
    );
    expect([...menu.querySelectorAll('.menu-item')].map((b) => b.textContent)).toEqual(['Delete forever', 'Cancel']);
    // The question in place of the items: no Archive beside it.
    expect(menu.querySelector('.btn-archive')).toBeNull();
    // The focus is on Cancel: Enter right away deletes nothing.
    expect(document.activeElement).toBe(menu.querySelector('.btn-cancel-delete'));
    await click(menu.querySelector('.btn-cancel-delete'));
    expect(root.querySelector('.ctx-menu')).toBeNull();
    expect(api.deleteSession).not.toHaveBeenCalled();
    await menuOf('Cave scene port');
    await click(root.querySelector('.menu .btn-delete'));
    await click(root.querySelector('.menu .btn-confirm-delete'));
    expect(api.deleteSession).toHaveBeenCalledWith('cave');
    expect(titles()).not.toContain('Cave scene port');
  });

  it('restarts the session from the menu with POST /restart; a session VS Code drives has no Restart', async () => {
    const sent: { method?: string; url: string }[] = [];
    const fetchHub: typeof fetch = async (input, init) => {
      sent.push({ method: init?.method, url: String(input) });
      return new Response('{"sent":true}', { status: 200, headers: { 'content-type': 'application/json' } });
    };
    const list = [...LIST, sess('vsc', 'Opened in VS Code', 'locked_vscode', at(1))];
    await mount(<SessionList app={makeApp(list, Object.assign(new Api('', fetchHub), hubApi()))} />);
    await menuOf('Network sync');
    await click(root.querySelector('.menu .btn-restart'));
    expect(root.querySelector('.ctx-menu')).toBeNull();
    expect(sent.filter((r) => r.url.endsWith('/restart'))).toEqual([{ method: 'POST', url: '/api/sessions/net/restart' }]);

    await menuOf('Opened in VS Code');
    expect(menuItems()).toEqual(SESSION_MENU.filter((l) => l !== 'Restart session'));
  });

  it('the menu: links to a new tab and to the folder rules, the move form in the column; Esc or a press away closes it', async () => {
    const api = {
      ...hubApi(),
      workerDirs: vi.fn(async () => []),
      moveSession: vi.fn(async (id: string, body: { path: string }) => sess(id, 'Network sync', 'idle', at(0), { cwd: body.path })),
    };
    const app = makeApp(LIST, api);
    await mount(<SessionList app={app} />);
    await menuOf('Network sync');
    const tab = root.querySelector('.menu a.btn-open-tab')!;
    expect([tab.getAttribute('href'), tab.getAttribute('target')]).toEqual(['#/s/net', '_blank']);
    expect(root.querySelector('.menu a.btn-rules')!.getAttribute('href')).toBe('#/p/p1/rules/w1?session=net');
    await pressKey('Escape');
    expect(root.querySelector('.ctx-menu')).toBeNull();
    expect(rowOf('Network sync').querySelector('.is-menu')).toBeNull();
    // A row focused before its menu (a right click focuses it) has the focus back after Esc.
    const link = rowOf('Network sync').querySelector<HTMLElement>('a.session-row')!;
    link.focus();
    await menuOf('Network sync');
    expect(document.activeElement!.textContent).toBe('Open in new tab');
    await pressKey('Escape');
    expect(document.activeElement).toBe(link);
    await menuOf('Network sync');
    await act(async () => {
      document.body.dispatchEvent(new PointerEvent('pointerdown', { bubbles: true }));
    });
    expect(root.querySelector('.ctx-menu')).toBeNull();
    // A press away leaves the focus to that press: pulling it back to the row would scroll the
    // list back to it and lose the click on another row.
    expect(document.activeElement).not.toBe(link);

    await menuOf('Network sync');
    await click(root.querySelector('.menu .btn-move'));
    expect(root.querySelector('.ctx-menu')).toBeNull();
    const form = root.querySelector('.move-panel form.move-session')!;
    await act(async () => {
      const path = form.querySelector<HTMLInputElement>('input[name=path]')!;
      path.value = '/Users/me/work/games/phys-mp/netcode';
      path.dispatchEvent(new Event('input', { bubbles: true }));
    });
    await act(async () => {
      form.dispatchEvent(new Event('submit', { bubbles: true, cancelable: true }));
      for (let i = 0; i < 5; i++) await Promise.resolve();
    });
    expect(api.moveSession).toHaveBeenCalledWith('net', { path: '/Users/me/work/games/phys-mp/netcode', project: undefined, create: undefined });
    expect(root.querySelector('.move-panel')).toBeNull();
  });
});

describe('Reorder (T50)', () => {
  it('moves a row with the arrow keys on its handle; Done saves the order and turns Custom on', async () => {
    const api = hubApi();
    const app = makeApp(LIST, api);
    await mount(<SessionList app={app} />);
    await startReorder();
    expect(root.querySelector('.reorder-bar')!.textContent).toBe('ReorderCancelDone');
    expect(root.querySelector('.ctx-menu')).toBeNull();
    // The order can't change mid-way, and a row has no menu or link.
    expect(root.querySelector<HTMLButtonElement>('.side-head .btn-mode')!.disabled).toBe(true);
    expect(root.querySelector('a.session-row')).toBeNull();
    await menuOf('Network sync');
    expect(root.querySelector('.ctx-menu')).toBeNull();
    // The order so far: no record yet, so newest first.
    expect(groups()).toEqual([['Drag to reorder', undefined, ['Stone textures', 'Step determinism', 'Network sync', 'Collision profiling', 'Cave scene port']]]);
    const key = (title: string, k: string) =>
      act(async () => {
        rowOf(title).querySelector('.grip')!.dispatchEvent(new KeyboardEvent('keydown', { key: k, bubbles: true }));
      });
    await key('Cave scene port', 'ArrowUp');
    await key('Cave scene port', 'ArrowUp');
    await key('Stone textures', 'ArrowDown');
    await key('Stone textures', 'ArrowLeft');
    expect(titles()).toEqual(['Step determinism', 'Stone textures', 'Cave scene port', 'Network sync', 'Collision profiling']);
    // The moved handle keeps the focus.
    expect(document.activeElement).toBe(rowOf('Stone textures').querySelector('.grip'));

    await click(root.querySelector('.btn-reorder-done'));
    expect(api.putSessionOrder).toHaveBeenCalledWith('p1', { mode: 'custom', session_ids: ['det', 'stones', 'cave', 'net', 'prof'] });
    expect(root.querySelector('.side-head .btn-mode')!.textContent).toBe('Custom');
    expect(groups()).toEqual([['Custom order', undefined, ['Step determinism', 'Stone textures', 'Cave scene port', 'Network sync', 'Collision profiling']]]);
  });

  it('drags a row by its handle with pointer events', async () => {
    await mount(<SessionList app={makeApp()} />);
    await startReorder();
    const grip = rowOf('Stone textures').querySelector<HTMLElement>('.grip')!;
    const ev = (type: string, y: number) =>
      act(async () => {
        grip.dispatchEvent(new PointerEvent(type, { bubbles: true, cancelable: true, pointerId: 1, clientY: y }));
      });
    await ev('pointerdown', 100);
    expect(rowOf('Stone textures').classList.contains('is-dragging')).toBe(true);
    // A row is 64 px here (no layout in the test): two rows down, then one back.
    await ev('pointermove', 230);
    expect(titles().slice(0, 3)).toEqual(['Step determinism', 'Network sync', 'Stone textures']);
    await ev('pointermove', 170);
    expect(titles().slice(0, 3)).toEqual(['Step determinism', 'Stone textures', 'Network sync']);
    await ev('pointerup', 170);
    expect(root.querySelector('.is-dragging')).toBeNull();
    await ev('pointermove', 400);
    expect(titles().slice(0, 3)).toEqual(['Step determinism', 'Stone textures', 'Network sync']);
  });

  it('Cancel restores the previous order', async () => {
    const order = { mode: 'custom' as const, session_ids: ['cave', 'net', 'stones', 'det', 'prof'] };
    const api = hubApi(order);
    await mount(<SessionList app={makeApp(LIST, api)} />);
    await startReorder();
    await act(async () => {
      rowOf('Cave scene port').querySelector('.grip')!.dispatchEvent(new KeyboardEvent('keydown', { key: 'ArrowDown', bubbles: true }));
    });
    expect(titles()[0]).toBe('Network sync');
    await click(root.querySelector('.btn-reorder-cancel'));
    expect(api.putSessionOrder).not.toHaveBeenCalled();
    expect(titles()).toEqual(['Cave scene port', 'Network sync', 'Stone textures', 'Step determinism', 'Collision profiling']);
  });
});

describe('archive mode (T50)', () => {
  const archived = [
    sess('old', 'Old SPI experiment', 'archived', at(100), { archived_at: new Date(NOW - 30 * 60_000).toISOString() }),
    sess('older', 'Test session', 'archived', at(300)),
  ];

  it('a plain hatched list with Restore and Delete on every row, delete confirmed in the row', async () => {
    const api = hubApi();
    const app = makeApp([...LIST, ...archived], api);
    await mount(<SessionList app={app} current="old" />);
    await click(root.querySelector('.btn-archive-open'));
    const side = root.querySelector('.sessions-archive')!;
    expect(side.querySelector('.archive-head.stripes .project-title')!.textContent).toBe('Archive');
    expect(side.querySelector('.side-counts')!.textContent).toBe('physics-mp · 2 sessions');
    expect(side.querySelector('.archive-note')!.textContent).toBe('Read-only. Restore a session to keep working, or delete it for good.');
    expect(root.querySelector('.worker-list')).toBeNull();
    expect(root.querySelector('.seg-group')).toBeNull();
    const rows = () => [...root.querySelectorAll<HTMLElement>('.archive-row')];
    expect(rows().map((r) => [r.querySelector('.session-title')!.textContent, r.querySelector('.session-sub')!.textContent])).toEqual([
      ['Old SPI experiment', 'Archived today at 19:20'],
      ['Test session', 'Archived'],
    ]);
    expect(rows()[0].classList.contains('is-current')).toBe(true);
    expect(rows()[0].querySelector('a')!.getAttribute('href')).toBe('#/s/old');
    expect(rows().map((r) => [...r.querySelectorAll('.archive-acts button')].map((b) => b.textContent))).toEqual([
      ['Restore', 'Delete'],
      ['Restore', 'Delete'],
    ]);

    await click(rows()[1].querySelector('.btn-delete'));
    expect(rows()[1].querySelector('.row-confirm')!.textContent).toBe('Delete forever?DeleteCancel');
    await click(rows()[1].querySelector('.btn-confirm-delete'));
    expect(api.deleteSession).toHaveBeenCalledWith('older');
    expect(rows()).toHaveLength(1);
    expect(root.querySelector('.toast')!.textContent).toBe('Deleted «Test session»');

    await click(rows()[0].querySelector('.btn-restore'));
    expect(api.unarchive).toHaveBeenCalledWith('old');
    expect(root.querySelector('.empty')!.textContent).toBe('Archive is empty');
    expect(root.querySelector('.toast')!.textContent).toBe('Restored «Old SPI experiment» to sessions');

    await click(root.querySelector('.btn-archive-back'));
    expect(root.querySelector('.sessions-archive')).toBeNull();
    expect(titles()).toContain('old');
  });
});

describe('ProjectRail', () => {
  it('shows icons on the project colors, the current project and a "needs you" badge (T49)', async () => {
    await mount(<ProjectRail app={makeApp()} />);
    const rows = [...root.querySelectorAll<HTMLElement>('.rail-project')];
    // Without an icon the first letters, as before; an emoji as it is.
    expect(rows.map((r) => [r.getAttribute('aria-label'), r.textContent])).toEqual([
      ['Personal', 'P'],
      ['physics-mp', '⚛️'],
    ]);
    expect(root.querySelector('.rail-project.is-current')!.getAttribute('aria-label')).toBe('physics-mp');
    // Another project: its color at 24 %; the current one: --mark-bg from CSS and the pill.
    expect(rows[0].style.background).toBe('rgba(138, 147, 166, 0.24)');
    expect(rows[1].style.background).toBe('');
    expect(root.querySelectorAll('.rail-pill')).toHaveLength(1);
    expect(root.querySelector('.rail-pill')!.nextElementSibling).toBe(rows[1]);
    const badges = [...root.querySelectorAll('.rail-badge')];
    expect(badges.map((b) => b.textContent)).toEqual(['1']);
    expect(badges[0].getAttribute('aria-label')).toBe('needs you: 1');
    expect(root.querySelector('.avatar')!.getAttribute('title')).toBe('who@example.com');
  });

  it('in the Compact look, a row per project: its icon on its color and its full name', async () => {
    const app = makeApp();
    // As `/api/me` brings it: `setLookName` would ask the hub.
    app.me = { ...app.me!, look: 'compact' };
    app.projects.set('p2', { id: 'p2', name: 'ukuetis_controller_backend', slug: 'ukuetis', description: '', is_personal: false, color: 'moss', created_at: at(80) });
    await mount(<ProjectRail app={app} />);
    const rows = () => [...root.querySelectorAll<HTMLElement>('.rail-project')];
    expect(rows().map((r) => [r.querySelector('.rail-glyph')!.textContent, r.querySelector('.rail-name')!.textContent])).toEqual([
      ['P', 'Personal'],
      ['⚛️', 'physics-mp'],
      ['UC', 'ukuetis_controller_backend'],
    ]);
    // The hub's name next to its mark; "+ project", the order menu, the settings and the avatar
    // in one row at the bottom.
    expect(root.querySelector('.rail-logo')!.textContent).toBe('HQC');
    expect(root.querySelector('.rail-actions')).toBeNull();
    expect([...root.querySelectorAll('.rail-foot > *')].map((b) => b.getAttribute('aria-label'))).toEqual([
      'New project',
      'Project order: Custom',
      'Settings',
      'who@example.com',
    ]);
    // The color is on the icon, not on the row; the current row is --mark-bg from CSS.
    expect(rows()[0].style.background).toBe('');
    expect(rows()[0].querySelector<HTMLElement>('.rail-glyph')!.style.background).toBe('rgba(138, 147, 166, 0.24)');
    // The badge stays, on physics-mp's row.
    const badges = [...root.querySelectorAll('.rail-item')].map((i) => i.querySelector('.rail-badge')?.textContent ?? null);
    expect(badges).toEqual([null, '1', null]);

    // Reorder: a row is 40 px with its gap, so a drag of 80 px moves a project by two places.
    await act(async () => root.querySelector<HTMLButtonElement>('.rail-order')!.click());
    await act(async () => root.querySelector<HTMLButtonElement>('.menu .btn-reorder-projects')!.click());
    const grip = root.querySelector<HTMLElement>('.rail-drag[data-tile="p0"]')!;
    expect(grip.querySelector('.rail-name')!.textContent).toBe('Personal');
    const ev = (type: string, y: number) =>
      act(async () => {
        grip.dispatchEvent(new PointerEvent(type, { bubbles: true, cancelable: true, pointerId: 1, clientY: y }));
      });
    await ev('pointerdown', 100);
    await ev('pointermove', 180);
    await ev('pointerup', 180);
    // physics-mp works: its hover text says how.
    expect(rows().map((r) => r.getAttribute('title'))).toEqual(['physics-mp · 2 agents · 0 tokens/s', 'ukuetis_controller_backend', 'Personal']);
  });

  it('marks a project with unread events by a blue dot, unless someone waits there', async () => {
    const app = makeApp([
      ...LIST.map((s) => ({ ...s, unread: 2 })),
      sess('notes', 'Notes', 'idle', at(0.5), { project_id: 'p0', unread: 1 }),
      sess('gone', 'Gone', 'archived', at(0.5), { project_id: 'p0', unread: 3 }),
    ]);
    await mount(<ProjectRail app={app} />);
    const dots = () => [...root.querySelectorAll('.rail-item')].map((i) => [!!i.querySelector('.rail-unread'), !!i.querySelector('.rail-badge')]);
    // Personal: an unread session, no one waits; physics-mp: the badge wins.
    expect(dots()).toEqual([
      [true, false],
      [false, true],
    ]);
    // A phone's first screen: a bold name and how many sessions have unread events, beside the
    // other marks.
    await mount(<PhoneHome app={app} />);
    expect(['Personal', 'physics-mp'].map((n) => [phoneMarks(n), phoneRow(n).classList.contains('has-unread')])).toEqual([
      [['unread: 1'], true],
      [['running: 2', 'needs you: 1', 'unread: 5'], true],
    ]);
    app.sessions.set('notes', { ...app.sessions.get('notes')!, unread: 0 });
    await mount(<ProjectRail app={app} />);
    // Archived and service sessions do not count.
    expect(dots()[0]).toEqual([false, false]);
  });

  it('marks a project whose session works by a stripe around its tile, an idle one by nothing', async () => {
    const app = makeApp([
      sess('notes', 'Notes', 'running', at(0.5), { project_id: 'p0', unread: 1 }),
      sess('det', 'Step determinism', 'idle', at(0.1)),
      sess('svc', 'wk1', 'running', at(0.1), { system: true }),
      sess('gone', 'Gone', 'archived', at(0.5), { state: 'running' }),
    ]);
    const marks = () =>
      [...root.querySelectorAll('.rail-item')].map((i) => i.querySelector('.rail-badge, .rail-run, .rail-unread')?.className ?? null);
    await mount(<ProjectRail app={app} />);
    // Personal works, over its unread; physics-mp: a service and an archived session do not
    // count, so it is idle and has no mark at all.
    expect(marks()).toEqual(['rail-run', null]);
    expect(root.querySelector('.rail-run')!.getAttribute('aria-label')).toBe('working');
    expect(root.querySelectorAll('.rail-item')[1].querySelectorAll('[aria-label]')).toHaveLength(1);
    // A phone's first screen: Personal runs one (and has one unread), physics-mp has no mark.
    await mount(<PhoneHome app={app} />);
    expect(['Personal', 'physics-mp'].map(phoneMarks)).toEqual([['running: 1', 'unread: 1'], []]);
    expect(phoneNames('run')).toEqual(['Personal']);
    // The bar's stripe around Personal's tile there too, without a label of its own: the row
    // says how many run.
    const stripes = () => ['Personal', 'physics-mp'].map((n) => !!phoneRow(n).querySelector('.rail-run'));
    expect(stripes()).toEqual([true, false]);
    // Queued input works too; a session waiting for you wins over a working one.
    app.sessions.set('det', { ...app.sessions.get('det')!, state: 'queued' });
    await mount(<ProjectRail app={app} />);
    expect(marks()).toEqual(['rail-run', 'rail-run']);
    await mount(<PhoneHome app={app} />);
    expect(stripes()).toEqual([true, true]);
    app.sessions.set('net', sess('net', 'Network sync', 'waiting', at(0.2)));
    await mount(<ProjectRail app={app} />);
    expect(marks()).toEqual(['rail-run', 'rail-badge']);
    await mount(<PhoneHome app={app} />);
    expect(stripes()).toEqual([true, false]);
  });

  it('runs the stripe faster the more output tokens per second the project writes, and says so on hover', () => {
    // A lap in 18 s (the CSS) without tokens, 9 s at 40 tokens/s, 3 s at the fastest.
    expect(runSpeed(0)).toBe(1);
    expect(runSpeed(40)).toBe(2);
    expect(18 / runSpeed(100)).toBeCloseTo(6.41, 2);
    expect(18 / runSpeed(300)).toBeCloseTo(4.4, 2);
    expect(runSpeed(5000)).toBe(6);
    // Its hover text: the agents at work and their tokens per second.
    expect(runningTitle('hqc', 1, 0)).toBe('hqc · 1 agent · 0 tokens/s');
    expect(runningTitle('hqc', 3, 120)).toBe('hqc · 3 agents · 120 tokens/s');
  });

  it('in the Compact look: the same marks on the icon, an unread project in bold', async () => {
    const app = makeApp([
      ...LIST,
      sess('notes', 'Notes', 'idle', at(0.5), { project_id: 'p0', unread: 1 }),
      sess('etch', 'Etching', 'running', at(0.5), { project_id: 'p2', unread: 2 }),
    ]);
    app.me = { ...app.me!, look: 'compact' };
    app.projects.set('p2', { id: 'p2', name: 'boards', slug: 'boards', description: '', is_personal: false, color: 'moss', created_at: at(80) });
    app.projects.set('p3', { id: 'p3', name: 'kiln', slug: 'kiln', description: '', is_personal: false, color: 'coral', created_at: at(70) });
    await mount(<ProjectRail app={app} />);
    const rows = [...root.querySelectorAll('.rail-item')].map((i) => [
      i.querySelector('.rail-name')!.textContent,
      i.querySelector('.rail-badge, .rail-run, .rail-unread')?.className ?? null,
      i.querySelector('.rail-project')!.classList.contains('has-unread'),
    ]);
    // Personal: unread; physics-mp waits for you; boards works (over its unread); kiln is idle.
    expect(rows).toEqual([
      ['Personal', 'rail-unread', true],
      ['physics-mp', 'rail-badge', false],
      ['boards', 'rail-run', false],
      ['kiln', null, false],
    ]);
  });

  it('a favorite project has a star on its tile beside its other mark; the others have none', async () => {
    const app = makeApp([sess('notes', 'Notes', 'running', at(0.5), { project_id: 'p0' })]);
    app.projects.set('p0', { ...app.projects.get('p0')!, favorite_at: at(1) });
    await mount(<ProjectRail app={app} />);
    const rows = () =>
      [...root.querySelectorAll('.rail-item')].map((i) => [
        i.querySelector('.rail-project')!.getAttribute('aria-label'),
        i.querySelector('.rail-fav')?.getAttribute('aria-label') ?? null,
        i.querySelector('.rail-badge, .rail-run, .rail-unread')?.className ?? null,
      ]);
    expect(rows()).toEqual([
      ['Personal', 'favorite', 'rail-run'],
      ['physics-mp', null, null],
    ]);
    // Unstarred: the star goes, the working stripe stays.
    app.projects.set('p0', { ...app.projects.get('p0')!, favorite_at: undefined });
    await mount(<ProjectRail app={app} />);
    expect(rows()).toEqual([
      ['Personal', null, 'rail-run'],
      ['physics-mp', null, null],
    ]);
  });
});

describe("the project bar's order", () => {
  const project = (id: string, name: string, created: number, more: Partial<Project> = {}): Project => ({
    id,
    name,
    slug: name,
    description: '',
    is_personal: false,
    color: 'moss',
    created_at: at(created),
    ...more,
  });
  /** The hub's order: Personal, physics-mp, kiln, Boards (a favorite), board10, board9, Arduino
   *  (a favorite). Listed sessions: physics-mp's newest 0.01 h ago, kiln's 5 h, Personal's
   *  10 h, Arduino's 30 h; board10 has only an archived one, board9 only a service one, Boards
   *  none. */
  function sortApp(sort: ProjectSort) {
    const api = {
      ...hubApi(),
      setProjectSort: vi.fn(async (project_sort: ProjectSort) => ({ ...app.me!, project_sort })),
      putProjectOrder: vi.fn(async (ids: string[]) => ids.map((id) => app.projects.get(id)!)),
    };
    const app = makeApp(
      [
        ...LIST,
        sess('notes', 'Notes', 'idle', at(10), { project_id: 'p0' }),
        sess('glaze', 'Glaze', 'idle', at(5), { project_id: 'p2' }),
        sess('old', 'Old', 'archived', at(0), { project_id: 'p4' }),
        sess('svc9', 'wk9', 'idle', at(0), { project_id: 'p5', system: true }),
        sess('blink', 'Blink', 'idle', at(30), { project_id: 'p6' }),
      ],
      api as unknown as ReturnType<typeof hubApi>,
    );
    app.me = { ...app.me!, project_sort: sort };
    for (const p of [
      project('p2', 'kiln', 80),
      project('p3', 'Boards', 70, { favorite_at: at(1) }),
      project('p4', 'board10', 60),
      project('p5', 'board9', 50),
      project('p6', 'Arduino', 40, { favorite_at: at(2) }),
    ])
      app.projects.set(p.id, p);
    return { app, api };
  }
  const bar = () => [...root.querySelectorAll('.rail-project')].map((r) => r.getAttribute('aria-label'));
  const settle = () =>
    act(async () => {
      for (let i = 0; i < 20; i++) await Promise.resolve();
    });

  it("Custom, Name, Recent: the favorites first in each, on the bar and on a phone's first screen", async () => {
    await mount(<ProjectRail app={sortApp('custom').app} />);
    expect(bar()).toEqual(['Boards', 'Arduino', 'Personal', 'physics-mp', 'kiln', 'board10', 'board9']);
    // By the shown name, whatever its case, a number in it by its value.
    await mount(<ProjectRail app={sortApp('name').app} />);
    expect(bar()).toEqual(['Arduino', 'Boards', 'board9', 'board10', 'kiln', 'Personal', 'physics-mp']);
    // The newest listed session first; archived and service ones do not count; a project without
    // one after those with one, the newest project first.
    const { app } = sortApp('recent');
    await mount(<ProjectRail app={app} />);
    expect(bar()).toEqual(['Arduino', 'Boards', 'physics-mp', 'kiln', 'Personal', 'board9', 'board10']);
    await mount(<PhoneHome app={app} />);
    // Each group in the bar's order; physics-mp waits for the user, so it is under Needs you.
    expect(phoneNames('favorites')).toEqual(['Arduino', 'Boards']);
    expect(phoneNames('all')).toEqual(['kiln', 'Personal', 'board9', 'board10']);
  });

  it('the order menu: a pick goes to the hub, the bar follows its answer', async () => {
    const { app, api } = sortApp('custom');
    await mount(<ProjectRail app={app} />);
    const btn = root.querySelector('.rail-order')!;
    expect(btn.getAttribute('aria-label')).toBe('Project order: Custom');
    await click(btn);
    const items = () =>
      [...root.querySelectorAll('.menu .menu-item')].map((b) => [b.textContent, b.getAttribute('aria-checked')]);
    expect(items()).toEqual([
      ['Custom', 'true'],
      ['Name', 'false'],
      ['Recent', 'false'],
      ['Reorder…', null],
    ]);
    await click(root.querySelector('.menu .btn-sort-name'));
    await settle();
    expect(api.setProjectSort).toHaveBeenCalledWith('name');
    expect(app.me!.project_sort).toBe('name');
    expect(bar()).toEqual(['Arduino', 'Boards', 'board9', 'board10', 'kiln', 'Personal', 'physics-mp']);
    expect(root.querySelector('.rail-order')!.getAttribute('aria-label')).toBe('Project order: Name');
    await click(root.querySelector('.rail-order'));
    await click(root.querySelector('.menu .btn-sort-recent'));
    await settle();
    expect(api.setProjectSort).toHaveBeenLastCalledWith('recent');
    expect(bar()).toEqual(['Arduino', 'Boards', 'physics-mp', 'kiln', 'Personal', 'board9', 'board10']);
  });

  it("Reorder… arranges the user's own order, and Done makes the bar Custom", async () => {
    const { app, api } = sortApp('name');
    await mount(<ProjectRail app={app} />);
    await click(root.querySelector('.rail-order'));
    await click(root.querySelector('.menu .btn-reorder-projects'));
    const grips = () => [...root.querySelectorAll<HTMLElement>('.rail-drag')];
    // Not the names' order: the one Reorder stores. physics-mp works: its hover text says how.
    expect(grips().map((g) => g.getAttribute('title'))).toEqual([
      'Boards',
      'Arduino',
      'Personal',
      'physics-mp · 2 agents · 0 tokens/s',
      'kiln',
      'board10',
      'board9',
    ]);
    await act(async () => {
      grips().at(-1)!.dispatchEvent(new KeyboardEvent('keydown', { key: 'ArrowUp', bubbles: true }));
    });
    await click(root.querySelector('.rail-done'));
    await settle();
    expect(api.putProjectOrder).toHaveBeenCalledWith(['p3', 'p6', 'p0', 'p1', 'p2', 'p5', 'p4']);
    expect(api.setProjectSort).toHaveBeenCalledWith('custom');
    expect(app.projectSort()).toBe('custom');
    expect(bar()).toEqual(['Boards', 'Arduino', 'Personal', 'physics-mp', 'kiln', 'board9', 'board10']);
  });

  it('a reload shows the order last shown on this device before the hub answers; a pick is kept there', async () => {
    localStorage.setItem('hqc.projectSort', 'name');
    const { app } = sortApp('name');
    app.me = null;
    await mount(<ProjectRail app={app} />);
    expect(bar()).toEqual(['Arduino', 'Boards', 'board9', 'board10', 'kiln', 'Personal', 'physics-mp']);
    // Nothing to pick until the hub says who it is.
    expect(root.querySelector<HTMLButtonElement>('.rail-order')!.disabled).toBe(true);
    const { app: other } = sortApp('custom');
    await mount(<ProjectRail app={other} />);
    await click(root.querySelector('.rail-order'));
    await click(root.querySelector('.menu .btn-sort-recent'));
    await settle();
    expect(localStorage.getItem('hqc.projectSort')).toBe('recent');
  });

  it('a pick shows at once, before the hub answers, and goes back when the hub refuses it', async () => {
    const { app, api } = sortApp('custom');
    let refuse!: (e: Error) => void;
    api.setProjectSort.mockImplementationOnce(() => new Promise((_, rej) => (refuse = rej)));
    await mount(<ProjectRail app={app} />);
    await click(root.querySelector('.rail-order'));
    await click(root.querySelector('.menu .btn-sort-name'));
    // The hub has not answered: the bar is by name already.
    expect(bar()).toEqual(['Arduino', 'Boards', 'board9', 'board10', 'kiln', 'Personal', 'physics-mp']);
    await act(async () => refuse(new Error('hub down')));
    await settle();
    expect(app.projectSort()).toBe('custom');
    expect(localStorage.getItem('hqc.projectSort')).toBe('custom');
    expect(bar()).toEqual(['Boards', 'Arduino', 'Personal', 'physics-mp', 'kiln', 'board10', 'board9']);
  });
});

describe('the project bar: search, menu, archive', () => {
  const project = (id: string, name: string, more: Partial<Project> = {}): Project => ({
    id,
    name,
    slug: name,
    description: '',
    is_personal: false,
    color: 'moss',
    created_at: at(80),
    ...more,
  });
  const BOT = project('p9', '99bot_ng', { archived_at: at(5) });
  const ALL = [
    project('p0', 'Personal', { is_personal: true, color: 'slate' }),
    project('p1', 'physics-mp'),
    project('p2', 'caldavllm_bot'),
    project('p3', 'medication_bot'),
    project('p4', 'kiln'),
    project('p9', '99bot_ng'),
  ];
  /** The hub's side of archived projects, `p9` archived at first: archive and restore answer
   *  the project, the bar's list is the rest in the hub's order. */
  function archiveApi() {
    const archived = new Map([[BOT.id, BOT]]);
    return {
      ...hubApi(),
      archivedProjects: vi.fn(async () => [...archived.values()]),
      archiveProject: vi.fn(async (id: string) => {
        const p = { ...ALL.find((x) => x.id === id)!, archived_at: at(0) };
        archived.set(id, p);
        return p;
      }),
      unarchiveProject: vi.fn(async (id: string) => {
        archived.delete(id);
        return ALL.find((x) => x.id === id)!;
      }),
      projects: vi.fn(async () => ALL.filter((p) => !archived.has(p.id))),
      allSessions: vi.fn(async () => LIST),
      createProject: vi.fn(async (name: string) => project('p5', name)),
      moveSession: vi.fn(async (id: string) => sess(id, 'Moved', 'idle', at(0), { project_id: 'p5' })),
    };
  }
  function railApp(names: boolean) {
    const api = archiveApi();
    const app = makeApp(LIST, api as unknown as ReturnType<typeof hubApi>);
    app.me = { ...app.me!, look: names ? 'compact' : 'spacious' };
    for (const p of ALL.slice(2, 5)) app.projects.set(p.id, p);
    // As the lists brought them.
    app.archivedProjects.set(BOT.id, BOT);
    return { app, api };
  }
  /** The promises of the hub's (mocked) replies, chained after each other. */
  const settle = () =>
    act(async () => {
      for (let i = 0; i < 20; i++) await Promise.resolve();
    });
  const field = () => root.querySelector<HTMLInputElement>('input[aria-label="Search projects"]');
  const typeQuery = (value: string) =>
    act(async () => {
      field()!.value = value;
      field()!.dispatchEvent(new Event('input', { bubbles: true }));
    });
  const hits = () =>
    [...root.querySelectorAll('.rail-hit')].map((h) => [
      h.querySelector('.rail-name')!.textContent,
      h.querySelector('mark')?.textContent ?? null,
      h.querySelector('.rail-tag')?.textContent ?? null,
      h.classList.contains('is-sel'),
    ]);
  const bar = () => [...root.querySelectorAll('.rail-project')].map((r) => r.getAttribute('aria-label'));
  const rightClick = (el: Element) =>
    act(async () => {
      el.dispatchEvent(new MouseEvent('contextmenu', { bubbles: true, cancelable: true, clientX: 30, clientY: 200 }));
    });
  afterEach(() => {
    location.hash = '';
  });

  it('in the Compact look: the magnifier swaps "HQC" for a field, the list shrinks to the matches, archived ones too', async () => {
    const { app, api } = railApp(true);
    await mount(<ProjectRail app={app} />);
    const head = root.querySelector('.rail-head')!;
    expect(head.querySelector('.rail-title')!.textContent).toBe('HQC');
    expect(head.querySelector('.rail-search')!.getAttribute('aria-label')).toBe('Search projects');

    await click(head.querySelector('.rail-search'));
    // The archived ones came with the lists: nothing more to fetch.
    expect(api.archivedProjects).not.toHaveBeenCalled();
    expect(document.activeElement).toBe(field());
    expect(root.querySelector('.rail-title')).toBeNull();
    expect(root.querySelector('.rail-hint')!.textContent).toBe('↑↓ select · ↵ open · Esc close');
    // Nothing typed: the projects on the bar, no archived one.
    expect(hits().map((h) => h[0])).toEqual(['Personal', 'physics-mp', 'caldavllm_bot', 'medication_bot', 'kiln']);

    await typeQuery('BOT');
    expect(hits()).toEqual([
      ['caldavllm_bot', 'bot', null, true],
      ['medication_bot', 'bot', null, false],
      ['99bot_ng', 'bot', 'archived', false],
    ]);
    expect(root.querySelector('.rail-hit.is-archived .rail-name')!.textContent).toBe('99bot_ng');
    expect(root.querySelector('.rail-find-count')!.textContent).toBe('3 of 6');
    // ↓ and ↑ go round; Enter opens the one they stand on and closes the search.
    await pressKey('ArrowDown');
    await pressKey('ArrowDown');
    expect(hits().map((h) => h[3])).toEqual([false, false, true]);
    await pressKey('ArrowDown');
    await pressKey('ArrowUp');
    await pressKey('ArrowUp');
    expect(hits().map((h) => h[3])).toEqual([false, true, false]);
    await pressKey('Enter');
    expect(location.hash).toBe('#/p/p3');
    expect(field()).toBeNull();
    expect(root.querySelector('.rail-title')!.textContent).toBe('HQC');
    expect(bar()).toEqual(['Personal', 'physics-mp', 'caldavllm_bot', 'medication_bot', 'kiln']);

    // Ctrl+K (not on a Mac) opens it from anywhere; no match says so; Esc puts the title and the
    // list back, the focus on the magnifier.
    await act(async () => {
      document.body.dispatchEvent(new KeyboardEvent('keydown', { key: 'k', ctrlKey: true, bubbles: true, cancelable: true }));
    });
    expect(document.activeElement).toBe(field());
    await typeQuery('zzz');
    expect(hits()).toEqual([]);
    expect(root.querySelector('.rail-hits-none')!.textContent).toBe('No projects match');
    await pressKey('Escape');
    expect(field()).toBeNull();
    expect(document.activeElement).toBe(root.querySelector('.rail-search'));
    expect(bar()).toHaveLength(5);
    // Again (a capital K too); a press outside the bar closes it.
    await act(async () => {
      document.body.dispatchEvent(new KeyboardEvent('keydown', { key: 'K', ctrlKey: true, bubbles: true, cancelable: true }));
    });
    expect(field()).not.toBeNull();
    await act(async () => {
      document.body.dispatchEvent(new PointerEvent('pointerdown', { bubbles: true }));
    });
    expect(field()).toBeNull();
    // Esc on a match (a click or its closed menu leaves the focus there) closes it as well.
    await click(root.querySelector('.rail-search'));
    await typeQuery('bot');
    await act(async () => root.querySelector<HTMLElement>('.rail-hit')!.focus());
    await pressKey('Escape');
    expect(field()).toBeNull();
    expect(document.activeElement).toBe(root.querySelector('.rail-search'));
  });

  it('its shortcut: ⌘K on a Mac (there Ctrl+K deletes to the end of the line), Ctrl+K elsewhere', async () => {
    const { app } = railApp(true);
    await mount(<ProjectRail app={app} />);
    const press = (mod: 'metaKey' | 'ctrlKey') =>
      act(async () => {
        document.body.dispatchEvent(new KeyboardEvent('keydown', { key: 'k', [mod]: true, bubbles: true, cancelable: true }));
      });
    const platform = vi.spyOn(navigator, 'platform', 'get');
    try {
      platform.mockReturnValue('Linux x86_64');
      await press('metaKey');
      expect(field()).toBeNull();
      await press('ctrlKey');
      expect(field()).not.toBeNull();
      await pressKey('Escape');
      expect(root.querySelector('.rail-search')!.getAttribute('title')).toBe('Search projects (Ctrl+K)');

      platform.mockReturnValue('MacIntel');
      await press('ctrlKey');
      expect(field()).toBeNull();
      await press('metaKey');
      expect(field()).not.toBeNull();
      await pressKey('Escape');
      await act(async () => app.setDevice({}));
      expect(root.querySelector('.rail-search')!.getAttribute('title')).toBe('Search projects (⌘K)');
    } finally {
      platform.mockRestore();
    }
  });

  it('"+" and the order menu pressed while the search is open: the search closes first', async () => {
    const { app } = railApp(true);
    await mount(<ProjectRail app={app} />);
    // "+" asks for the name; no answer, no project.
    const before = globalThis.prompt;
    const ask = vi.fn(() => null);
    globalThis.prompt = ask;
    try {
      await click(root.querySelector('.rail-search'));
      await typeQuery('bot');
      await click(root.querySelector('.rail-new'));
      expect(ask).toHaveBeenCalledTimes(1);
      expect(field()).toBeNull();
    } finally {
      globalThis.prompt = before;
    }
    await click(root.querySelector('.rail-search'));
    await typeQuery('bot');
    await click(root.querySelector('.rail-order'));
    expect(field()).toBeNull();
    await click(root.querySelector('.menu .btn-reorder-projects'));
    expect(root.querySelector('.rail-done')).not.toBeNull();
    expect(bar()).toHaveLength(5);
  });

  it('with tiles: the magnifier under the mark opens the bar over the column, with the names', async () => {
    const { app } = railApp(false);
    await mount(<ProjectRail app={app} />);
    const head = root.querySelector('.rail-head')!;
    expect([...head.children].map((c) => c.getAttribute('aria-label') ?? c.getAttribute('title'))).toEqual(['HQC', 'Search projects']);
    expect(root.querySelector('.rail-pop')).toBeNull();
    await click(head.querySelector('.rail-search'));
    const pop = root.querySelector('.rail-pop')!;
    expect(pop.contains(field())).toBe(true);
    // The tiles stay under it.
    expect(bar()).toHaveLength(5);
    await typeQuery('bot');
    expect([...pop.querySelectorAll('.rail-hit .rail-name')].map((n) => n.textContent)).toEqual(['caldavllm_bot', 'medication_bot', '99bot_ng']);
    expect(pop.querySelector('.rail-hint')).not.toBeNull();
    // A match shows its name: its menu has no line over the items.
    await rightClick(pop.querySelector('.rail-hit')!);
    expect(root.querySelector('.ctx-menu')).not.toBeNull();
    expect(root.querySelector('.ctx-menu .menu-head')).toBeNull();
    await pressKey('Escape');
    expect(root.querySelector('.ctx-menu')).toBeNull();
    expect(root.querySelector('.rail-pop')).not.toBeNull();
    // A click on a match opens it.
    const second = pop.querySelectorAll<HTMLAnchorElement>('.rail-hit')[1];
    expect(second.getAttribute('href')).toBe('#/p/p3');
    await click(second);
    expect(root.querySelector('.rail-pop')).toBeNull();
    await click(root.querySelector('.rail-search'));
    await pressKey('Escape');
    expect(root.querySelector('.rail-pop')).toBeNull();
  });

  it("a project's right click: the column's menu, its name first on a tile; Archive project goes at once, Undo brings it back", async () => {
    vi.useFakeTimers({ toFake: ['Date', 'setTimeout', 'clearTimeout'] });
    vi.setSystemTime(NOW);
    const { app, api } = railApp(false);
    await mount(<ProjectRail app={app} />);
    await rightClick(root.querySelector('.rail-project[aria-label="kiln"]')!);
    const menu = root.querySelector('.ctx-menu')!;
    expect(menu.getAttribute('aria-label')).toBe('Project «kiln»');
    expect(menu.querySelector('.menu-head')!.textContent).toBe('kiln');
    expect(menuItems()).toEqual(['Open in new tab', 'Add to favorites', 'Project settings…', 'Archive project', 'Delete project…']);
    // Nothing but the items: no button beside the menu.
    expect([...menu.children].every((c) => c.matches('.menu-head, .menu-item, .menu-sep'))).toBe(true);
    expect(root.querySelector('.rail-project[aria-label="kiln"]')!.classList.contains('is-menu')).toBe(true);

    await click(menu.querySelector('.btn-archive-project'));
    expect(root.querySelector('.ctx-menu')).toBeNull();
    expect(bar()).toEqual(['Personal', 'physics-mp', 'caldavllm_bot', 'medication_bot']);
    await settle();
    expect(api.archiveProject).toHaveBeenCalledWith('p4');
    const toast = root.querySelector('.toast-project')!;
    expect(toast.querySelector('.toast-text')!.textContent).toBe('Archived «kiln» · find it with the search on the bar');
    await click(toast.querySelector('.btn-undo'));
    await settle();
    expect(api.unarchiveProject).toHaveBeenCalledWith('p4');
    expect(root.querySelector('.toast-project')).toBeNull();
    expect(bar()).toEqual(['Personal', 'physics-mp', 'caldavllm_bot', 'medication_bot', 'kiln']);
    expect(app.archivedProjects.has('p4')).toBe(false);

    // Again, and the toast runs out: it stays archived, the search finds it.
    await rightClick(root.querySelector('.rail-project[aria-label="kiln"]')!);
    await click(root.querySelector('.ctx-menu .btn-archive-project'));
    await settle();
    await act(async () => {
      vi.advanceTimersByTime(ARCHIVE_UNDO_MS);
    });
    expect(root.querySelector('.toast-project')).toBeNull();
    expect(api.unarchiveProject).toHaveBeenCalledTimes(1);
    expect(app.archivedProjects.get('p4')!.archived_at).toBe(at(0));

    // The personal project is neither archived nor deleted.
    await rightClick(root.querySelector('.rail-project[aria-label="Personal"]')!);
    expect(menuItems()).toEqual(['Open in new tab', 'Add to favorites', 'Project settings…']);
  });

  it('an archived project found by the search: opened, the column keeps it; its menu restores it', async () => {
    const { app, api } = railApp(true);
    await mount(
      <>
        <ProjectRail app={app} />
        <SessionList app={app} />
      </>,
    );
    await click(root.querySelector('.rail-search'));
    await settle();
    await typeQuery('99');
    await rightClick(root.querySelector('.rail-hit.is-archived')!);
    expect(menuItems()).toEqual(['Open in new tab', 'Add to favorites', 'Project settings…', 'Restore', 'Delete project…']);
    // A line over the items only for a tile: the names are on here.
    expect(root.querySelector('.ctx-menu .menu-head')).toBeNull();
    await pressKey('Escape');
    expect(field()).not.toBeNull();

    // Opened (App selects the project of the address): the column's, and its "⋯" offers
    // Restore; a refresh of the lists keeps it open.
    await act(async () => field()!.focus());
    await pressKey('Enter');
    expect(location.hash).toBe('#/p/p9');
    await act(async () => app.selectProject('p9'));
    expect(root.querySelector('.side-head .project-title')!.textContent).toBe('99bot_ng');
    await act(async () => app.refreshSessions());
    expect(app.currentProjectId).toBe('p9');
    await click(root.querySelector('.side-head .btn-project-menu'));
    expect(menuItems()).toContain('Restore');
    expect(menuItems()).not.toContain('Archive project');
    await click(root.querySelector('.ctx-menu .btn-restore-project'));
    await settle();
    expect(api.unarchiveProject).toHaveBeenCalledWith('p9');
    expect(app.archivedProjects.has('p9')).toBe(false);
    expect(bar()).toEqual(['Personal', 'physics-mp', 'caldavllm_bot', 'medication_bot', 'kiln', '99bot_ng']);
    expect(root.querySelector('.rail-project.is-current')!.getAttribute('aria-label')).toBe('99bot_ng');

    // Archived elsewhere, the open project stays open, archived; deleted, it gives its place to
    // the personal one at the next refresh.
    await api.archiveProject('p9');
    await act(async () => app.refreshSessions());
    expect(app.currentProjectId).toBe('p9');
    expect(app.archivedProjects.has('p9')).toBe(true);
    api.archivedProjects.mockResolvedValueOnce([]);
    await act(async () => app.refreshSessions());
    expect(app.currentProjectId).toBe('p0');
  });

  it("archiving the open project from the column's \"⋯\" opens the personal one", async () => {
    const { app, api } = railApp(true);
    await mount(
      <>
        <ProjectRail app={app} />
        <SessionList app={app} />
      </>,
    );
    expect(app.currentProjectId).toBe('p1');
    await click(root.querySelector('.side-head .btn-project-menu'));
    await click(root.querySelector('.ctx-menu .btn-archive-project'));
    await settle();
    expect(api.archiveProject).toHaveBeenCalledWith('p1');
    expect(app.currentProjectId).toBe('p0');
    expect(location.hash).toBe('#/p/p0');
    expect(bar()).not.toContain('physics-mp');
    expect(root.querySelector('.toast-project .toast-text')!.textContent).toBe('Archived «physics-mp» · find it with the search on the bar');
    // Its sessions are as they were.
    expect(app.sessions.get('net')!.archived).toBe(false);
  });

  it('a refused archive puts the project back with a notice', async () => {
    const { app, api } = railApp(false);
    api.archiveProject.mockRejectedValueOnce(new Error('hub unavailable'));
    await mount(
      <>
        <ProjectRail app={app} />
        <SessionList app={app} />
      </>,
    );
    await rightClick(root.querySelector('.rail-project[aria-label="kiln"]')!);
    await click(root.querySelector('.ctx-menu .btn-archive-project'));
    await settle();
    expect(bar()).toContain('kiln');
    expect(root.querySelector('.toast-project')).toBeNull();
    expect(root.querySelector('.notice')!.textContent).toBe('Not archived «kiln»: hub unavailable');
  });

  it('an archived project is known without the search: refreshes, a reload, its settings, its sessions', async () => {
    const api = {
      ...archiveApi(),
      allSessions: vi.fn(async () => [...LIST, sess('bot1', 'Bot run', 'idle', at(1), { project_id: 'p9' })]),
      workers: vi.fn(async () => []),
      patchProject: vi.fn(async (id: string, patch: { default_workspace_id?: string }) => ({ ...BOT, id, ...patch })),
    };
    const socket = { onState: () => () => {} } as unknown as HubSocket;
    const app = new AppState(api as unknown as Api, socket);
    app.me = { id: 'u', email: 'who@example.com', personal_project_id: 'p0', theme: 'warm', look: 'spacious', project_sort: 'custom' };
    // The lists bring it, the search never opened.
    await act(async () => app.refreshSessions());
    expect(api.archivedProjects).toHaveBeenCalledTimes(1);
    expect([...app.archivedProjects.keys()]).toEqual(['p9']);
    expect(app.projects.has('p9')).toBe(false);
    // Opened from its address (App selects it), or from one of its sessions: a refresh keeps it.
    app.selectProject('p9');
    await act(async () => app.refreshSessions());
    expect(app.currentProjectId).toBe('p9');
    expect(app.findSession('99bot_ng/bot1')).toEqual({ id: 'bot1', address: '99bot_ng/bot1', title: 'Bot run' });
    expect(app.findSession('bot1')?.address).toBe('99bot_ng/bot1');
    // A reload has it from the kept list before the hub answers.
    const again = new AppState(api as unknown as Api, socket);
    expect(again.project('p9')?.name).toBe('99bot_ng');
    expect(again.projects.has('p9')).toBe(false);

    // Its settings page is whole: the name, Appearance and Delete.
    await mount(<ProjectSettings app={app} projectId="p9" />);
    expect(root.textContent).toContain('99bot_ng');
    expect(root.querySelector('.btn-appearance')).not.toBeNull();
    expect(root.querySelector('.btn-delete-project')).not.toBeNull();
    // A change made there keeps it off the bar ("Make default" answers the archived project).
    await act(async () => app.setDefaultWorkspace('p9', 'w9'));
    expect(app.projects.has('p9')).toBe(false);
    expect(app.archivedProjects.get('p9')!.default_workspace_id).toBe('w9');
    // A workspace added or removed there: its new default comes from the archived list.
    const lists = api.projects.mock.calls.length;
    const archivedLists = api.archivedProjects.mock.calls.length;
    api.archivedProjects.mockResolvedValueOnce([{ ...BOT, default_workspace_id: 'w7' }]);
    await act(async () => app.workspacesChanged('p9'));
    expect(api.archivedProjects).toHaveBeenCalledTimes(archivedLists + 1);
    expect(api.projects).toHaveBeenCalledTimes(lists);
    expect(app.archivedProjects.get('p9')!.default_workspace_id).toBe('w7');
    expect(app.projects.has('p9')).toBe(false);
  });

  it('Undo while the archive is on its way: the hub gets the archive first, then the restore', async () => {
    const { app, api } = railApp(false);
    let release!: () => void;
    const latch = new Promise<void>((r) => (release = r));
    const archive = api.archiveProject.getMockImplementation()!;
    api.archiveProject.mockImplementationOnce(async (id: string) => {
      await latch;
      return archive(id);
    });
    await mount(<ProjectRail app={app} />);
    await rightClick(root.querySelector('.rail-project[aria-label="kiln"]')!);
    await click(root.querySelector('.ctx-menu .btn-archive-project'));
    expect(bar()).not.toContain('kiln');
    await click(root.querySelector('.toast-project .btn-undo'));
    await settle();
    expect(api.archiveProject).toHaveBeenCalledWith('p4');
    expect(api.unarchiveProject).not.toHaveBeenCalled();
    release();
    await settle();
    await settle();
    expect(api.unarchiveProject).toHaveBeenCalledWith('p4');
    expect(api.archiveProject.mock.invocationCallOrder[0]).toBeLessThan(api.unarchiveProject.mock.invocationCallOrder[0]);
    expect(bar()).toEqual(['Personal', 'physics-mp', 'caldavllm_bot', 'medication_bot', 'kiln']);
  });

  it('a refused archive puts the project back in its place on the bar, not last', async () => {
    const { app, api } = railApp(true);
    api.archiveProject.mockRejectedValueOnce(new Error('hub unavailable'));
    await mount(<ProjectRail app={app} />);
    const before = bar();
    expect(before.indexOf('caldavllm_bot')).toBe(2);
    await rightClick(root.querySelector('.rail-project[aria-label="caldavllm_bot"]')!);
    await click(root.querySelector('.ctx-menu .btn-archive-project'));
    await settle();
    expect(app.listNotice).toBe('Not archived «caldavllm_bot»: hub unavailable');
    expect(bar()).toEqual(before);
  });

  it('two refused archives in a row put both back in their places, by the hub\'s order', async () => {
    const { app, api } = railApp(true);
    await act(() => app.refreshSessions());
    let refuse!: () => void;
    const gate = new Promise<void>((r) => (refuse = r));
    api.archiveProject.mockImplementationOnce(async () => {
      await gate;
      throw new Error('hub unavailable');
    });
    api.archiveProject.mockRejectedValueOnce(new Error('hub unavailable'));
    await mount(<ProjectRail app={app} />);
    const before = bar();
    expect(before.slice(2, 4)).toEqual(['caldavllm_bot', 'medication_bot']);
    // The second archive takes its snapshot of the bar while the first is off it.
    app.archiveProject('p2');
    app.archiveProject('p3');
    refuse();
    await settle();
    expect(bar()).toEqual(before);
  });

  it('a project created here, archived right after an older one, both refused: the bar as it was', async () => {
    const { app, api } = railApp(true);
    await act(() => app.refreshSessions());
    await act(async () => {
      await app.createProject('glaze');
    });
    let refuse!: () => void;
    const gate = new Promise<void>((r) => (refuse = r));
    api.archiveProject.mockImplementationOnce(async () => {
      await gate;
      throw new Error('hub unavailable');
    });
    api.archiveProject.mockRejectedValueOnce(new Error('hub unavailable'));
    await mount(<ProjectRail app={app} />);
    const before = bar();
    expect(before.slice(-2)).toEqual(['kiln', 'glaze']);
    app.archiveProject('p3');
    app.archiveProject('p5');
    refuse();
    await settle();
    expect(bar()).toEqual(before);
  });

  it("a project the hub's list put on the bar here, archived right after an older one, both refused: the bar as it was", async () => {
    const { app, api } = railApp(true);
    await act(() => app.refreshSessions());
    // A session moved into a new project: the client learns the project from the hub's list.
    api.projects.mockResolvedValueOnce([...ALL.slice(0, 5), project('p5', 'glaze')]);
    await act(async () => {
      await app.moveSession('net', { path: '/Users/me/work/glaze', create: true });
    });
    let refuse!: () => void;
    const gate = new Promise<void>((r) => (refuse = r));
    api.archiveProject.mockImplementationOnce(async () => {
      await gate;
      throw new Error('hub unavailable');
    });
    api.archiveProject.mockRejectedValueOnce(new Error('hub unavailable'));
    await mount(<ProjectRail app={app} />);
    const before = bar();
    expect(before.slice(-2)).toEqual(['kiln', 'glaze']);
    app.archiveProject('p3');
    app.archiveProject('p5');
    refuse();
    await settle();
    expect(bar()).toEqual(before);
  });

  it('an archive the hub answers after a list read before it: the project stays off the bar', async () => {
    const { app, api } = railApp(true);
    let answer!: () => void;
    const gate = new Promise<void>((r) => (answer = r));
    const archive = api.archiveProject.getMockImplementation()!;
    api.archiveProject.mockImplementationOnce(async (id: string) => {
      await gate;
      return archive(id);
    });
    app.archiveProject('p4');
    expect(app.projects.has('p4')).toBe(false);
    // The lists are read before the hub stored the archive: kiln is still among the live ones.
    await act(async () => app.refreshSessions());
    expect(api.projects).toHaveBeenCalled();
    expect(app.projects.has('p4')).toBe(false);
    expect(app.archivedProjects.has('p4')).toBe(true);
    expect(app.projectUndo).toBe('p4');
    answer();
    await settle();
    expect(app.projects.has('p4')).toBe(false);
    expect(app.archivedProjects.get('p4')!.archived_at).toBe(at(0));

    // A restore's list, read before an archive that waits behind it: that one stays off the bar.
    let restored!: () => void;
    const back = new Promise<void>((r) => (restored = r));
    const unarchive = api.unarchiveProject.getMockImplementation()!;
    api.unarchiveProject.mockImplementationOnce(async (id: string) => {
      await back;
      return unarchive(id);
    });
    let stored!: () => void;
    const later = new Promise<void>((r) => (stored = r));
    api.archiveProject.mockImplementationOnce(async (id: string) => {
      await later;
      return archive(id);
    });
    const restore = app.restoreProject('p9');
    app.archiveProject('p3');
    expect(app.projects.has('p3')).toBe(false);
    restored();
    await act(async () => restore);
    expect(app.projects.has('p9')).toBe(true);
    expect(app.projects.has('p3')).toBe(false);
    stored();
    await settle();
    expect(api.archiveProject).toHaveBeenLastCalledWith('p3');
    expect(app.projects.has('p3')).toBe(false);
    expect(app.archivedProjects.has('p3')).toBe(true);
  });

  it('a list read before the hub stored an archive or a restore is read again', async () => {
    const { app, api } = railApp(true);
    // The sessions' list of the next refresh waits for `land`.
    const holdLists = () => {
      let land!: () => void;
      const gate = new Promise<void>((r) => (land = r));
      api.allSessions.mockImplementationOnce(async () => {
        await gate;
        return LIST;
      });
      return land;
    };
    const lists = () => api.projects.mock.calls.length;
    let answer!: () => void;
    const stored = new Promise<void>((r) => (answer = r));
    const archive = api.archiveProject.getMockImplementation()!;
    api.archiveProject.mockImplementationOnce(async (id: string) => {
      await stored;
      return archive(id);
    });
    app.archiveProject('p4');
    let land = holdLists();
    const before = lists();
    let refresh = app.refreshSessions();
    // kiln is still among the projects the hub listed.
    expect(lists()).toBe(before + 1);
    answer();
    await settle();
    land();
    await act(async () => {
      await refresh;
    });
    expect(lists()).toBe(before + 2);
    expect(app.projects.has('p4')).toBe(false);
    expect(app.archivedProjects.has('p4')).toBe(true);

    // A restore the hub stores while a list read before it is on its way.
    land = holdLists();
    refresh = app.refreshSessions();
    await act(() => app.restoreProject('p9'));
    land();
    await act(async () => {
      await refresh;
    });
    expect(app.projects.has('p9')).toBe(true);
    expect(app.archivedProjects.has('p9')).toBe(false);
  });
});

// SPEC §6: a session row or a project tile loads the newest page of its chat before the click.
describe('loading a chat ahead', () => {
  const answer = parseEvent({
    session_id: 'net',
    event_id: '0199f3a2-7c10-7000-8000-000000000103',
    seq: 1,
    ts: at(0.2),
    author: { kind: 'agent', id: 'net' },
    type: 'agent.message',
    payload: { text: 'Synced.', final: true },
  });
  function chatApp() {
    const api = {
      ...hubApi(),
      eventsBefore: vi.fn(async (_id: string, _before?: number, _limit?: number) => ({ events: [answer], invalid: [], maxSeq: 1, minSeq: 1, count: 1 })),
      readCursor: vi.fn(async () => {}),
    };
    const app = makeApp(
      LIST.map((s) => (s.id === 'net' ? { ...s, unread: 1 } : s)),
      api,
    );
    const subscribe = vi.fn(() => () => {});
    Object.assign(app.socket, { subscribe });
    return { app, api, subscribe };
  }
  const link = (title: string) => rowOf(title).querySelector<HTMLElement>('a.session-row')!;
  const wait = (ms: number) => act(async () => void (await new Promise((r) => setTimeout(r, ms))));
  const pointer = (type: string, pointerType = 'mouse') => new PointerEvent(type, { bubbles: type === 'pointerdown', pointerType });

  it('a row the mouse rests on, or pressed, loads the newest page once; its chat then opens without "Loading…"', async () => {
    const { app, api } = chatApp();
    await mount(<SessionList app={app} />);
    // Passing over a row loads nothing.
    link('Stone textures').dispatchEvent(pointer('pointerenter'));
    await wait(20);
    link('Stone textures').dispatchEvent(pointer('pointerleave'));
    await wait(80);
    expect(api.eventsBefore).not.toHaveBeenCalled();

    link('Network sync').dispatchEvent(pointer('pointerenter'));
    await wait(80);
    expect(api.eventsBefore.mock.calls).toEqual([['net', undefined, 100]]);
    link('Network sync').dispatchEvent(pointer('pointerdown'));
    await wait(10);
    expect(api.eventsBefore).toHaveBeenCalledTimes(1);
    // A press loads at once.
    link('Cave scene port').dispatchEvent(pointer('pointerdown'));
    expect(api.eventsBefore).toHaveBeenLastCalledWith('cave', undefined, 100);

    render(null, root);
    root = document.createElement('div');
    document.body.appendChild(root);
    // No act: the frame before any effect has run.
    render(<Chat app={app} sessionId="net" />, root);
    expect(root.textContent).not.toContain('Loading…');
    expect(root.querySelector('.feed')!.textContent).toContain('Synced.');
    await wait(100);
    // Fetched once more behind the open, for the fields changed since (`Timeline.refresh`).
    expect(api.eventsBefore.mock.calls.map((c) => c[0])).toEqual(['net', 'cave', 'net']);
  });

  it('loading ahead neither subscribes nor moves the read cursor', async () => {
    const { app, api, subscribe } = chatApp();
    await mount(<SessionList app={app} />);
    link('Network sync').dispatchEvent(pointer('pointerdown'));
    await wait(80);
    expect(api.eventsBefore).toHaveBeenCalledTimes(1);
    expect(subscribe).not.toHaveBeenCalled();
    expect(api.readCursor).not.toHaveBeenCalled();
    expect(rowOf('Network sync').querySelector('.badge')!.textContent).toBe('1');
  });

  it('a touch loads nothing ahead: on a phone it may start a scroll', async () => {
    const { app, api } = chatApp();
    await mount(<SessionList app={app} phone />);
    link('Network sync').dispatchEvent(pointer('pointerenter', 'touch'));
    link('Network sync').dispatchEvent(pointer('pointerdown', 'touch'));
    await wait(80);
    expect(api.eventsBefore).not.toHaveBeenCalled();
  });

  it('a project tile loads the session the project reopens', async () => {
    const { app, api } = chatApp();
    localStorage.setItem('hqc.lastSession', JSON.stringify({ p1: 'net' }));
    await mount(<ProjectRail app={app} />);
    const tile = (name: string) => root.querySelector<HTMLElement>(`a.project-row[aria-label="${name}"]`)!;
    // Nothing remembered for the personal project: nothing to load.
    tile('Personal').dispatchEvent(pointer('pointerdown'));
    tile('physics-mp').dispatchEvent(pointer('pointerenter'));
    await wait(80);
    expect(api.eventsBefore.mock.calls).toEqual([['net', undefined, 100]]);
  });
});

describe('SessionList (phone)', () => {
  it('a project\'s screen: one header row — "‹", the name, search, State, "⋯", "+" — then the groups; no chips, gear or floating "New"', async () => {
    await mount(<SessionList app={makeApp()} phone />);
    const head = root.querySelector('.sessions-phone > .side-head')!;
    expect([...head.children].map((c) => c.getAttribute('aria-label') ?? c.textContent)).toEqual([
      'Back to projects',
      'physics-mp',
      'Search sessions and messages',
      'Session order: State',
      'Project: settings, favorites, archive',
      'New session',
    ]);
    expect(head.querySelector('.btn-back')!.getAttribute('href')).toBe('#/');
    // The search is a screen of its own (T33, tests/search.test.tsx).
    expect(head.querySelector('.btn-search')!.getAttribute('href')).toBe('#/search');
    expect(groups().map((g) => g[0])).toEqual(['Needs you', 'Running', 'Idle · Yesterday', 'Idle · Last 7 days']);
    // Gone: the project chips, the gear, the order switch, the floating "New" and the workers
    // (on the first screen now); the desktop's workers line and width handle are not here.
    expect(root.querySelector('.chips, .m-head, a.btn-project-settings, .seg-group, .fab, .worker-list, .wline, .col-grip')).toBeNull();
    // Two lines a row, as in the desktop's Spacious column: the title and the state.
    expect(rowOf('Network sync').querySelector('.session-sub')!.textContent).toBe('Waiting for reply');
    expect(root.querySelector('.session-where')).toBeNull();
    expect(root.querySelector('.session-item button')).toBeNull();
    expect(root.querySelector('.list-hint')!.textContent).toBe('Press and hold a session to archive, rename or delete it.');
  });

  it('"⋯" is the project\'s menu — settings, favorites, archive — and a tap beside it only closes it; "+" opens a new session', async () => {
    const api = {
      ...hubApi(),
      archiveProject: vi.fn(async (id: string) => app.archivedProjects.get(id)!),
    };
    const app = makeApp(LIST, api as unknown as ReturnType<typeof hubApi>);
    await mount(<SessionList app={app} phone />);
    await click(root.querySelector('.side-head .btn-project-menu'));
    expect(menuItems()).toEqual(['Open in new tab', 'Add to favorites', 'Project settings…', 'Archive project', 'Delete project…']);
    expect(root.querySelector('.menu .btn-project-settings')!.getAttribute('href')).toBe('#/p/p1/settings');
    expect(root.querySelector('.menu-scrim')).not.toBeNull();
    await click(root.querySelector('.menu .btn-favorite'));
    expect(api.setProjectFavorite).toHaveBeenCalledWith('p1', true);
    await click(root.querySelector('.side-head .btn-project-menu'));
    await click(root.querySelector('.menu .btn-archive-project'));
    expect(api.archiveProject).toHaveBeenCalledWith('p1');
    // A phone goes to the projects screen, where its Undo is.
    expect(location.hash).toBe('#/');
    // The order menu too has the layer.
    await click(root.querySelector('.side-head .btn-mode'));
    expect(root.querySelector('.menu-scrim')).not.toBeNull();
    await pressKey('Escape');
    await click(root.querySelector('.side-head .btn-new-session'));
    expect(location.hash).toBe('#/p/p0/new');
    location.hash = '';
  });

  it('in the Compact look its rows are one line, the subagents after the title, no tooltip', async () => {
    const app = makeApp(LIST.map((s) => (s.id === 'stones' ? { ...s, subagents: 2 } : s)));
    app.me = { ...app.me!, look: 'compact' };
    await mount(<SessionList app={app} phone />);
    const rows = [...root.querySelectorAll('.session-row')];
    expect(rows.length).toBeGreaterThan(0);
    expect(rows.filter((r) => r.hasAttribute('title'))).toEqual([]);
    expect(rowOf('Stone textures').querySelector('.session-top .session-ag')!.textContent).toBe('2 agents');
    // Spacious keeps it on the state line.
    app.me = { ...app.me!, look: 'spacious' };
    await mount(<SessionList app={app} phone />);
    expect(rowOf('Stone textures').querySelector('.session-ag')).toBeNull();
  });

  it('a long press opens the same menu at the press point: Archive with Undo, Delete in a second step (T50)', async () => {
    vi.useFakeTimers({ toFake: ['Date', 'setTimeout', 'clearTimeout'] });
    const api = hubApi();
    await mount(<SessionList app={makeApp(LIST, api)} phone />);
    const press = (title: string) =>
      act(async () => {
        rowOf(title).dispatchEvent(new PointerEvent('pointerdown', { bubbles: true, clientX: 30, clientY: 200 }));
      });
    // A short press is a tap.
    await press('Network sync');
    await act(async () => {
      rowOf('Network sync').dispatchEvent(new PointerEvent('pointerup', { bubbles: true }));
    });
    await act(async () => {
      vi.advanceTimersByTime(1000);
    });
    expect(root.querySelector('.ctx-menu')).toBeNull();

    await press('Network sync');
    await act(async () => {
      vi.advanceTimersByTime(450);
    });
    const menu = root.querySelector<HTMLElement>('.ctx-menu')!;
    expect([menu.style.left, menu.style.top]).toEqual(['30px', '200px']);
    expect(rowOf('Network sync').querySelector('.session-row.is-menu')).not.toBeNull();
    expect(menuItems()).toEqual(SESSION_MENU);
    expect(menu.querySelector('.btn-rules')!.getAttribute('href')).toBe('#/p/p1/rules/w1?session=net');
    await click(menu.querySelector('.btn-archive'));
    expect(root.querySelector('.ctx-menu')).toBeNull();
    expect(root.querySelector('.toast-text')!.textContent).toBe('Archived «Network sync»');
    await click(root.querySelector('.btn-undo'));
    expect(titles()).toContain('Network sync');

    // A right click opens it too; Delete asks once more.
    await act(async () => {
      rowOf('Cave scene port').dispatchEvent(new MouseEvent('contextmenu', { bubbles: true, cancelable: true }));
    });
    await click(root.querySelector('.menu .btn-delete'));
    expect(root.querySelector('.menu-note')!.textContent).toBe(
      'Delete this session forever? Messages and history go too. Files on disk stay.',
    );
    await click(root.querySelector('.btn-confirm-delete'));
    expect(api.deleteSession).toHaveBeenCalledWith('cave');
    expect(api.archive).not.toHaveBeenCalled();
  });

  it('a tap beside the menu closes it and does nothing else: a clear layer under the menu takes it', async () => {
    const api = hubApi();
    await mount(<SessionList app={makeApp(LIST, api)} phone />);
    const hash = location.hash;
    await act(async () => {
      rowOf('Network sync').dispatchEvent(new MouseEvent('contextmenu', { bubbles: true, cancelable: true, clientX: 30, clientY: 200 }));
    });
    const scrim = root.querySelector<HTMLElement>('.menu-scrim')!;
    expect(scrim).not.toBeNull();
    // The click that ends the long press which opened the menu did not start on the layer.
    await act(async () => {
      scrim.dispatchEvent(new MouseEvent('click', { bubbles: true, cancelable: true }));
    });
    expect(root.querySelector('.ctx-menu')).not.toBeNull();
    // A tap on it: the press keeps the menu (the layer stays to take the click), the click
    // closes it; nothing under it is opened or pressed.
    await act(async () => {
      scrim.dispatchEvent(new PointerEvent('pointerdown', { bubbles: true }));
    });
    expect(root.querySelector('.ctx-menu')).not.toBeNull();
    const tap = new MouseEvent('click', { bubbles: true, cancelable: true });
    await act(async () => {
      scrim.dispatchEvent(tap);
    });
    expect(tap.defaultPrevented).toBe(true);
    expect(root.querySelector('.ctx-menu, .menu-scrim')).toBeNull();
    expect(location.hash).toBe(hash);
    expect(api.archive).not.toHaveBeenCalled();
    // A desktop's menu has no such layer: a press away goes where it was aimed.
    render(null, root);
    await mount(<SessionList app={makeApp()} />);
    await menuOf('Network sync');
    expect(root.querySelector('.ctx-menu')).not.toBeNull();
    expect(root.querySelector('.menu-scrim')).toBeNull();
  });

  it('Reorder, from the order menu, puts the handle on the right of the row', async () => {
    await mount(<SessionList app={makeApp()} phone />);
    await startReorder();
    const item = rowOf('Network sync');
    expect(item.lastElementChild!.classList.contains('grip')).toBe(true);
    expect(root.querySelector('.side-head + .side-tools .reorder-bar')).not.toBeNull();
    expect(root.querySelector('.list-hint')!.textContent).toBe('Drag the handle on the right. Done saves the order and switches to Custom.');
  });
});

describe('favorites', () => {
  const favoriteTitles = () => [...root.querySelectorAll('.group-favorites .session-title')].map((t) => t.textContent);
  const rowIn = (sel: string, title: string) =>
    [...root.querySelectorAll<HTMLElement>(`${sel} .session-item`)].find((r) => r.querySelector('.session-title')!.textContent === title)!;
  const listed = (title: string) => rowIn('.session-group:not(.group-favorites)', title);
  const starred = (title: string) => rowIn('.group-favorites', title);
  /** The row's menu, by a right click on it. */
  const rightClick = (row: HTMLElement) =>
    act(async () => {
      row.dispatchEvent(new MouseEvent('contextmenu', { bubbles: true, cancelable: true }));
    });

  it("lists the project's favorites above the groups, the newest first; a click opens one", async () => {
    const app = makeApp([
      ...LIST.map((s) => (s.id === 'cave' ? { ...s, favorite_at: at(30) } : s.id === 'prof' ? { ...s, favorite_at: at(1) } : s)),
      sess('plan', 'Migration plan', 'idle', at(50), { project_id: 'p0', favorite_at: at(0.1) }),
      sess('old', 'Old SPI experiment', 'archived', at(70), { favorite_at: at(0.5) }),
    ]);
    await mount(<SessionList app={app} current="prof" />);
    // Archived favorites stay in the archive; another project's stay in that project.
    expect(groups()[0]).toEqual(['Favorites', '2', ['Collision profiling', 'Cave scene port']]);
    const rows = [...root.querySelectorAll<HTMLElement>('.group-favorites .session-item')];
    expect(rows[0].querySelector('.session-row')!.getAttribute('href')).toBe('#/s/prof');
    expect(rows[0].querySelector('.session-row')!.classList.contains('is-current')).toBe(true);
    // Pinned over the list on a desktop: not in its scroll.
    expect(root.querySelector('.fav-pinned.group-favorites')).not.toBeNull();
    expect(root.querySelector('.session-scroll .group-favorites')).toBeNull();
    expect(root.querySelector('.fav-pinned .group-pin svg')).not.toBeNull();
    // In its own project the other one is listed.
    await act(() => app.selectProject('p0'));
    expect(favoriteTitles()).toEqual(['Migration plan']);
    await act(() => app.selectProject('p1'));
    // Listed once: the project's own groups no longer hold it.
    expect(groups().slice(1).flatMap((g) => g[2])).toEqual(['Network sync', 'Stone textures', 'Step determinism']);
    // Reorder hides them.
    await startReorder();
    expect(root.querySelector('.group-favorites')).toBeNull();
  });

  it("on a phone's project screen too they are pinned over the scrolling list", async () => {
    await mount(<SessionList app={makeApp(LIST.map((s) => (s.id === 'cave' ? { ...s, favorite_at: at(30) } : s)))} phone />);
    expect(root.querySelector('.sessions-phone > .fav-pinned.group-favorites')).not.toBeNull();
    expect(root.querySelector('.session-scroll .group-favorites')).toBeNull();
    expect(root.querySelector('.fav-pinned .group-pin svg')).not.toBeNull();
  });

  it("the row's menu adds a session to the favorites and takes it out; the summary of another device too", async () => {
    const api = hubApi();
    const app = makeApp(LIST, api);
    await mount(<SessionList app={app} />);
    expect(root.querySelector('.group-favorites')).toBeNull();
    await rightClick(listed('Cave scene port'));
    expect(menuItems()[1]).toBe('Add to favorites');
    await click(root.querySelector('.menu .btn-favorite'));
    expect(api.setSessionFavorite).toHaveBeenCalledWith('cave', true);
    expect(favoriteTitles()).toEqual(['Cave scene port']);
    expect(listed('Cave scene port')).toBeUndefined();

    await rightClick(starred('Cave scene port'));
    expect(menuItems()[1]).toBe('Remove from favorites');
    await click(root.querySelector('.menu .btn-favorite'));
    expect(api.setSessionFavorite).toHaveBeenLastCalledWith('cave', false);
    expect(root.querySelector('.group-favorites')).toBeNull();
    expect(listed('Cave scene port')).toBeTruthy();

    // Made a favorite on another device: the hub's summary brings it.
    const net = app.sessions.get('net')!;
    await act(() =>
      app.mergeSummary([
        {
          session_id: 'net',
          project_id: 'p1',
          title: net.title,
          state: net.state,
          queue_len: 0,
          unread: 0,
          last_seq: 1,
          last_read_seq: 1,
          favorite_at: at(0),
        },
      ]),
    );
    expect(favoriteTitles()).toEqual(['Network sync']);
  });

  it('a refusal goes to the notice and changes nothing', async () => {
    const api = hubApi();
    api.setSessionFavorite.mockRejectedValueOnce(new Error('hub unavailable'));
    const app = makeApp(LIST, api);
    await mount(<SessionList app={app} />);
    await rightClick(listed('Cave scene port'));
    await click(root.querySelector('.menu .btn-favorite'));
    expect(root.querySelector('.notice')!.textContent).toBe('Favorites not saved: hub unavailable');
    expect(root.querySelector('.group-favorites')).toBeNull();
  });

  it('a project whose sessions are all favorites is not "No sessions yet"', async () => {
    await mount(<SessionList app={makeApp(LIST.map((s) => ({ ...s, favorite_at: at(1) })))} />);
    expect(groups().map((g) => g[0])).toEqual(['Favorites']);
    expect(root.querySelector('.empty')).toBeNull();
  });

  it("the phone's menu has it too", async () => {
    const api = hubApi();
    await mount(<SessionList app={makeApp(LIST, api)} phone />);
    await rightClick(listed('Network sync'));
    await click(root.querySelector('.menu .btn-favorite'));
    expect(api.setSessionFavorite).toHaveBeenCalledWith('net', true);
    expect(root.querySelector('.ctx-menu')).toBeNull();
    expect(favoriteTitles()).toEqual(['Network sync']);
    await rightClick(starred('Network sync'));
    expect(root.querySelector('.menu .btn-favorite')!.textContent).toBe('Remove from favorites');
  });

  it('the rail: favorite projects first, in the hub\'s order; the context menu of a tile marks one', async () => {
    const api = hubApi();
    const app = makeApp(LIST, api);
    app.projects.set('p2', { id: 'p2', name: 'boards', slug: 'boards', description: '', is_personal: false, color: 'moss', created_at: at(80), favorite_at: at(5) });
    app.projects.set('p3', { id: 'p3', name: 'kiln', slug: 'kiln', description: '', is_personal: false, color: 'coral', created_at: at(70) });
    await mount(<ProjectRail app={app} />);
    const order = () => [...root.querySelectorAll('.rail-project')].map((r) => r.getAttribute('aria-label'));
    expect(order()).toEqual(['boards', 'Personal', 'physics-mp', 'kiln']);

    const tile = (name: string) => root.querySelector<HTMLElement>(`.rail-project[aria-label="${name}"]`)!;
    await act(async () => {
      tile('kiln').dispatchEvent(new MouseEvent('contextmenu', { bubbles: true, cancelable: true, clientX: 30, clientY: 200 }));
    });
    const menu = root.querySelector<HTMLElement>('.ctx-menu')!;
    expect([menu.style.left, menu.style.top]).toEqual(['30px', '200px']);
    expect(menuItems()).toEqual(['Open in new tab', 'Add to favorites', 'Project settings…', 'Archive project', 'Delete project…']);
    await click(root.querySelector('.ctx-menu .btn-favorite'));
    expect(api.setProjectFavorite).toHaveBeenCalledWith('p3', true);
    expect(root.querySelector('.ctx-menu')).toBeNull();
    expect(order()).toEqual(['boards', 'kiln', 'Personal', 'physics-mp']);

    await act(async () => {
      tile('boards').dispatchEvent(new MouseEvent('contextmenu', { bubbles: true, cancelable: true }));
    });
    expect(menuItems()[1]).toBe('Remove from favorites');
    // Esc closes it without a change.
    await act(async () => {
      document.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true }));
    });
    expect(root.querySelector('.ctx-menu')).toBeNull();
    await act(async () => {
      tile('boards').dispatchEvent(new MouseEvent('contextmenu', { bubbles: true, cancelable: true }));
    });
    await click(root.querySelector('.ctx-menu .btn-favorite'));
    expect(api.setProjectFavorite).toHaveBeenLastCalledWith('p2', false);
    expect(order()).toEqual(['kiln', 'Personal', 'physics-mp', 'boards']);
  });
});

describe('the list on a reload', () => {
  const ME = { id: 'u', email: 'who@example.com', personal_project_id: 'p0', theme: 'warm' as const, look: 'spacious' as const, project_sort: 'custom' as const };
  /** A hub that has not answered yet. */
  const pending = () => new Promise<never>(() => {});
  /** A socket that has not connected; `state` plays its connect. */
  function quietSocket() {
    const listeners: ((s: ConnState) => void)[] = [];
    const socket = {
      onState: (fn: (s: ConnState) => void) => (listeners.push(fn), () => {}),
      onRequests: () => () => {},
      onSummary: () => () => {},
      start: () => {},
    } as unknown as HubSocket;
    return { socket, state: (s: ConnState) => listeners.forEach((fn) => fn(s)) };
  }

  it('refreshSessions asks for the projects and every session in one request each', async () => {
    const api = {
      ...hubApi(),
      projects: vi.fn(async () => [...makeApp().projects.values()]),
      sessions: vi.fn(async () => []),
      allSessions: vi.fn(async () => LIST),
    };
    const app = new AppState(api as unknown as Api, quietSocket().socket);
    app.me = ME;
    await app.refreshSessions();
    expect(api.projects).toHaveBeenCalledTimes(1);
    expect(api.allSessions).toHaveBeenCalledTimes(1);
    expect(api.sessions).not.toHaveBeenCalled();
    expect([...app.sessions.keys()]).toEqual(LIST.map((s) => s.id));
  });

  it('a summary pushed while the lists load is not undone by them', async () => {
    // The lists left the hub with the session held in VS Code; it was closed there meanwhile.
    const held = { ...LIST[0], held_by: { entrypoint: 'claude-vscode', status: 'idle' as const } };
    let land!: () => void;
    const api = {
      ...hubApi(),
      projects: vi.fn(async () => [...makeApp().projects.values()]),
      allSessions: vi.fn(() => new Promise<SessionInfo[]>((r) => (land = () => r([held, ...LIST.slice(1)])))),
      session: vi.fn(async (id: string) => ({ ...LIST.find((s) => s.id === id)!, held_by: undefined })),
    };
    const app = makeApp(LIST, api as unknown as ReturnType<typeof hubApi>);
    const refresh = app.refreshSessions();
    await vi.waitFor(() => expect(api.allSessions).toHaveBeenCalled());
    app.mergeSummary([
      {
        session_id: held.id,
        project_id: held.project_id,
        title: held.title,
        state: held.state,
        queue_len: 0,
        unread: 0,
        last_seq: 1,
        last_read_seq: 1,
      },
    ]);
    land();
    await refresh;
    expect(api.session).toHaveBeenCalledWith(held.id);
    await vi.waitFor(() => expect(app.sessions.get(held.id)!.held_by).toBeUndefined());
  });

  it('shows the remembered projects, sessions, order and current project before the hub answers', async () => {
    const mine = LIST.map((s) => ({ ...s, project_id: 'p0' }));
    const before = new AppState(
      {
        ...hubApi({ mode: 'recent', session_ids: [] }),
        projects: async () => [...makeApp().projects.values()],
        allSessions: async () => mine,
        // Per project too, as the list was fetched before `GET /api/sessions`.
        sessions: async (pid: string) => (pid === 'p0' ? mine : []),
      } as unknown as Api,
      quietSocket().socket,
    );
    before.me = ME;
    await before.loadOrder('p0');
    await before.refreshSessions();

    const api = {
      ...hubApi(),
      me: vi.fn(pending),
      projects: vi.fn(pending),
      allSessions: vi.fn(pending),
      sessionOrder: vi.fn(pending),
    };
    const app = new AppState(api as unknown as Api, quietSocket().socket);
    void app.init();
    await mount(
      <>
        <ProjectRail app={app} />
        <SessionList app={app} />
      </>,
    );
    expect(api.me).toHaveBeenCalled();
    expect(app.me).toBeNull();
    expect(app.currentProjectId).toBe('p0');
    expect([...root.querySelectorAll('.rail-project')].map((r) => r.getAttribute('aria-label'))).toEqual(['Personal', 'physics-mp']);
    expect(root.querySelector('.rail-project.is-current')!.getAttribute('aria-label')).toBe('Personal');
    expect(root.querySelector('.project-title')!.textContent).toBe('Personal');
    expect(root.querySelector('.side-head .btn-mode')!.textContent).toBe('Recent');
    expect(groups()).toEqual([
      ['Today', '3', ['Stone textures', 'Step determinism', 'Network sync']],
      ['Yesterday', '1', ['Collision profiling']],
      ['Last 7 days', '1', ['Cave scene port']],
    ]);
  });

  it('with nothing remembered there is no "offline" until the hub and the socket answer', async () => {
    const api = { ...hubApi(), me: vi.fn(pending), projects: vi.fn(pending), allSessions: vi.fn(pending) };
    const { socket, state } = quietSocket();
    const app = new AppState(api as unknown as Api, socket);
    void app.init();
    await mount(<SessionList app={app} />);
    expect(root.querySelector('.project-title')!.textContent).toBe('HQC');
    expect(root.querySelector('.wline')).toBeNull();
    await mount(<SessionList app={app} phone />);
    expect(root.querySelector('.m-head-state')).toBeNull();
    // The first connect is pending: still not offline.
    await act(async () => state('connecting'));
    expect(root.querySelector('.m-head-state')).toBeNull();
    // It failed: offline now.
    await act(async () => state('closed'));
    expect(root.querySelector('.m-head-state')!.textContent).toBe('offline');
    // A desktop says it in the workers line.
    await mount(<SessionList app={app} />);
    expect(root.querySelector('.wline .wline-count')!.textContent).toBe('offline');
    expect(root.querySelector('.wline .dot-offline')).not.toBeNull();
  });

  it("the first frame on a project's address shows that project, not the personal one", async () => {
    const before = new AppState(
      { ...hubApi(), projects: async () => [...makeApp().projects.values()], allSessions: async () => LIST } as unknown as Api,
      quietSocket().socket,
    );
    before.me = ME;
    await before.refreshSessions();

    const api = { ...hubApi(), me: vi.fn(pending), projects: vi.fn(pending), allSessions: vi.fn(pending), sessionOrder: vi.fn(pending) };
    const app = new AppState(api as unknown as Api, quietSocket().socket);
    location.hash = '#/p/p1';
    try {
      root = document.createElement('div');
      document.body.appendChild(root);
      // No act: the frame before any effect has run.
      render(<App app={app} />, root);
      expect(root.querySelector('.rail-project.is-current')!.getAttribute('aria-label')).toBe('physics-mp');
    } finally {
      location.hash = '';
    }
  });

  it('does not write back a kept list order the hub has not given on this load', async () => {
    const before = new AppState(
      {
        ...hubApi({ mode: 'recent', session_ids: [] }),
        projects: async () => [...makeApp().projects.values()],
        allSessions: async () => LIST,
      } as unknown as Api,
      quietSocket().socket,
    );
    before.me = ME;
    await before.loadOrder('p0');
    await before.refreshSessions();

    // The next load has the kept order but never asks the hub for it, then refreshes.
    const app = new AppState(
      { ...hubApi(), projects: async () => [...makeApp().projects.values()], allSessions: async () => LIST } as unknown as Api,
      quietSocket().socket,
    );
    expect(app.listMode('p0')).toBe('recent');
    app.me = ME;
    await app.refreshSessions();
    expect(JSON.parse(localStorage.getItem('hqc.list')!).orders).toEqual({});
  });

  it('asks the hub again for a remembered list order', async () => {
    const before = new AppState(
      {
        ...hubApi({ mode: 'recent', session_ids: [] }),
        projects: async () => [...makeApp().projects.values()],
        allSessions: async () => LIST,
      } as unknown as Api,
      quietSocket().socket,
    );
    before.me = ME;
    await before.loadOrder('p0');
    await before.refreshSessions();

    // The order was changed elsewhere: the hub now says `state`.
    const api = { ...hubApi(), me: vi.fn(pending), projects: vi.fn(pending), allSessions: vi.fn(pending) };
    const app = new AppState(api as unknown as Api, quietSocket().socket);
    void app.init();
    expect(app.listMode('p0')).toBe('recent');
    await mount(<SessionList app={app} />);
    await act(async () => {});
    expect(api.sessionOrder).toHaveBeenCalledWith('p0');
    expect(app.listMode('p0')).toBe('state');
  });
});

describe('the read mark', () => {
  /** A hub answer the test releases by hand. */
  function later<T>() {
    let resolve!: (v: T) => void;
    const promise = new Promise<T>((r) => (resolve = r));
    return { promise, resolve };
  }
  const unread = sess('prof', 'Collision profiling', 'idle', at(22), { unread: 1, last_seq: 5, last_read_seq: 4 });
  const badge = () => root.querySelector('.session-row .badge');

  it('a session asked for before the chat was read does not bring its badge back', async () => {
    const answer = later<SessionInfo>();
    const api = { ...hubApi(), session: vi.fn(() => answer.promise) };
    const app = makeApp([unread], api);
    await mount(<SessionList app={app} />);
    expect(badge()!.textContent).toBe('1');
    // Opening the chat asks for the session, then moves the read cursor; the hub's summary
    // of the move comes first, the answer counted before it after.
    const asked = app.refreshSession('prof');
    await act(async () => app.markRead('prof', 5));
    expect(badge()).toBeNull();
    await act(async () =>
      app.mergeSummary([
        { session_id: 'prof', project_id: 'p1', title: 'Collision profiling', state: 'idle', queue_len: 0, unread: 0, last_seq: 5, last_read_seq: 5 },
      ]),
    );
    await act(async () => {
      answer.resolve(unread);
      await asked;
    });
    expect(app.sessions.get('prof')!.unread).toBe(0);
    expect(badge()).toBeNull();
    // A newer answer stands: a message after the one read.
    api.session.mockResolvedValueOnce({ ...unread, last_seq: 6, last_read_seq: 5 });
    await act(() => app.refreshSession('prof'));
    expect(badge()!.textContent).toBe('1');
  });

  it('a list asked for before the chat was read does not bring its badge back', async () => {
    const answer = later<SessionInfo[]>();
    const api = { ...hubApi(), projects: async () => [...makeApp().projects.values()], allSessions: () => answer.promise };
    const app = makeApp([unread], api);
    const asked = app.refreshSessions();
    app.markRead('prof', 5);
    answer.resolve([unread]);
    await asked;
    expect(app.sessions.get('prof')!.unread).toBe(0);
  });
});

describe("the column's project menu and width (desktop)", () => {
  it('"⋯": Open in new tab, favorites, Project settings…, Archive project, Delete project… (the last two not for the personal project)', async () => {
    const api = { ...hubApi(), deleteProject: vi.fn(async () => {}) };
    const app = makeApp(LIST, api);
    await mount(<SessionList app={app} />);
    await click(root.querySelector('.side-head .btn-project-menu'));
    const menu = root.querySelector('.ctx-menu')!;
    expect(menu.getAttribute('aria-label')).toBe('Project «physics-mp»');
    expect(menuItems()).toEqual(['Open in new tab', 'Add to favorites', 'Project settings…', 'Archive project', 'Delete project…']);
    // The column has the name: no line over the items.
    expect(menu.querySelector('.menu-head')).toBeNull();
    expect(menu.querySelector('.btn-open-tab')!.getAttribute('href')).toBe('#/p/p1');
    expect(menu.querySelector('.btn-open-tab')!.getAttribute('target')).toBe('_blank');
    expect(menu.querySelector('.btn-project-settings')!.getAttribute('href')).toBe('#/p/p1/settings');
    await click(menu.querySelector('.btn-favorite'));
    expect(api.setProjectFavorite).toHaveBeenCalledWith('p1', true);
    expect(root.querySelector('.ctx-menu')).toBeNull();

    // Delete asks first; "no" keeps the project.
    const ask = vi.fn((_text: string) => false);
    vi.stubGlobal('confirm', ask);
    try {
      await click(root.querySelector('.side-head .btn-project-menu'));
      await click(root.querySelector('.menu .btn-delete-project'));
      expect(ask.mock.calls[0][0]).toMatch(/^Delete project “physics-mp” and its sessions \(5\)\?/);
      expect(api.deleteProject).not.toHaveBeenCalled();
      ask.mockReturnValue(true);
      await click(root.querySelector('.side-head .btn-project-menu'));
      await click(root.querySelector('.menu .btn-delete-project'));
      expect(api.deleteProject).toHaveBeenCalledWith('p1');
      expect(app.projects.has('p1')).toBe(false);
    } finally {
      vi.unstubAllGlobals();
      location.hash = '';
    }
    // The personal project is not deleted (SPEC §3).
    await click(root.querySelector('.side-head .btn-project-menu'));
    expect(menuItems()).toEqual(['Open in new tab', 'Add to favorites', 'Project settings…']);
  });

  it('the right edge sets the width: dragged, or ←/→; kept on this device, within bounds', async () => {
    const app = makeApp();
    const css = () => document.documentElement.style.getPropertyValue('--list-w');
    // No layout here: the column (`.sidebar`, its border with it) is as wide as `--list-w` says,
    // 300 px without one; the list inside it would be a pixel narrower.
    const rect = vi.spyOn(HTMLElement.prototype, 'getBoundingClientRect').mockImplementation(function (this: HTMLElement) {
      const width = this.classList.contains('sidebar') ? parseInt(css()) || 300 : this.classList.contains('sessions') ? 299 : 0;
      return { width, height: 0, top: 0, left: 0, right: width, bottom: 0, x: 0, y: 0, toJSON: () => ({}) } as DOMRect;
    });
    // As the desktop layout has them: the grip next to the column, not in it.
    await mount(
      <>
        <aside class="sidebar">
          <SessionList app={app} />
        </aside>
        <WidthGrip app={app} />
      </>,
    );
    expect(root.querySelector('.sidebar .col-grip')).toBeNull();
    const grip = root.querySelector<HTMLElement>(':scope > .col-grip')!;
    expect(grip.getAttribute('role')).toBe('separator');
    // The width read out is the column's as it stands.
    expect(grip.getAttribute('aria-valuenow')).toBe('300');
    const ev = (type: string, x: number) =>
      act(async () => {
        grip.dispatchEvent(new PointerEvent(type, { bubbles: true, cancelable: true, pointerId: 1, clientX: x }));
      });
    try {
      // A click alone keeps the default.
      await ev('pointerdown', 100);
      await ev('pointerup', 100);
      expect(app.device.listWidth).toBeUndefined();
      // 20 px to the right: the column is 20 px wider, not 19.
      await ev('pointerdown', 100);
      await ev('pointermove', 120);
      expect(css()).toBe('320px');
      expect(grip.getAttribute('aria-valuenow')).toBe('320');
      expect(app.device.listWidth).toBeUndefined();
      await ev('pointerup', 120);
      expect(app.device.listWidth).toBe(320);
      expect(JSON.parse(localStorage.getItem('hqc.look')!).listWidth).toBe(320);
      expect(new AppState(hubApi() as unknown as Api, { onState: () => () => {} } as unknown as HubSocket).device.listWidth).toBe(320);
      await ev('pointerdown', 0);
      await ev('pointermove', 5000);
      await ev('pointerup', 5000);
      expect(app.device.listWidth).toBe(LIST_WIDTH_MAX);
      await act(async () => {
        grip.dispatchEvent(new KeyboardEvent('keydown', { key: 'ArrowLeft', bubbles: true }));
      });
      expect(app.device.listWidth).toBe(LIST_WIDTH_MAX - LIST_WIDTH_STEP);
      await ev('pointerdown', 5000);
      await ev('pointermove', -5000);
      await ev('pointerup', -5000);
      expect(app.device.listWidth).toBe(LIST_WIDTH_MIN);
      // The root carries it as `--list-w`; without one the default comes back.
      applyLook(document.documentElement, 'spacious', app.device);
      expect(css()).toBe(`${LIST_WIDTH_MIN}px`);
      applyLook(document.documentElement, 'spacious', { ...app.device, listWidth: undefined });
      expect(css()).toBe('');
    } finally {
      rect.mockRestore();
      document.documentElement.style.removeProperty('--list-w');
    }
  });
});

describe('menus over a pending permission card', () => {
  /** A Bash request whose card has Enter (allow once) and Esc (deny), as in the open chat. */
  function permissionCard() {
    const wire = join(import.meta.dirname, '../../crates/hqc-proto/examples/wire/event.approval.request.json');
    const timeline = new Timeline();
    timeline.apply(parseEvent(JSON.parse(readFileSync(wire, 'utf8'))));
    const actions: ChatActions = {
      timeline,
      approve: vi.fn(async () => {}),
      answer: vi.fn(async () => {}),
      resend: vi.fn(),
      discard: vi.fn(),
      openArtifact: vi.fn(),
      // Before the request expires (`expires_at` 11:00), so its card has the keys.
      now: () => Date.parse('2026-10-22T10:30:00Z'),
      hotkeyRequest: 'req-1',
    };
    return { actions, card: <ChatItems items={timeline.items()} actions={actions} /> };
  }

  it("Esc and Enter in an open menu are the menu's: the request is not answered", async () => {
    const { actions, card } = permissionCard();
    const app = makeApp();
    await mount(
      <>
        <SessionList app={app} />
        {card}
      </>,
    );
    expect(root.querySelector('.perm')).not.toBeNull();
    // The row's menu, its first item focused: Esc closes it and denies nothing.
    await menuOf('Network sync');
    await pressKey('Escape');
    expect(root.querySelector('.ctx-menu')).toBeNull();
    expect(actions.approve).not.toHaveBeenCalled();
    // Enter on an item is the item's: the card neither answers nor cancels it.
    await menuOf('Network sync');
    expect(document.activeElement!.textContent).toBe('Open in new tab');
    const enter = new KeyboardEvent('keydown', { key: 'Enter', bubbles: true, cancelable: true });
    await act(async () => {
      document.activeElement!.dispatchEvent(enter);
    });
    expect(enter.defaultPrevented).toBe(false);
    expect(actions.approve).not.toHaveBeenCalled();
    await pressKey('Escape');

    // The order menu with the focus left on the page (a click that moved no focus).
    await click(root.querySelector('.side-head .btn-mode'));
    (document.activeElement as HTMLElement).blur();
    await pressKey('Escape');
    expect(root.querySelector('.ctx-menu')).toBeNull();
    expect(actions.approve).not.toHaveBeenCalled();
    // The workers' list: Esc closes it only.
    await click(root.querySelector('.wline'));
    expect(root.querySelector('.wline-pop')).not.toBeNull();
    await pressKey('Escape');
    expect(root.querySelector('.wline-pop')).toBeNull();
    expect(actions.approve).not.toHaveBeenCalled();
    // Nothing open: the card has its keys.
    (document.activeElement as HTMLElement).blur();
    await pressKey('Escape');
    expect(actions.approve).toHaveBeenCalledWith('req-1', 'deny');
  });

  it("the project bar's menu: an item's Enter or Esc does not answer the request", async () => {
    const { actions, card } = permissionCard();
    const app = makeApp();
    await mount(
      <>
        <ProjectRail app={app} />
        {card}
      </>,
    );
    const tile = root.querySelector<HTMLElement>('a.rail-project[href="#/p/p1"]')!;
    await act(async () => {
      tile.dispatchEvent(new MouseEvent('contextmenu', { bubbles: true, cancelable: true, clientX: 10, clientY: 10 }));
    });
    expect(root.querySelector('.ctx-menu')).not.toBeNull();
    expect(root.querySelector('.ctx-menu')!.contains(document.activeElement)).toBe(true);
    await pressKey('Enter');
    await pressKey('Escape');
    expect(root.querySelector('.ctx-menu')).toBeNull();
    expect(actions.approve).not.toHaveBeenCalled();
    // The menu closed: the card has its keys.
    (document.activeElement as HTMLElement).blur();
    await pressKey('Enter');
    expect(actions.approve).toHaveBeenCalledWith('req-1', 'allow');
  });

  it('the project search: Esc in its field, Enter and Esc on a match do not answer the request', async () => {
    const { actions, card } = permissionCard();
    const app = makeApp();
    app.me = { ...app.me!, look: 'compact' };
    await mount(
      <>
        <ProjectRail app={app} />
        {card}
      </>,
    );
    const field = () => root.querySelector('input[aria-label="Search projects"]');
    await click(root.querySelector('.rail-search'));
    expect(document.activeElement).toBe(field());
    await pressKey('Escape');
    expect(field()).toBeNull();
    expect(actions.approve).not.toHaveBeenCalled();

    // Enter on a focused match is its link's: not cancelled, the card does not take it.
    await click(root.querySelector('.rail-search'));
    const hit = root.querySelector<HTMLAnchorElement>('.rail-hit[href="#/p/p1"]')!;
    await act(async () => hit.focus());
    const enter = new KeyboardEvent('keydown', { key: 'Enter', bubbles: true, cancelable: true });
    await act(async () => {
      hit.dispatchEvent(enter);
    });
    expect(enter.defaultPrevented).toBe(false);
    expect(actions.approve).not.toHaveBeenCalled();
    // The link opens the project and closes the search.
    await click(hit);
    expect(location.hash).toBe('#/p/p1');
    expect(field()).toBeNull();
    // Esc on a match closes the search only.
    await click(root.querySelector('.rail-search'));
    await act(async () => root.querySelector<HTMLElement>('.rail-hit')!.focus());
    await pressKey('Escape');
    expect(field()).toBeNull();
    expect(actions.approve).not.toHaveBeenCalled();
    location.hash = '';
  });
});

// SPEC §6: "+" opens a new session as a chat window at once, nothing on the hub; chips over its
// input choose the workspace, folder, permissions and model; the first message creates the
// session and the window becomes its chat; left unsent, the draft is gone.
describe('a new session', () => {
  const started = parseEvent({
    session_id: 'fresh',
    event_id: '0199f3a2-7c10-7000-8000-000000000201',
    seq: 1,
    ts: at(0),
    author: { kind: 'system' },
    type: 'system',
    payload: { text: 'Session created', code: 'session_created' },
  });
  const created = (more: Partial<SessionInfo> = {}) => sess('fresh', 'Session 6', 'idle', at(0), more);

  function draftApp(create: () => Promise<SessionInfo> = async () => created()) {
    let app!: AppState;
    // The chat's first page; held back while `loading` is set.
    let loading: Promise<void> | null = null;
    const api = {
      ...hubApi(),
      workspaces: async () => app.workspaces.get('p1') ?? [],
      createSession: vi.fn(create),
      eventsBefore: vi.fn(async (id: string) => {
        await loading;
        return id === 'fresh'
          ? { events: [started], invalid: [], maxSeq: 1, minSeq: 1, count: 1 }
          : { events: [], invalid: [], maxSeq: 0, count: 0 };
      }),
      input: vi.fn(async (id: string, text: string, op: string, ids: string[]) =>
        parseEvent({
          session_id: id,
          event_id: '0199f3a2-7c10-7000-8000-000000000202',
          seq: 2,
          ts: at(0),
          author: { kind: 'user', id: 'u' },
          type: 'user.message',
          payload: { text, artifact_ids: ids },
          client_op_id: op,
          delivery: 'pending',
        }),
      ),
      uploadArtifact: vi.fn(async (_file: Blob, name: string) => ({ id: `art-${name}` })),
      readCursor: vi.fn(async () => {}),
    };
    app = makeApp(LIST, api as unknown as ReturnType<typeof hubApi>);
    Object.assign(app.socket, { subscribe: vi.fn(() => () => {}) });
    return { app, api, hold: (p: Promise<void> | null) => (loading = p) };
  }

  /** The hash change and the hub's (mocked) replies. */
  const settle = () =>
    act(async () => {
      for (let i = 0; i < 4; i++) await new Promise((r) => setTimeout(r, 0));
    });
  const win = () => root.querySelector<HTMLElement>('.main .chat-new');
  const chips = () => [...root.querySelectorAll('.new-chip')].map((c) => c.textContent);
  const input = () => root.querySelector<HTMLTextAreaElement>('.chat-new .composer-input')!;
  const typeMessage = (text: string) =>
    act(async () => {
      input().value = text;
      input().dispatchEvent(new Event('input', { bubbles: true }));
    });
  const sendMessage = () =>
    act(async () => {
      root.querySelector('.chat-new form.composer')!.dispatchEvent(new Event('submit', { bubbles: true, cancelable: true }));
      for (let i = 0; i < 5; i++) await Promise.resolve();
    });
  /** Types into the field of a chip's menu and presses Enter there. */
  const typeInMenu = (name: string, value: string) =>
    act(async () => {
      const el = root.querySelector<HTMLInputElement>(`.ctx-menu input[name=${name}]`)!;
      el.value = value;
      el.dispatchEvent(new Event('input', { bubbles: true }));
      el.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', bubbles: true, cancelable: true }));
    });

  // A desktop window: the column with its "+" beside the chat.
  beforeEach(() => windowWidth(1280));
  afterEach(() => {
    location.hash = '';
    windowWidth(1024);
  });

  it('one press of "+" opens its window: nothing on the hub, the caret in its input, the defaults on its chips, "Draft" on top of the list', async () => {
    const { app, api } = draftApp();
    location.hash = '#/p/p1';
    await mount(<App app={app} />);
    await click(root.querySelector('.side-head .btn-new-session'));
    await settle();
    expect(location.hash).toBe('#/p/p1/new');
    expect(win()!.querySelector('.chat-title')!.textContent).toBe('New session');
    expect(win()!.querySelector('.chat-meta')!.textContent).toBe('Drafthome-mac/~/work/games/phys-mp');
    expect(win()!.querySelector('.new-empty')!.textContent).toBe(
      'Write the first message — the session starts with itWorkspace, folder, permissions and model are above the box; the defaults are already picked.',
    );
    expect(chips()).toEqual(['phys-mp · home-mac', 'Folder .', "Permissions: worker's default", 'Model: default']);
    expect(document.activeElement).toBe(input());
    // The draft on top of the list, the current row; no form in the column.
    expect(groups()[0]).toEqual(['Draft', undefined, ['New session']]);
    const row = root.querySelector('.session-row.is-draft')!;
    expect(row.getAttribute('aria-current')).toBe('page');
    expect(row.getAttribute('href')).toBe('#/p/p1/new');
    expect(row.querySelector('.session-sub')!.textContent).toBe('Draft · home-mac · ~/work/games/phys-mp');
    expect(root.querySelector('.sidebar form, .sidebar select')).toBeNull();
    expect(api.createSession).not.toHaveBeenCalled();

    // The workspace's menu: each with its worker and whole path, "Add a workspace…" under them.
    await click(root.querySelector('.btn-chip-workspace'));
    expect(root.querySelector('.ctx-menu .menu-head')!.textContent).toBe('Workspace');
    expect(menuItems()).toEqual(['phys-mphome-mac:~/work/games/phys-mp', 'phys-detdrzlo-mac:~/work/games/phys-det', 'Add a workspace…']);
    expect(root.querySelector('.ctx-menu .btn-add-workspace')!.getAttribute('href')).toBe('#/p/p1/settings');
    await click(root.querySelectorAll('.ctx-menu .btn-pick-workspace')[1]);
    expect(chips()[0]).toBe('phys-det · drzlo-mac');
    expect(win()!.querySelector('.chat-meta')!.textContent).toBe('Draftdrzlo-mac/~/work/games/phys-det');
    expect(row.querySelector('.session-sub')!.textContent).toBe('Draft · drzlo-mac · ~/work/games/phys-det');
    expect(api.createSession).not.toHaveBeenCalled();
  });

  it("the first message creates it with the chips' choices and goes in as its first input; the window becomes its chat, the message in it", async () => {
    let create!: (s: SessionInfo) => void;
    const { app, api, hold } = draftApp(() => new Promise((r) => (create = r)));
    location.hash = '#/p/p1/new';
    await mount(<App app={app} />);
    await settle();
    await click(root.querySelector('.btn-chip-folder'));
    await typeInMenu('cwd', 'net');
    expect(root.querySelector('.ctx-menu')).toBeNull();
    await click(root.querySelector('.btn-chip-permissions'));
    expect(menuItems()).toEqual(["The worker's default", 'Ask', 'Auto']);
    await click(root.querySelector('.ctx-menu .btn-permissions-ask'));
    await click(root.querySelector('.btn-chip-model'));
    expect(menuItems()).toEqual(["Claude Code's default", 'Opus', 'Sonnet', 'Haiku', 'Fable', 'Another model…']);
    await click(root.querySelector('.ctx-menu .btn-model-haiku'));
    expect(chips()).toEqual(['phys-mp · home-mac', 'Folder net', 'Permissions: Ask', 'Model: Haiku']);
    expect(win()!.querySelector('.chat-meta')!.textContent).toBe('Drafthome-mac/~/work/games/phys-mp/net');

    await typeMessage('Hello there');
    await sendMessage();
    expect(api.createSession).toHaveBeenCalledWith('p1', undefined, { workspace_id: 'w1', cwd: 'net', permission_mode: 'ask', model: 'haiku' });
    // On its way: shown as being sent, the input clear, the chips holding what was sent.
    expect(win()!.querySelector('.msg-pending')!.textContent).toContain('Hello there');
    expect(win()!.querySelector('.msg-pending')!.textContent).toContain('sending…');
    expect(input().value).toBe('');
    expect([...root.querySelectorAll<HTMLButtonElement>('.new-chip')].every((c) => c.disabled)).toBe(true);

    // Created: the input goes; until the chat's first page is in, the draft stands for the
    // session in the list, never both.
    let loaded!: () => void;
    hold(new Promise((r) => (loaded = r)));
    await act(async () => create(created({ cwd: 'net' })));
    await settle();
    expect(api.input).toHaveBeenCalledWith('fresh', 'Hello there', expect.any(String), []);
    expect(location.hash).toBe('#/p/p1/new');
    expect(groups()[0]).toEqual(['Draft', undefined, ['New session']]);
    expect(titles()).not.toContain('Session 6');

    await act(async () => loaded());
    await settle();
    expect(location.hash).toBe('#/s/fresh');
    expect(win()).toBeNull();
    const chat = root.querySelector('.main .chat')!;
    expect(chat.querySelector('.chat-title')!.textContent).toBe('Session 6');
    expect(chat.textContent).not.toContain('Loading…');
    expect(chat.querySelector('.feed')!.textContent).toContain('Session started');
    expect(chat.querySelector('.feed')!.textContent).toContain('Hello there');
    expect(app.newSession).toBeNull();
    expect(root.querySelector('.group-draft')).toBeNull();
    expect(rowOf('Session 6').querySelector('.session-row')!.getAttribute('aria-current')).toBe('page');
    expect(api.createSession).toHaveBeenCalledTimes(1);
  });

  it('left unsent, it is gone: another session opened, then "+" again opens a new one with the defaults', async () => {
    const { app, api } = draftApp();
    location.hash = '#/p/p1/new';
    await mount(<App app={app} />);
    await settle();
    await click(root.querySelector('.btn-chip-model'));
    await click(root.querySelector('.ctx-menu .btn-model-opus'));
    await typeMessage('half a thought');
    expect(chips()[3]).toBe('Model: Opus');

    await click(root.querySelector<HTMLElement>('a.session-row[href="#/s/net"]'));
    await settle();
    expect(location.hash).toBe('#/s/net');
    expect(win()).toBeNull();
    expect(app.newSession).toBeNull();
    expect(root.querySelector('.group-draft, .is-draft')).toBeNull();

    await click(root.querySelector('.side-head .btn-new-session'));
    await settle();
    expect(input().value).toBe('');
    expect(chips()).toEqual(['phys-mp · home-mac', 'Folder .', "Permissions: worker's default", 'Model: default']);
    expect(root.querySelectorAll('.is-draft')).toHaveLength(1);
    expect(api.createSession).not.toHaveBeenCalled();
    expect(api.input).not.toHaveBeenCalled();
  });

  /** A file picked with the input's clip. */
  const attach = (name: string) =>
    act(async () => {
      const picker = root.querySelector<HTMLInputElement>('.chat-new input[type=file]')!;
      Object.defineProperty(picker, 'files', { value: [new File(['abc'], name, { type: 'text/plain' })], configurable: true });
      picker.dispatchEvent(new Event('change', { bubbles: true }));
    });
  const pressEnter = () =>
    act(async () => {
      input().dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', bubbles: true, cancelable: true }));
      for (let i = 0; i < 5; i++) await Promise.resolve();
    });
  const chatInput = () => root.querySelector<HTMLTextAreaElement>('.main .chat:not(.chat-new) .composer-input')!;

  it('while it is being created the input sends nothing more: a second Enter keeps its text, send says "Starting…"', async () => {
    let create!: (s: SessionInfo) => void;
    const { app, api } = draftApp(() => new Promise((r) => (create = r)));
    location.hash = '#/p/p1/new';
    await mount(<App app={app} />);
    await settle();
    await typeMessage('Hello there');
    await pressEnter();
    expect(api.createSession).toHaveBeenCalledTimes(1);
    expect(input().value).toBe('');
    await typeMessage('and one more thing');
    await pressEnter();
    expect(input().value).toBe('and one more thing');
    const sendBtn = win()!.querySelector<HTMLButtonElement>('.btn-send')!;
    expect(sendBtn.disabled).toBe(true);
    expect(sendBtn.getAttribute('title')).toBe('Starting…');
    expect(win()!.querySelectorAll('.msg-pending')).toHaveLength(1);
    await act(async () => create(created()));
    await settle();
    expect(api.createSession).toHaveBeenCalledTimes(1);
    expect(api.input).toHaveBeenCalledTimes(1);
    expect(api.input).toHaveBeenCalledWith('fresh', 'Hello there', expect.any(String), []);
    expect(location.hash).toBe('#/s/fresh');
  });

  it('what is typed and attached while it is being created goes to the chat\'s input', async () => {
    let create!: (s: SessionInfo) => void;
    const { app, api } = draftApp(() => new Promise((r) => (create = r)));
    location.hash = '#/p/p1/new';
    await mount(<App app={app} />);
    await settle();
    await typeMessage('Hello there');
    await sendMessage();
    await typeMessage('and then this');
    await attach('notes.txt');
    await settle();
    await act(async () => create(created()));
    await settle();
    expect(location.hash).toBe('#/s/fresh');
    expect(chatInput().value).toBe('and then this');
    expect(root.querySelector('.main .chat .composer-files')!.textContent).toContain('notes.txt');
    expect(app.draft('fresh')).toBe('and then this');
    expect(api.input).toHaveBeenCalledTimes(1);
    expect(api.uploadArtifact).not.toHaveBeenCalled();
  });

  it('several files picked with the clip are all attached, though the input empties its list', async () => {
    const { app } = draftApp(() => new Promise(() => {}));
    location.hash = '#/p/p1/new';
    await mount(<App app={app} />);
    await settle();
    await act(async () => {
      const picker = root.querySelector<HTMLInputElement>('.chat-new input[type=file]')!;
      // As a browser does: clearing the input's value empties the very list it handed over.
      const files = [new File(['a'], 'one.txt', { type: 'text/plain' }), new File(['b'], 'two.txt', { type: 'text/plain' })];
      Object.defineProperty(picker, 'files', { value: files, configurable: true });
      Object.defineProperty(picker, 'value', { get: () => '', set: () => void (files.length = 0), configurable: true });
      picker.dispatchEvent(new Event('change', { bubbles: true }));
    });
    await settle();
    const shown = root.querySelector('.chat-new .composer-files')?.textContent ?? '';
    expect(shown).toContain('one.txt');
    expect(shown).toContain('two.txt');
  });

  it('a HEIC still being decoded holds the first message back, and shows when it is read', async () => {
    const { app } = draftApp(() => new Promise(() => {}));
    location.hash = '#/p/p1/new';
    await mount(<App app={app} />);
    await settle();
    // Brave on a computer: the browser does not decode HEIC, the worker answers when told to.
    let answer: (() => void) | null = null;
    vi.stubGlobal(
      'Worker',
      class {
        onmessage: ((e: { data: Blob }) => void) | null = null;
        onerror: (() => void) | null = null;
        postMessage() {
          answer = () => this.onmessage?.({ data: new Blob(['jpeg'], { type: 'image/jpeg' }) });
        }
        terminate() {}
      },
    );
    const decode = vi.spyOn(HTMLImageElement.prototype, 'decode').mockRejectedValue(new DOMException('', 'EncodingError'));
    try {
      // A text typed first: Send would be on but for the file being read.
      const area = root.querySelector<HTMLTextAreaElement>('.chat-new textarea')!;
      await act(async () => {
        area.value = 'look';
        area.dispatchEvent(new Event('input', { bubbles: true }));
      });
      await act(async () => {
        const picker = root.querySelector<HTMLInputElement>('.chat-new input[type=file]')!;
        const heic = new File([new Uint8Array([0, 0, 0, 24, 102, 116, 121, 112])], 'HQC.heic', { type: 'image/heic' });
        Object.defineProperty(picker, 'files', { value: [heic], configurable: true });
        picker.dispatchEvent(new Event('change', { bubbles: true }));
      });
      const send = () => root.querySelector<HTMLButtonElement>('.chat-new button[type=submit]')!;
      await vi.waitFor(() => expect(root.querySelector('.chat-new .composer-files .attachment-chip')?.textContent).toBe('reading 1 file…'));
      expect(send().disabled).toBe(true);
      // The file is read before it goes to the worker.
      await vi.waitFor(() => expect(answer).not.toBeNull());
      expect(send().disabled).toBe(true);
      await act(async () => answer!());
      await vi.waitFor(() => expect(root.querySelector('.chat-new .attachment-thumb')?.getAttribute('title')).toBe('HQC.jpg'));
      expect(root.querySelector('.chat-new .composer-files')!.textContent).not.toContain('reading');
      expect(send().disabled).toBe(false);
    } finally {
      decode.mockRestore();
      vi.unstubAllGlobals();
    }
  });

  it('HEICs attached while it is being created and still being read go to the chat once they are read', async () => {
    let create!: (s: SessionInfo) => void;
    const { app } = draftApp(() => new Promise((r) => (create = r)));
    location.hash = '#/p/p1/new';
    await mount(<App app={app} />);
    await settle();
    await typeMessage('Hello there');
    await sendMessage();
    // One worker per file; each answers when told to.
    const answers: (() => void)[] = [];
    vi.stubGlobal(
      'Worker',
      class {
        onmessage: ((e: { data: Blob }) => void) | null = null;
        onerror: (() => void) | null = null;
        postMessage() {
          answers.push(() => this.onmessage?.({ data: new Blob(['jpeg'], { type: 'image/jpeg' }) }));
        }
        terminate() {}
      },
    );
    const decode = vi.spyOn(HTMLImageElement.prototype, 'decode').mockRejectedValue(new DOMException('', 'EncodingError'));
    const pick = (name: string) =>
      act(async () => {
        const picker = root.querySelector<HTMLInputElement>('.chat-new input[type=file]')!;
        const heic = new File([new Uint8Array([0, 0, 0, 24, 102, 116, 121, 112])], name, { type: 'image/heic' });
        Object.defineProperty(picker, 'files', { value: [heic], configurable: true });
        picker.dispatchEvent(new Event('change', { bubbles: true }));
      });
    try {
      await pick('one.heic');
      await vi.waitFor(() => expect(answers).toHaveLength(1));
      await act(async () => create(created()));
      await settle();
      // The window waits for the file before it gives way to the chat.
      expect(location.hash).toBe('#/p/p1/new');
      // A second one attached during that wait is waited for too.
      await pick('two.heic');
      await vi.waitFor(() => expect(answers).toHaveLength(2));
      await act(async () => answers[0]());
      await settle();
      expect(location.hash).toBe('#/p/p1/new');
      await act(async () => answers[1]());
      await settle();
      await vi.waitFor(() => expect(location.hash).toBe('#/s/fresh'));
      await vi.waitFor(() =>
        expect([...root.querySelectorAll('.main .chat .composer-files .attachment-thumb')].map((i) => i.getAttribute('title'))).toEqual(['one.jpg', 'two.jpg']),
      );
    } finally {
      decode.mockRestore();
      vi.unstubAllGlobals();
    }
  });

  it("after the switch the caret is in the chat's input, and again when the chat is opened later", async () => {
    const { app } = draftApp();
    location.hash = '#/p/p1/new';
    await mount(<App app={app} />);
    await settle();
    await typeMessage('Hello there');
    await sendMessage();
    await settle();
    expect(location.hash).toBe('#/s/fresh');
    expect(document.activeElement).toBe(chatInput());
    // On a computer every chat opened has the caret in its input, as in the VS Code tab.
    location.hash = '#/s/net';
    await settle();
    (document.activeElement as HTMLElement | null)?.blur();
    location.hash = '#/s/fresh';
    await settle();
    expect(document.activeElement).toBe(chatInput());
  });

  it('the caret is handed once: the input shown again in the same chat (after a lock) does not take it', async () => {
    const { app } = draftApp();
    location.hash = '#/p/p1/new';
    await mount(<App app={app} />);
    await settle();
    await typeMessage('Hello there');
    await sendMessage();
    await settle();
    expect(document.activeElement).toBe(chatInput());
    const flip = async (state: SessionInfo['state']) => {
      app.sessions.set('fresh', { ...app.sessions.get('fresh')!, state });
      await act(async () => (app as unknown as { changed(): void }).changed());
      await settle();
    };
    await flip('locked_vscode');
    expect(root.querySelector('.main .chat:not(.chat-new) .composer-input')).toBeNull();
    (document.activeElement as HTMLElement | null)?.blur();
    await flip('idle');
    expect(chatInput()).not.toBeNull();
    expect(document.activeElement).not.toBe(chatInput());
  });

  it('left while it is being created: no switch, the session in the list with the message sent and what was typed as its draft', async () => {
    let create!: (s: SessionInfo) => void;
    const { app, api } = draftApp(() => new Promise((r) => (create = r)));
    location.hash = '#/p/p1/new';
    await mount(<App app={app} />);
    await settle();
    await typeMessage('Hello there');
    await sendMessage();
    await typeMessage('half a thought');
    location.hash = '#/s/net';
    await settle();
    expect(win()).toBeNull();
    await act(async () => create(created()));
    await settle();
    expect(location.hash).toBe('#/s/net');
    expect(api.input).toHaveBeenCalledWith('fresh', 'Hello there', expect.any(String), []);
    expect(titles()).toContain('Session 6');
    expect(app.draft('fresh')).toBe('half a thought');
    expect(rowOf('Session 6').querySelector('.session-draft')!.textContent).toBe('Draft');
    expect(root.querySelector('.sidebar .notice')).toBeNull();
  });

  it('left while it is being created and the hub refuses it: the list says so, the message was not sent', async () => {
    let refuse!: (e: Error) => void;
    const { app, api } = draftApp(() => new Promise((_, r) => (refuse = r)));
    location.hash = '#/p/p1/new';
    await mount(<App app={app} />);
    await settle();
    await typeMessage('Hello there');
    await sendMessage();
    location.hash = '#/s/net';
    await settle();
    await act(async () => refuse(new Error('no workspace')));
    await settle();
    expect(location.hash).toBe('#/s/net');
    expect(root.querySelector('.sidebar .notice[role=alert]')!.textContent).toBe('Not started: no workspace — your message was not sent');
    expect(api.input).not.toHaveBeenCalled();
  });

  it('files dropped on its window are attached, as on a chat', async () => {
    const { app } = draftApp();
    location.hash = '#/p/p1/new';
    await mount(<App app={app} />);
    await settle();
    const drag = (type: string, files: File[] = []) =>
      act(async () => {
        const ev = new Event(type, { bubbles: true, cancelable: true });
        Object.defineProperty(ev, 'dataTransfer', { value: { types: ['Files'], files } });
        win()!.querySelector('.timeline')!.dispatchEvent(ev);
      });
    await drag('dragover');
    expect(win()!.classList.contains('chat-dragging')).toBe(true);
    await drag('drop', [new File(['abc'], 'shot.txt', { type: 'text/plain' })]);
    await settle();
    expect(win()!.classList.contains('chat-dragging')).toBe(false);
    expect(win()!.querySelector('.composer-files')!.textContent).toContain('shot.txt');
  });

  it('the workspace chip names itself with the whole path', async () => {
    const { app } = draftApp();
    location.hash = '#/p/p1/new';
    await mount(<App app={app} />);
    await settle();
    const ws = root.querySelector('.btn-chip-workspace')!;
    expect(ws.getAttribute('aria-label')).toBe('Workspace: phys-mp — home-mac:~/work/games/phys-mp');
    expect(ws.getAttribute('title')).toBe('Workspace: phys-mp — home-mac:~/work/games/phys-mp');
  });

  it('a create the hub refuses keeps the draft: the reason said, the text back in the input, the file with it; the next send starts it', async () => {
    let fail = true;
    const { app, api } = draftApp(async () => {
      if (fail) throw new Error('the worker is still making ~/work/games/phys-mp');
      return created();
    });
    location.hash = '#/p/p1/new';
    await mount(<App app={app} />);
    await settle();
    await act(async () => {
      const picker = root.querySelector<HTMLInputElement>('.chat-new input[type=file]')!;
      Object.defineProperty(picker, 'files', { value: [new File(['abc'], 'notes.txt', { type: 'text/plain' })], configurable: true });
      picker.dispatchEvent(new Event('change', { bubbles: true }));
    });
    await settle();
    await typeMessage('Build it');
    await sendMessage();
    await settle();
    expect(location.hash).toBe('#/p/p1/new');
    expect(win()!.querySelector('.notice')!.textContent).toBe('Not started: the worker is still making ~/work/games/phys-mp');
    expect(win()!.querySelector('.msg-pending')).toBeNull();
    expect(input().value).toBe('Build it');
    expect(document.activeElement).toBe(input());
    expect(win()!.querySelector('.composer-files')!.textContent).toContain('notes.txt');
    expect([...root.querySelectorAll<HTMLButtonElement>('.new-chip')].some((c) => c.disabled)).toBe(false);
    expect(groups()[0]).toEqual(['Draft', undefined, ['New session']]);
    expect(api.input).not.toHaveBeenCalled();
    expect(api.uploadArtifact).not.toHaveBeenCalled();

    fail = false;
    await sendMessage();
    await settle();
    expect(api.createSession).toHaveBeenCalledTimes(2);
    expect(api.uploadArtifact).toHaveBeenCalledWith(expect.any(Blob), 'notes.txt');
    expect(api.input).toHaveBeenCalledWith('fresh', 'Build it', expect.any(String), ['art-notes.txt']);
    expect(location.hash).toBe('#/s/fresh');
  });

  it('a session made by its first message while the lists load is not dropped by them', async () => {
    const { app, api } = draftApp();
    let land!: () => void;
    const gate = new Promise<void>((r) => (land = r));
    // The first list is read before the hub made the session; the next one has it.
    const lists = {
      projects: vi.fn(async () => [...app.projects.values()]),
      allSessions: vi.fn(async () => {
        if (lists.allSessions.mock.calls.length > 1) return [...LIST, created()];
        await gate;
        return LIST;
      }),
    };
    Object.assign(api, lists);
    const refresh = app.refreshSessions();
    app.openNewSession('p1');
    const s = await app.startNewSession();
    land();
    await act(async () => {
      await refresh;
    });
    expect(lists.allSessions).toHaveBeenCalledTimes(2);
    expect(app.sessions.has(s.id)).toBe(true);
  });
});

// A window 900–1079 px wide has no session column in the grid;
// "Sessions" at the left of the chat's header (or ⌘\, Ctrl+\ off a Mac) slides the same column
// out over the chat, and it goes away by itself.
describe('a window 900–1079 px wide', () => {
  /** "Network sync" waits on a Bash request whose card has Enter and Esc. */
  const wire = JSON.parse(readFileSync(join(import.meta.dirname, '../../crates/hqc-proto/examples/wire/event.approval.request.json'), 'utf8'));
  const request = parseEvent({ ...wire, session_id: 'net', seq: 1, payload: { ...wire.payload, expires_at: '2026-12-01T00:00:00Z' } });
  function narrowApp() {
    const api = {
      ...hubApi(),
      eventsBefore: vi.fn(async (id: string) =>
        id === 'net' ? { events: [request], invalid: [], maxSeq: 1, minSeq: 1, count: 1 } : { events: [], invalid: [], maxSeq: 0, count: 0 },
      ),
      readCursor: vi.fn(async () => {}),
      approve: vi.fn(async () => {}),
    };
    const app = makeApp(LIST, api);
    Object.assign(app.socket, { subscribe: vi.fn(() => () => {}) });
    return { app, api };
  }
  /** The app on `hash`, the chat's first page in. */
  async function open(app: AppState, hash: string) {
    location.hash = hash;
    await mount(<App app={app} />);
    await settle();
  }
  /** The hash change and the hub's (mocked) replies. */
  const settle = () =>
    act(async () => {
      for (let i = 0; i < 4; i++) await new Promise((r) => setTimeout(r, 0));
    });
  const go = async (hash: string) => {
    await act(async () => void (location.hash = hash));
    await settle();
  };
  const sessionsButton = () => root.querySelector<HTMLButtonElement>('.main .chat-head .btn-sessions');
  const slid = () => root.querySelector<HTMLElement>('.layout > .sidebar-slide');
  const scrim = () => root.querySelector<HTMLElement>('.layout > .sessions-scrim');
  /** ⌘\ on a Mac, Ctrl+\ elsewhere, pressed where the focus is. */
  const toggleKey = () =>
    act(async () => {
      const mac = onMac();
      (document.activeElement ?? document).dispatchEvent(
        new KeyboardEvent('keydown', { key: '\\', code: 'Backslash', metaKey: mac, ctrlKey: !mac, bubbles: true, cancelable: true }),
      );
    });

  beforeEach(() => windowWidth(1000));
  afterEach(() => {
    location.hash = '';
    windowWidth(1024);
  });

  it('no column in the grid: "Sessions" at the left of the header, with the count of the sessions that wait for you', async () => {
    const { app } = narrowApp();
    await open(app, '#/s/stones');
    const layout = root.querySelector('.layout')!;
    expect(layout.classList.contains('layout-narrow')).toBe(true);
    expect(root.querySelector('.sidebar, .col-grip, .session-row')).toBeNull();
    const button = sessionsButton()!;
    expect(button.nextElementSibling!.classList.contains('chat-head-main')).toBe(true);
    expect(button.getAttribute('aria-expanded')).toBe('false');
    // "Network sync" waits: the rail badge's count.
    expect(button.textContent).toBe('Sessions1');
    expect(button.querySelector('.sessions-waiting')!.textContent).toBe('1');
    expect(root.querySelector('.rail-badge')!.textContent).toBe('1');
    // No one waits: no count.
    await act(async () => {
      app.sessions.set('net', { ...app.sessions.get('net')!, state: 'idle' });
      (app as unknown as { changed(): void }).changed();
    });
    expect(sessionsButton()!.textContent).toBe('Sessions');
    expect(sessionsButton()!.querySelector('.sessions-waiting')).toBeNull();
    // A new session's window has it too, and a project with nothing to reopen.
    await go('#/p/p1/new');
    expect(root.querySelector('.main .chat-new .chat-head .btn-sessions')).not.toBeNull();
    await go('#/p/p0');
    expect(root.querySelector('.main > .chat-head .btn-sessions')).not.toBeNull();
    expect(root.querySelector('.main > .placeholder')).not.toBeNull();
  });

  it('"Sessions" slides the column out over the chat, pressed while it is out; a click beside it puts it away', async () => {
    const { app } = narrowApp();
    await open(app, '#/s/stones');
    await click(sessionsButton());
    expect(sessionsButton()!.getAttribute('aria-expanded')).toBe('true');
    expect(sessionsButton()!.classList.contains('is-on')).toBe(true);
    expect(scrim()).not.toBeNull();
    // The desktop column's list, the open chat its current row.
    expect(slid()!.classList.contains('sidebar')).toBe(true);
    expect(slid()!.querySelector('.session-row.is-current .session-title')!.textContent).toBe('Stone textures');
    expect(slid()!.querySelector('.side-head .btn-new-session')).not.toBeNull();
    await click(scrim());
    expect(slid()).toBeNull();
    expect(scrim()).toBeNull();
    expect(sessionsButton()!.getAttribute('aria-expanded')).toBe('false');
    // The button slides it out and back.
    await click(sessionsButton());
    expect(slid()).not.toBeNull();
    await click(sessionsButton());
    expect(slid()).toBeNull();
  });

  it('picking a session puts it away, and so does any other address', async () => {
    const { app } = narrowApp();
    await open(app, '#/s/stones');
    // The open session's own row: the address stays, the column goes away all the same.
    await click(sessionsButton());
    await click(slid()!.querySelector('a.session-row.is-current'));
    await settle();
    expect(location.hash).toBe('#/s/stones');
    expect(slid()).toBeNull();
    await click(sessionsButton());
    await click(rowOf('Cave scene port').querySelector('a.session-row'));
    await settle();
    expect(location.hash).toBe('#/s/cave');
    expect(root.querySelector('.chat-title')!.textContent).toBe('Cave scene port');
    expect(slid()).toBeNull();
    expect(scrim()).toBeNull();
    // "+" of the column: the new session's window, the column away.
    await click(sessionsButton());
    await click(slid()!.querySelector('.side-head .btn-new-session'));
    await settle();
    expect(location.hash).toBe('#/p/p1/new');
    expect(slid()).toBeNull();
    // On the new session's window, its own Draft row and "+" (the same address) too.
    await click(sessionsButton());
    await click(slid()!.querySelector('a.session-row.is-draft'));
    await settle();
    expect(location.hash).toBe('#/p/p1/new');
    expect(slid()).toBeNull();
    await click(sessionsButton());
    await click(slid()!.querySelector('.side-head .btn-new-session'));
    await settle();
    expect(location.hash).toBe('#/p/p1/new');
    expect(slid()).toBeNull();
  });

  it('Esc on the search page puts the column away and stays on the page', async () => {
    const { app } = narrowApp();
    await open(app, '#/search');
    // The focus is in the page's field, which takes Esc to go back.
    expect(root.querySelector('.main')!.contains(document.activeElement)).toBe(true);
    await toggleKey();
    expect(slid()).not.toBeNull();
    // Esc is taken, else the browser would clear the field it is pressed in.
    const esc = new KeyboardEvent('keydown', { key: 'Escape', bubbles: true, cancelable: true });
    await act(async () => void document.activeElement!.dispatchEvent(esc));
    expect(slid()).toBeNull();
    expect(esc.defaultPrevented).toBe(true);
    expect(location.hash.startsWith('#/search')).toBe(true);
  });

  it('⌘\\ (Ctrl+\\ off a Mac) slides it out and back, from anywhere', async () => {
    const { app } = narrowApp();
    await open(app, '#/s/stones');
    await toggleKey();
    expect(slid()).not.toBeNull();
    expect(sessionsButton()!.getAttribute('aria-expanded')).toBe('true');
    await toggleKey();
    expect(slid()).toBeNull();
    // With the column away Esc is left to the page.
    const esc = new KeyboardEvent('keydown', { key: 'Escape', bubbles: true, cancelable: true });
    await act(async () => void document.body.dispatchEvent(esc));
    expect(esc.defaultPrevented).toBe(false);
    // From the chat's input too.
    await act(async () => root.querySelector<HTMLTextAreaElement>('.composer-input')!.focus());
    await toggleKey();
    expect(slid()).not.toBeNull();
  });

  it('Esc puts it away; an Esc a menu of the list takes closes that menu only', async () => {
    const { app } = narrowApp();
    await open(app, '#/s/stones');
    await click(sessionsButton());
    // A row's menu, its first item focused.
    await menuOf('Cave scene port');
    expect(root.querySelector('.ctx-menu')).not.toBeNull();
    await pressKey('Escape');
    expect(root.querySelector('.ctx-menu')).toBeNull();
    expect(slid()).not.toBeNull();
    // The workers' list at the column's foot.
    await click(slid()!.querySelector('.wline'));
    expect(root.querySelector('.wline-pop')).not.toBeNull();
    await pressKey('Escape');
    expect(root.querySelector('.wline-pop')).toBeNull();
    expect(slid()).not.toBeNull();
    (document.activeElement as HTMLElement).blur();
    await pressKey('Escape');
    expect(slid()).toBeNull();
    expect(scrim()).toBeNull();
  });

  it("Esc over a chat with a pending request puts the column away and does not answer the request", async () => {
    const { app, api } = narrowApp();
    await open(app, '#/s/net');
    expect(root.querySelector('.perm.is-open')).not.toBeNull();
    await click(sessionsButton());
    (document.activeElement as HTMLElement).blur();
    await pressKey('Escape');
    expect(slid()).toBeNull();
    expect(api.approve).not.toHaveBeenCalled();
    // The column away, the card has its keys again.
    await pressKey('Escape');
    expect(api.approve.mock.calls[0].slice(0, 3)).toEqual(['net', 'req-1', 'deny']);
  });

  it('only from 900 to 1079 px: narrower the phone, wider the desktop as it was', async () => {
    const { app } = narrowApp();
    location.hash = '#/s/stones';
    for (const [width, kind] of [
      [899, 'phone'],
      [900, 'narrow'],
      [1079, 'narrow'],
      [1080, 'desktop'],
    ] as const) {
      windowWidth(width);
      await mount(<App app={app} />);
      await settle();
      const cls = root.querySelector('.layout')!.classList;
      expect([cls.contains('layout-phone'), cls.contains('layout-narrow')], `${width}`).toEqual([kind === 'phone', kind === 'narrow']);
      expect(!!root.querySelector('.btn-sessions'), `${width}`).toBe(kind === 'narrow');
      expect(!!root.querySelector('.layout > .sidebar'), `${width}`).toBe(kind === 'desktop');
      render(null, root);
    }
    // The desktop, here without `matchMedia`: the column and its grip in the grid, no
    // "Sessions", and ⌘\ slides nothing out.
    vi.stubGlobal('matchMedia', undefined);
    try {
      await mount(<App app={app} />);
      await settle();
      expect(root.querySelector('.layout')!.className).toBe('layout route-session');
      expect(root.querySelector('.layout > .sidebar')).not.toBeNull();
      expect(root.querySelector('.layout > .col-grip')).not.toBeNull();
      expect(root.querySelector('.btn-sessions')).toBeNull();
      // ⌘\ is not taken: the key stays the page's.
      const press = new KeyboardEvent('keydown', { key: '\\', code: 'Backslash', metaKey: onMac(), ctrlKey: !onMac(), bubbles: true, cancelable: true });
      await act(async () => void document.dispatchEvent(press));
      expect(press.defaultPrevented).toBe(false);
      await toggleKey();
      expect(root.querySelector('.sidebar-slide, .sessions-scrim')).toBeNull();
    } finally {
      vi.unstubAllGlobals();
    }
  });

  it("Settings' arrow goes back to the address before it, past its worker-code page", async () => {
    const { app, api } = narrowApp();
    Object.assign(api, {
      workers: vi.fn(async () => []),
      enrollInfo: vi.fn(async () => ({})),
      search: vi.fn(async () => ({ results: [], also: [] })),
    });
    await open(app, '#/s/stones');
    await go('#/settings');
    const back = () => root.querySelector('.page .bar .back')!.getAttribute('href');
    expect(back()).toBe('#/s/stones');
    await go('#/enroll');
    await go('#/settings');
    expect(back()).toBe('#/s/stones');
    // From the search, with the query it keeps in the address without a hash change; the
    // search's own arrow then skips Settings.
    await go('#/search');
    history.replaceState(null, '', '#/search?q=kiln');
    await go('#/settings');
    expect(back()).toBe('#/search?q=kiln');
    await go('#/search?q=kiln');
    expect(root.querySelector('.sp-back')!.getAttribute('href')).toBe('#/s/stones');
    // From "#/", an empty placeholder on a computer: the current project.
    await go('#/');
    await go('#/settings');
    expect(back()).toBe('#/p/p1');
  });
});

// A phone (< 900 px). `#/` lists the projects — Favorites, Needs you,
// Running, the others, each project once — or, from the title's menu, every project's sessions by state;
// `#/p/<id>` is one project's sessions under one header row. The look is the account's.
describe('a phone (< 900 px)', () => {
  const project = (id: string, name: string, more: Partial<Project> = {}): Project => ({
    id,
    name,
    slug: name,
    description: '',
    is_personal: false,
    color: 'moss',
    created_at: at(80),
    ...more,
  });
  /** physics-mp (LIST) has one session waiting and two running; boards, a favorite, an idle one
   *  with unread events; kiln two at work (one by a subagent after its turn) and an archived
   *  one; Personal only the workers' service sessions. */
  function phoneApp(look: 'compact' | 'spacious' = 'spacious') {
    const api = {
      ...hubApi(),
      eventsBefore: vi.fn(async () => ({ events: [], invalid: [], maxSeq: 0, count: 0 })),
      readCursor: vi.fn(async () => {}),
    };
    const app = makeApp(
      [
        ...LIST,
        sess('etch', 'Etching', 'idle', at(3), { project_id: 'p2', unread: 4 }),
        sess('fire', 'Firing curve', 'running', at(0.5), { project_id: 'p3', subagents: 1 }),
        sess('glaze', 'Glaze test', 'idle', at(0.3), { project_id: 'p3', subagents: 2 }),
        sess('old', 'Old kiln log', 'archived', at(0.4), { project_id: 'p3' }),
      ],
      api as unknown as ReturnType<typeof hubApi>,
    );
    app.me = { ...app.me!, look };
    app.projects.set('p2', project('p2', 'boards', { favorite_at: at(5) }));
    app.projects.set('p3', project('p3', 'kiln', { color: 'coral' }));
    Object.assign(app.socket, { subscribe: vi.fn(() => () => {}) });
    return { app, api };
  }
  /** The hash change and the hub's (mocked) replies. */
  const settle = () =>
    act(async () => {
      for (let i = 0; i < 4; i++) await new Promise((r) => setTimeout(r, 0));
    });
  async function open(app: AppState, hash: string) {
    location.hash = hash;
    await mount(<App app={app} />);
    await settle();
  }
  const go = async (hash: string) => {
    await act(async () => void (location.hash = hash));
    await settle();
  };
  const viewTitle = () => root.querySelector('.phone-home > .side-head .phone-view')!.textContent;

  beforeEach(() => windowWidth(390));
  afterEach(() => {
    location.hash = '';
    windowWidth(1024);
    // The look App put on the root.
    document.documentElement.className = '';
  });

  it("`#/` is the projects: Favorites, Needs you, Running, then the others in the bar's order and \"New project\"; a tap opens one", async () => {
    const { app } = phoneApp();
    await open(app, '#/');
    expect(root.querySelector('.layout')!.classList.contains('layout-phone')).toBe(true);
    // One header row: the view's title, the search, the settings and who is signed in.
    const head = root.querySelector('.phone-home > .side-head')!;
    expect(viewTitle()).toBe('Projects');
    expect(head.querySelector('.btn-search')!.getAttribute('href')).toBe('#/search');
    expect(head.querySelector('.btn-settings')!.getAttribute('href')).toBe('#/settings');
    expect(head.querySelector('.avatar')!.getAttribute('title')).toBe('who@example.com');
    expect([...root.querySelectorAll('.phone-home .group-head')].map((h) => h.textContent)).toEqual([
      'Favorites1',
      'Needs you1',
      'Running1',
      'Other projects1',
    ]);
    expect(phoneNames('wait')).toEqual(['physics-mp']);
    // physics-mp runs too, but it is under Needs you already.
    expect(phoneNames('run')).toEqual(['kiln']);
    expect(phoneNames('favorites')).toEqual(['boards']);
    expect(phoneNames('all')).toEqual(['Personal']);
    // Each project once: a favorite whose session works is under Favorites only.
    expect(new Set(phoneAll()).size).toBe(phoneAll().length);
    await act(async () => void (await app.setProjectFavorite('p3', true)));
    expect(phoneNames('run')).toEqual([]);
    expect(phoneNames('favorites')).toContain('kiln');
    expect(phoneAll().filter((n) => n === 'kiln')).toHaveLength(1);
    await act(async () => void (await app.setProjectFavorite('p3', false)));
    // A row: how many run, how many wait, how many have unread events (bold then); archived and
    // service sessions do not count.
    expect(['boards', 'Personal', 'physics-mp', 'kiln'].map(phoneMarks)).toEqual([['unread: 1'], [], ['running: 2', 'needs you: 1'], ['running: 2']]);
    expect(['boards', 'kiln'].map((n) => phoneRow(n).classList.contains('has-unread'))).toEqual([true, false]);
    // The running dot is each project's own color (blue, coral), not the last opened project's.
    const runDot = (n: string) => phoneRow(n).querySelector<HTMLElement>('.phone-run .dot')!.style.background;
    expect(['physics-mp', 'kiln'].map(runDot)).toEqual(['#6E9BFF', '#F08A5D']);
    expect(root.querySelector('.group-all li:last-child .btn-new-project')!.textContent).toBe('New project');
    // The workers: one line at the bottom, their list above it on a press.
    expect(root.querySelector('.phone-home > .wline-box .wline-count')!.textContent).toBe('1 worker online');
    expect(root.querySelector('.worker-list')).toBeNull();
    expect(root.querySelector('.session-row, .chips, .fab')).toBeNull();

    await click(phoneRow('physics-mp'));
    await settle();
    expect(location.hash).toBe('#/p/p1');
    expect(root.querySelector('.phone-home')).toBeNull();
    expect(root.querySelector('.sessions-phone > .side-head .project-title')!.textContent).toBe('physics-mp');
    await click(root.querySelector('.side-head .btn-back'));
    await settle();
    expect(location.hash).toBe('#/');
    expect(viewTitle()).toBe('Projects');
  });

  it('with no favorite and nothing waiting or running, every project is under "All projects"; a favorite makes the rest "Other projects"', async () => {
    const api = {
      ...hubApi(),
      eventsBefore: vi.fn(async () => ({ events: [], invalid: [], maxSeq: 0, count: 0 })),
      readCursor: vi.fn(async () => {}),
    };
    const app = makeApp([sess('quiet', 'Quiet', 'idle', at(2))], api as unknown as ReturnType<typeof hubApi>);
    Object.assign(app.socket, { subscribe: vi.fn(() => () => {}) });
    await open(app, '#/');
    expect([...root.querySelectorAll('.phone-home .group-head')].map((h) => h.textContent)).toEqual(['All projects2']);
    expect(phoneNames('all')).toEqual(['Personal', 'physics-mp']);
    await act(async () => void (await app.setProjectFavorite('p1', true)));
    expect([...root.querySelectorAll('.phone-home .group-head')].map((h) => h.textContent)).toEqual(['Favorites1', 'Other projects1']);
    expect(phoneNames('all')).toEqual(['Personal']);
  });

  it("the title's menu: Projects or Sessions — every project's live sessions by state, each with its project's tile; the pick lasts until the page is reloaded", async () => {
    const { app } = phoneApp();
    await open(app, '#/');
    await click(root.querySelector('.phone-view'));
    expect([...root.querySelectorAll('.menu .menu-item')].map((b) => [b.textContent, b.getAttribute('aria-checked')])).toEqual([
      ['Projects', 'true'],
      ['Sessionsevery project, by state', 'false'],
    ]);
    expect(root.querySelector('.menu-scrim')).not.toBeNull();
    await click(root.querySelector('.menu .btn-view-sessions'));
    expect(viewTitle()).toBe('Sessions');
    expect(root.querySelector('.phone-project')).toBeNull();
    // As State groups a project's list: the archived and the service sessions left out.
    expect(groups()).toEqual([
      ['Needs you', '1', ['Network sync']],
      ['Running', '4', ['Stone textures', 'Step determinism', 'Glaze test', 'Firing curve']],
      ['Idle · Today', '1', ['Etching']],
      ['Idle · Yesterday', '1', ['Collision profiling']],
      ['Idle · Last 7 days', '1', ['Cave scene port']],
    ]);
    const tile = (title: string) => rowOf(title).querySelector('.session-row > .phone-tile')!.getAttribute('aria-label');
    expect(['Network sync', 'Glaze test', 'Etching'].map(tile)).toEqual(['physics-mp', 'kiln', 'boards']);
    // A working dot in its project's color (kiln's coral); waiting and idle keep theirs.
    const dot = (title: string) => rowOf(title).querySelector<HTMLElement>('.session-row > .dot')!.style.background;
    expect(['Glaze test', 'Network sync', 'Etching'].map(dot)).toEqual(['#F08A5D', '', '']);
    expect(rowOf('Etching').querySelector('a.session-row')!.getAttribute('href')).toBe('#/s/etch');

    // A project and back: still Sessions; nothing of it is stored.
    await go('#/p/p3');
    expect(root.querySelector('.phone-home')).toBeNull();
    await go('#/');
    expect(viewTitle()).toBe('Sessions');
    const keys = Array.from({ length: localStorage.length }, (_, i) => localStorage.key(i));
    expect(keys.filter((k) => /view|phone/i.test(k ?? ''))).toEqual([]);
    expect(phoneApp().app.phoneView).toBe('projects');
    // A tap opens the session; its "‹" leads to its project.
    await click(rowOf('Network sync').querySelector('a.session-row'));
    await settle();
    expect(location.hash).toBe('#/s/net');
    expect(root.querySelector('.chat-title')!.textContent).toBe('Network sync');
    expect(root.querySelector('.chat-head .back')!.getAttribute('href')).toBe('#/p/p1');
    await go('#/');
    await click(root.querySelector('.phone-view'));
    await click(root.querySelector('.menu .btn-view-projects'));
    expect(viewTitle()).toBe('Projects');
    expect(phoneAll()).toHaveLength(4);
  });

  it('`#/p/<id>` is the project: its favorites pinned over the groups; "+" opens its new session', async () => {
    const { app } = phoneApp();
    app.sessions.set('cave', { ...app.sessions.get('cave')!, favorite_at: at(1) });
    await open(app, '#/p/p1');
    expect(root.querySelector('.layout > .sessions-phone > .side-head .project-title')!.textContent).toBe('physics-mp');
    expect(root.querySelector('.sessions-phone > .fav-pinned .session-title')!.textContent).toBe('Cave scene port');
    expect(groups().map((g) => g[0])).toEqual(['Favorites', 'Needs you', 'Running', 'Idle · Yesterday']);
    expect(root.querySelector('.phone-home, .chips, .fab, .wline')).toBeNull();
    await click(root.querySelector('.side-head .btn-new-session'));
    await settle();
    expect(location.hash).toBe('#/p/p1/new');
    expect(app.newSession?.projectId).toBe('p1');
    expect(root.querySelector('.layout-phone > .main .chat-new')).not.toBeNull();
  });

  it('archiving the open project from its "⋯" goes to the projects, with Undo there', async () => {
    const { app, api } = phoneApp();
    const bar = [...app.projects.values()];
    const unarchiveProject = vi.fn(async (_id: string) => {});
    Object.assign(api, {
      archiveProject: vi.fn(async (id: string) => ({ ...app.archivedProjects.get(id)! })),
      unarchiveProject,
      projects: vi.fn(async () => bar),
    });
    await open(app, '#/p/p3');
    await click(root.querySelector('.side-head .btn-project-menu'));
    await click(root.querySelector('.menu .btn-archive-project'));
    await settle();
    expect(location.hash).toBe('#/');
    expect(phoneAll()).toEqual(['boards', 'physics-mp', 'Personal']);
    const toast = root.querySelector('.phone-home .toast-project')!;
    expect(toast.querySelector('.toast-text')!.textContent).toBe('Archived «kiln»');
    await click(toast.querySelector('.btn-undo'));
    await settle();
    expect(root.querySelector('.toast-project')).toBeNull();
    expect(unarchiveProject).toHaveBeenCalledWith('p3');
    expect(phoneAll()).toEqual(['boards', 'physics-mp', 'kiln', 'Personal']);
    // The hub refuses an archive: the project is back, and why is said where the phone is.
    Object.assign(api, { archiveProject: vi.fn(async () => Promise.reject(new Error('offline'))) });
    await go('#/p/p3');
    await click(root.querySelector('.side-head .btn-project-menu'));
    await click(root.querySelector('.menu .btn-archive-project'));
    await settle();
    expect(location.hash).toBe('#/');
    expect(root.querySelector('.phone-home .notice')!.textContent).toBe('Not archived «kiln»: offline');
    expect(phoneAll()).toContain('kiln');
  });

  it("follows the account's look: Spacious, the counts under a project's name and the project and state under a session's title; Compact, one line", async () => {
    const subs = () => [...root.querySelectorAll('.phone-home a.phone-project')].map((r) => r.querySelector('.phone-project-sub')?.textContent ?? null);
    await open(phoneApp('spacious').app, '#/');
    expect(document.documentElement.classList.contains('list-compact')).toBe(false);
    expect(subs()).toEqual([null, '1 waiting · 2 running', '2 running', null]);
    expect(phoneRow('physics-mp').querySelector('.phone-project-sub')!.classList.contains('is-wait')).toBe(true);
    await click(root.querySelector('.phone-view'));
    await click(root.querySelector('.menu .btn-view-sessions'));
    expect(rowOf('Network sync').querySelector('.session-sub')!.textContent).toBe('physics-mp · Waiting for reply');
    expect(rowOf('Firing curve').querySelector('.session-sub')!.textContent).toBe('kiln · Running · 1 agent');
    expect(root.querySelector('.session-ag')).toBeNull();

    render(null, root);
    const compact = phoneApp('compact').app;
    await open(compact, '#/');
    // The system font and the one-line list (styles.css below).
    expect(['list-compact', 'font-system'].map((c) => document.documentElement.classList.contains(c))).toEqual([true, true]);
    expect(subs()).toEqual([null, null, null, null]);
    await click(root.querySelector('.phone-view'));
    await click(root.querySelector('.menu .btn-view-sessions'));
    expect(rowOf('Glaze test').querySelector('.session-top .session-ag')!.textContent).toBe('2 agents');
    await go('#/p/p1');
    expect(root.querySelector('.sessions-phone .session-row')).not.toBeNull();
    expect(root.querySelectorAll('.sessions-phone .session-top .session-ag')).toHaveLength(0);
    compact.sessions.set('stones', { ...compact.sessions.get('stones')!, subagents: 1 });
    await act(async () => (compact as unknown as { changed(): void }).changed());
    expect(rowOf('Stone textures').querySelector('.session-top .session-ag')!.textContent).toBe('1 agent');

    const css = readFileSync(join(import.meta.dirname, '../src/styles.css'), 'utf8');
    // Compact: a 36 px header, 22 px group lines (6 + 14 + 2), 30 px rows of one line, 18 px tiles.
    expect(css).toMatch(/:root\.list-compact \.sessions-phone \.side-head \{\s*min-height: calc\(36px /);
    expect(css).toMatch(/:root\.list-compact \.sessions-phone \.group-head \{\s*padding: 6px 12px 2px;[^}]*line-height: 14px;/);
    expect(css).toMatch(/:root\.list-compact \.sessions-phone \.session-item > \.session-row \{[^}]*height: 30px;/);
    expect(css).toMatch(/:root\.list-compact \.sessions-phone \.session-item \.session-sub \{\s*display: none;/);
    expect(css).toMatch(/:root\.list-compact \.phone-project \{\s*height: 30px;/);
    expect(css).toMatch(/:root\.list-compact \.phone-tile \{\s*width: 18px;/);
    // Spacious: 42 px project tiles.
    expect(css).toMatch(/\n\.phone-tile \{[^}]*width: 42px;/);
  });

  it('"‹" steps back through the history: the back gesture from the project then goes to the projects, not to the session it closed', async () => {
    trackHistory();
    const { app } = phoneApp();
    await open(app, '#/');
    await click(phoneRow('physics-mp'));
    await settle();
    await click(root.querySelector('a.session-row[href="#/s/stones"]'));
    await settle();
    expect(location.hash).toBe('#/s/stones');
    await click(root.querySelector('.chat-head a.back'));
    await settle();
    expect(location.hash).toBe('#/p/p1');
    // The back gesture.
    await act(async () => history.back());
    await settle();
    expect(location.hash).toBe('#/');
    expect(viewTitle()).toBe('Projects');
  });
});
