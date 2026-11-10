// @vitest-environment happy-dom
// T26: the session list — the "State" and "Recent" orders, the idle sessions by recency, the
// search over title and path, the workers line with the running sessions of the open session's
// machine, the project rail with its "needs you" badge. T50: the order on the hub (State /
// Recent / Custom, Reorder by the keyboard), the delayed archive with Undo, archive mode. The
// one row menu (a right click; a long press on a phone), the row's card, the one-row header
// with the order and project menus, the pinned favorites, the column's width.

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { render } from 'preact';
import { act } from 'preact/test-utils';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import type { Api } from '../src/api';
import { ARCHIVE_UNDO_MS, AppState } from '../src/app';
import { parseEvent, type ListMode, type SessionInfo, type SessionState } from '../src/proto';
import { applyLook } from '../src/theme';
import { Timeline } from '../src/timeline';
import type { ConnState, HubSocket } from '../src/ws';
import { App } from '../src/ui/App';
import { ChatItems, type ChatActions } from '../src/ui/cards';
import { Chat } from '../src/ui/Chat';
import { ProjectRail } from '../src/ui/ProjectRail';
import { LIST_WIDTH_MAX, LIST_WIDTH_MIN, LIST_WIDTH_STEP, SessionList, WidthGrip } from '../src/ui/SessionList';
import {
  arrange,
  bucketOf,
  groupSessions,
  lastLine,
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

  it("the last line of a turn: what the agent said, thought or ran last, or the user's message", () => {
    const ev = (seq: number, type: string, payload: object) =>
      parseEvent({
        session_id: 'net',
        event_id: `0199f3a2-7c10-7000-8000-${String(seq).padStart(12, '0')}`,
        seq,
        ts: at(1),
        author: { kind: type === 'user.message' ? 'user' : 'agent', id: 'net' },
        type,
        payload,
      });
    const said = ev(1, 'agent.message', { text: 'Synced\nall of it.', final: true });
    const thought = ev(2, 'agent.thinking', { text: 'The user wants the map' });
    const ran = ev(3, 'tool.call', { call_id: 'c1', tool: 'Bash', args_preview: '{"command":"git log -1"}' });
    const result = ev(4, 'tool.result', { call_id: 'c1', result_preview: 'abc', error: false });
    expect(lastLine([])).toBeUndefined();
    expect(lastLine([said])).toBe('Synced all of it.');
    expect(lastLine([said, thought])).toBe('Thinking · The user wants the map');
    expect(lastLine([said, thought, ran, result])).toBe('$ git log -1');
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
  app.me = { id: 'u', email: 'who@example.com', personal_project_id: 'p0', theme: 'warm' };
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

const groups = () =>
  [...root.querySelectorAll('.session-group')].map((g) => [
    g.querySelector('.group-label')!.textContent,
    // A custom order has no count.
    g.querySelector('.group-count')?.textContent,
    [...g.querySelectorAll('.session-title')].map((t) => t.textContent),
  ]);

/** Types into the desktop column's search, opening it from the header's magnifier first. */
async function type(value: string) {
  if (!root.querySelector('input[aria-label="Search sessions"]')) await click(root.querySelector('.side-head .btn-search'));
  const input = root.querySelector<HTMLInputElement>('input[aria-label="Search sessions"]')!;
  await act(() => {
    input.value = value;
    input.dispatchEvent(new Event('input', { bubbles: true }));
  });
}

/** A right click on a row (a long press on a phone does the same): its menu. */
const menuOf = (title: string, x = 40, y = 120) =>
  act(async () => {
    rowOf(title).dispatchEvent(new MouseEvent('contextmenu', { bubbles: true, cancelable: true, clientX: x, clientY: y }));
  });
const menuItems = () => [...root.querySelectorAll('.menu .menu-item')].map((b) => b.textContent);
const SESSION_MENU = ['Open in new tab', 'Add to favorites', 'Rename', 'Move to project…', 'Folder rules', 'Archive', 'Delete…'];
/** Archives a row from its menu's round button. */
async function archiveRow(title: string) {
  await menuOf(title);
  await click(root.querySelector('.btn-menu-archive'));
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
      'Project: settings, favorites',
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
    // Two lines: the title, the state under it; the worker and the folder are in the card.
    expect(net.querySelector('.session-sub')!.textContent).toBe('Waiting for reply');
    expect(net.querySelector('.session-where')).toBeNull();
    expect(net.querySelector('.dot-wait')).not.toBeNull();
    expect(root.querySelector('.session-when')).toBeNull();
    expect(net.getAttribute('title')).toBeNull();
    // No buttons on a row.
    expect(root.querySelector('.session-item button')).toBeNull();
  });

  it('the compact look: a line per row, the subagents after the title, no tooltip (Settings → Session list)', async () => {
    const app = makeApp(LIST.map((s) => (s.id === 'stones' ? { ...s, subagents: 1 } : s)));
    app.look = { ...app.look, compactList: true };
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
    for (const compactList of [false, true]) {
      const app = makeApp(list);
      app.look = { ...app.look, compactList };
      await mount(<SessionList app={app} />);
      expect(rowOf('Fork in VS Code').querySelector('.dot-vs svg')).not.toBeNull();
      expect(rowOf('Fork in VS Code').querySelector('.dot')).toBeNull();
      expect(rowOf('Busy in VS Code').querySelector('.dot-run')).not.toBeNull();
      expect(groups()[1]).toEqual(['Running', '3', ['Stone textures', 'Step determinism', 'Busy in VS Code']]);
      render(null, root);
    }
  });

  it("a row's card: after the mouse rests on it, right of the column; a pass shows nothing", async () => {
    vi.useFakeTimers({ toFake: ['Date', 'setTimeout', 'clearTimeout'] });
    vi.setSystemTime(NOW);
    const app = makeApp();
    app.chat('net').timeline.applyAll([
      parseEvent({
        session_id: 'net',
        event_id: '0199f3a2-7c10-7000-8000-000000000201',
        seq: 1,
        ts: at(0.2),
        author: { kind: 'agent', id: 'net' },
        type: 'agent.thinking',
        payload: { text: 'The user wants the map' },
      }),
    ]);
    await mount(<SessionList app={app} />);
    const enter = (title: string) =>
      act(async () => {
        rowOf(title).dispatchEvent(new PointerEvent('pointerenter', { pointerType: 'mouse' }));
      });
    const leave = (title: string) =>
      act(async () => {
        rowOf(title).dispatchEvent(new PointerEvent('pointerleave', { pointerType: 'mouse' }));
      });
    const wait = (ms: number) =>
      act(async () => {
        vi.advanceTimersByTime(ms);
      });
    // Passing over a row.
    await enter('Network sync');
    await wait(300);
    await leave('Network sync');
    await wait(1000);
    expect(root.querySelector('.session-card')).toBeNull();

    await enter('Network sync');
    await wait(499);
    expect(root.querySelector('.session-card')).toBeNull();
    await wait(1);
    const card = root.querySelector<HTMLElement>('.session-card')!;
    expect(card.getAttribute('role')).toBe('tooltip');
    expect([...card.children].map((c) => c.textContent)).toEqual([
      'Network sync',
      'Waiting for reply · 12 min ago',
      'home-mac · ~/work/games/phys-mp/net',
      'Thinking · The user wants the map',
    ]);
    // Right of the column (no layout here: the column ends at 0).
    expect(card.style.left).toBe('12px');
    await leave('Network sync');
    expect(root.querySelector('.session-card')).toBeNull();

    // A right click takes it away for the menu.
    await enter('Cave scene port');
    await wait(500);
    expect(root.querySelector('.session-card .session-card-title')!.textContent).toBe('Cave scene port');
    expect(root.querySelector('.session-card .session-card-last')).toBeNull();
    await menuOf('Cave scene port');
    expect(root.querySelector('.session-card')).toBeNull();
    expect(root.querySelector('.ctx-menu')).not.toBeNull();
    // The menu's session is deleted on another device: the menu goes, the cards come back.
    await act(async () => app.removeSession('cave'));
    expect(root.querySelector('.ctx-menu')).toBeNull();
    await enter('Network sync');
    await wait(500);
    expect(root.querySelector('.session-card .session-card-title')!.textContent).toBe('Network sync');

    // Another row: the card shown goes at once, before the next one's time.
    await enter('Step determinism');
    expect(root.querySelector('.session-card')).toBeNull();
    // The row moves to another group before its time (Running → Idle): no card, anywhere.
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
    await wait(500);
    expect(root.querySelector('.session-card')).toBeNull();
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

  it('creates a session with a model from the form', async () => {
    const base = hubApi();
    let app!: AppState;
    const api = { ...base, workspaces: async () => app.workspaces.get('p1') ?? [], workers: async () => [] };
    app = makeApp(LIST, api as unknown as ReturnType<typeof hubApi>);
    await mount(<SessionList app={app} />);
    await click(root.querySelector('.btn-new-session'));
    await act(async () => void (await new Promise((r) => setTimeout(r, 10))));
    const set = (name: string, value: string) =>
      act(async () => {
        const el = root.querySelector<HTMLInputElement>(`.new-session [name=${name}]`)!;
        el.value = value;
        el.dispatchEvent(new Event('input', { bubbles: true }));
        el.dispatchEvent(new Event('change', { bubbles: true }));
      });
    await set('model', 'haiku');
    await act(async () => {
      root.querySelector('form.new-session')!.dispatchEvent(new Event('submit', { bubbles: true, cancelable: true }));
      for (let i = 0; i < 5; i++) await Promise.resolve();
    });
    expect(base.createSession).toHaveBeenCalledWith('p1', undefined, {
      workspace_id: 'w1',
      cwd: '',
      permission_mode: undefined,
      model: 'haiku',
    });
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

  it('searches by title and by path from the magnifier, the field in the place of the name', async () => {
    await mount(<SessionList app={makeApp()} />);
    expect(root.querySelector('input[aria-label="Search sessions"]')).toBeNull();
    await type('scene');
    expect(root.querySelector('.side-head .project-title')).toBeNull();
    expect(document.activeElement).toBe(root.querySelector('input[aria-label="Search sessions"]'));
    expect(groups()).toEqual([['Idle · Last 7 days', '1', ['Cave scene port']]]);
    await type('phys-det');
    expect(groups()).toEqual([['Running', '1', ['Step determinism']]]);
    await type('no such thing');
    expect(groups()).toEqual([]);
    expect(root.querySelector('.empty')!.textContent).toBe('No sessions match');
    // Esc drops the search and brings the name back.
    await act(async () => {
      root.querySelector('input[aria-label="Search sessions"]')!.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true }));
    });
    expect(root.querySelector('input[aria-label="Search sessions"]')).toBeNull();
    expect(root.querySelector('.side-head .project-title')!.textContent).toBe('physics-mp');
    expect(groups()).toHaveLength(4);
    expect(document.activeElement).toBe(root.querySelector('.side-head .btn-search'));
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
    // The menu stands at the click, the row under it lit; the archive is a round button past
    // the menu's right edge, and an item in it.
    await menuOf('Network sync', 40, 120);
    const menu = root.querySelector<HTMLElement>('.ctx-menu')!;
    expect([menu.style.left, menu.style.top]).toEqual(['40px', '120px']);
    expect(menu.getAttribute('aria-label')).toBe('Session «Network sync»');
    expect(rowOf('Network sync').querySelector('.session-row.is-menu')).not.toBeNull();
    expect(menuItems()).toEqual(SESSION_MENU);
    expect(menu.querySelectorAll('.menu-sep')).toHaveLength(2);
    const out = menu.querySelector('.menu-out.btn-menu-archive')!;
    expect(out.getAttribute('aria-label')).toBe('Archive «Network sync»');
    // The first item has the focus; ↓ goes on, ↑ from the first goes round to the last.
    expect(document.activeElement!.textContent).toBe('Open in new tab');
    await pressKey('ArrowDown');
    expect(document.activeElement!.textContent).toBe('Add to favorites');
    await pressKey('ArrowUp');
    await pressKey('ArrowUp');
    expect(document.activeElement!.textContent).toBe('Delete…');

    await click(out);
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
    // No archive button beside the question.
    expect(menu.querySelector('.btn-menu-archive')).toBeNull();
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

  it('with names on, a row per project: its icon on its color and its full name (SPEC 0.91)', async () => {
    const app = makeApp();
    // Not setLook: it would keep the names in localStorage for the tests below.
    app.look = { ...app.look, railNames: true };
    app.projects.set('p2', { id: 'p2', name: 'ukuetis_controller_backend', slug: 'ukuetis', description: '', is_personal: false, color: 'moss', created_at: at(80) });
    await mount(<ProjectRail app={app} />);
    const rows = () => [...root.querySelectorAll<HTMLElement>('.rail-project')];
    expect(rows().map((r) => [r.querySelector('.rail-glyph')!.textContent, r.querySelector('.rail-name')!.textContent])).toEqual([
      ['P', 'Personal'],
      ['⚛️', 'physics-mp'],
      ['UC', 'ukuetis_controller_backend'],
    ]);
    // The hub's name next to its mark; "+ project", Reorder, the settings and the avatar in one
    // row at the bottom.
    expect(root.querySelector('.rail-logo')!.textContent).toBe('HQC');
    expect(root.querySelector('.rail-actions')).toBeNull();
    expect([...root.querySelectorAll('.rail-foot > *')].map((b) => b.getAttribute('aria-label'))).toEqual([
      'New project',
      'Reorder projects',
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
    await act(async () => root.querySelector<HTMLButtonElement>('.rail-reorder')!.click());
    const grip = root.querySelector<HTMLElement>('.rail-drag[data-tile="p0"]')!;
    expect(grip.querySelector('.rail-name')!.textContent).toBe('Personal');
    const ev = (type: string, y: number) =>
      act(async () => {
        grip.dispatchEvent(new PointerEvent(type, { bubbles: true, cancelable: true, pointerId: 1, clientY: y }));
      });
    await ev('pointerdown', 100);
    await ev('pointermove', 180);
    await ev('pointerup', 180);
    expect(rows().map((r) => r.getAttribute('title'))).toEqual(['physics-mp', 'ukuetis_controller_backend', 'Personal']);
  });

  it('marks a project with unread events by a dot, unless someone waits there', async () => {
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
    await mount(<SessionList app={app} phone />);
    expect([...root.querySelectorAll('.chips .chip.project-row')].map((c) => !!c.querySelector('.chip-unread'))).toEqual([true, false]);
    app.sessions.set('notes', { ...app.sessions.get('notes')!, unread: 0 });
    await mount(<ProjectRail app={app} />);
    // Archived and service sessions do not count.
    expect(dots()[0]).toEqual([false, false]);
  });

  it('marks a project whose session works by a pulsing dot of its color, else an idle one by a ring', async () => {
    const app = makeApp([
      sess('notes', 'Notes', 'running', at(0.5), { project_id: 'p0', unread: 1 }),
      sess('det', 'Step determinism', 'idle', at(0.1)),
      sess('svc', 'wk1', 'running', at(0.1), { system: true }),
      sess('gone', 'Gone', 'archived', at(0.5), { state: 'running' }),
    ]);
    const marks = () =>
      [...root.querySelectorAll('.rail-item')].map((i) => i.querySelector('.rail-badge, .rail-run, .rail-unread, .rail-idle')?.className);
    await mount(<ProjectRail app={app} />);
    // Personal works, over its unread; physics-mp: a service and an archived session do not count.
    expect(marks()).toEqual(['rail-run', 'rail-idle']);
    expect(root.querySelector<HTMLElement>('.rail-run')!.style.background).toBe('#8A93A6');
    await mount(<SessionList app={app} phone />);
    const chips = () =>
      [...root.querySelectorAll('.chips .chip.project-row')].map((c) => c.querySelector('.chip-badge, .chip-run, .chip-unread, .chip-idle')?.className);
    expect(chips()).toEqual(['chip-run', 'chip-idle']);
    expect(root.querySelector<HTMLElement>('.chip-run')!.style.background).toBe('#8A93A6');
    // Queued input works too; a session waiting for you wins over a working one.
    app.sessions.set('det', { ...app.sessions.get('det')!, state: 'queued' });
    await mount(<ProjectRail app={app} />);
    expect(marks()).toEqual(['rail-run', 'rail-run']);
    app.sessions.set('net', sess('net', 'Network sync', 'waiting', at(0.2)));
    await mount(<ProjectRail app={app} />);
    expect(marks()).toEqual(['rail-run', 'rail-badge']);
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
  it('one screen: projects as chips, the same groups, a floating "New"', async () => {
    await mount(<SessionList app={makeApp()} phone />);
    expect(root.querySelector('.m-head h1')!.textContent).toBe('Sessions');
    const chips = [...root.querySelectorAll('.chips .chip.project-row')];
    // T49: the icon before the name; without one a dot of the project's color.
    expect(chips.map((c) => c.textContent)).toEqual(['Personal', '⚛️physics-mp1']);
    expect(chips[0].querySelector<HTMLElement>('.chip-dot')!.style.background).toBe('#8A93A6');
    expect(root.querySelector('.chip.is-current .chip-badge')!.textContent).toBe('1');
    expect(groups().map((g) => g[0])).toEqual(['Needs you', 'Running', 'Idle · Yesterday', 'Idle · Last 7 days']);
    expect(root.querySelector('.fab')!.textContent).toBe('New');
    // The desktop's header, workers line, pinned favorites and width handle are not here.
    expect(root.querySelector('.side-head, .wline, .fav-pinned, .col-grip')).toBeNull();
    // The search is a screen of its own (T33, tests/search.test.tsx).
    expect(root.querySelector('input[aria-label="Search sessions"]')).toBeNull();
    expect(root.querySelector('a[aria-label="Search sessions and messages"]')!.getAttribute('href')).toBe('#/search');
    expect(root.querySelector('.session-item button')).toBeNull();
    expect(root.querySelector('.list-hint')!.textContent).toBe('Press and hold a session to archive, rename or delete it.');
  });

  it('keeps its rows as they are with "Compact" on: their state and place stay visible, no tooltip', async () => {
    const app = makeApp();
    app.look = { ...app.look, compactList: true };
    await mount(<SessionList app={app} phone />);
    const rows = [...root.querySelectorAll('.session-row')];
    expect(rows.length).toBeGreaterThan(0);
    expect(rows.filter((r) => r.hasAttribute('title'))).toEqual([]);
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
    expect(menu.querySelector('.btn-menu-archive')).not.toBeNull();
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

  it('Reorder puts the handle on the right of the row', async () => {
    await mount(<SessionList app={makeApp()} phone />);
    await click(root.querySelector('.btn-reorder'));
    const item = rowOf('Network sync');
    expect(item.lastElementChild!.classList.contains('grip')).toBe(true);
    expect(root.querySelector('.fab')).toBeNull();
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
    // The search narrows them too; Reorder hides them.
    await type('cave');
    expect(favoriteTitles()).toEqual(['Cave scene port']);
    await type('');
    await startReorder();
    expect(root.querySelector('.group-favorites')).toBeNull();
  });

  it('on a phone the favorites are the first group of the scrolling list', async () => {
    await mount(<SessionList app={makeApp(LIST.map((s) => (s.id === 'cave' ? { ...s, favorite_at: at(30) } : s)))} phone />);
    expect(root.querySelector('.session-scroll > .group-favorites')).not.toBeNull();
    expect(root.querySelector('.fav-pinned')).toBeNull();
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
    const menu = root.querySelector<HTMLElement>('.rail-menu')!;
    expect([menu.style.left, menu.style.top]).toEqual(['30px', '200px']);
    expect(menuItems()).toEqual(['Add to favorites']);
    await click(root.querySelector('.rail-menu .btn-favorite'));
    expect(api.setProjectFavorite).toHaveBeenCalledWith('p3', true);
    expect(root.querySelector('.rail-menu')).toBeNull();
    expect(order()).toEqual(['boards', 'kiln', 'Personal', 'physics-mp']);

    await act(async () => {
      tile('boards').dispatchEvent(new MouseEvent('contextmenu', { bubbles: true, cancelable: true }));
    });
    expect(menuItems()).toEqual(['Remove from favorites']);
    // Esc closes it without a change.
    await act(async () => {
      document.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true }));
    });
    expect(root.querySelector('.rail-menu')).toBeNull();
    await act(async () => {
      tile('boards').dispatchEvent(new MouseEvent('contextmenu', { bubbles: true, cancelable: true }));
    });
    await click(root.querySelector('.rail-menu .btn-favorite'));
    expect(api.setProjectFavorite).toHaveBeenLastCalledWith('p2', false);
    expect(order()).toEqual(['kiln', 'Personal', 'physics-mp', 'boards']);
  });
});

describe('the list on a reload', () => {
  const ME = { id: 'u', email: 'who@example.com', personal_project_id: 'p0', theme: 'warm' as const };
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
  it('"⋯": Open in new tab, favorites, Project settings…, Delete project… (not for the personal project)', async () => {
    const api = { ...hubApi(), deleteProject: vi.fn(async () => {}) };
    const app = makeApp(LIST, api);
    await mount(<SessionList app={app} />);
    await click(root.querySelector('.side-head .btn-project-menu'));
    const menu = root.querySelector('.ctx-menu')!;
    expect(menu.getAttribute('aria-label')).toBe('Project «physics-mp»');
    expect(menuItems()).toEqual(['Open in new tab', 'Add to favorites', 'Project settings…', 'Delete project…']);
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
      expect(app.look.listWidth).toBeUndefined();
      // 20 px to the right: the column is 20 px wider, not 19.
      await ev('pointerdown', 100);
      await ev('pointermove', 120);
      expect(css()).toBe('320px');
      expect(grip.getAttribute('aria-valuenow')).toBe('320');
      expect(app.look.listWidth).toBeUndefined();
      await ev('pointerup', 120);
      expect(app.look.listWidth).toBe(320);
      expect(JSON.parse(localStorage.getItem('hqc.look')!).listWidth).toBe(320);
      expect(new AppState(hubApi() as unknown as Api, { onState: () => () => {} } as unknown as HubSocket).look.listWidth).toBe(320);
      await ev('pointerdown', 0);
      await ev('pointermove', 5000);
      await ev('pointerup', 5000);
      expect(app.look.listWidth).toBe(LIST_WIDTH_MAX);
      await act(async () => {
        grip.dispatchEvent(new KeyboardEvent('keydown', { key: 'ArrowLeft', bubbles: true }));
      });
      expect(app.look.listWidth).toBe(LIST_WIDTH_MAX - LIST_WIDTH_STEP);
      await ev('pointerdown', 5000);
      await ev('pointermove', -5000);
      await ev('pointerup', -5000);
      expect(app.look.listWidth).toBe(LIST_WIDTH_MIN);
      // The root carries it as `--list-w`; without one the default comes back.
      applyLook(document.documentElement, app.look);
      expect(css()).toBe(`${LIST_WIDTH_MIN}px`);
      applyLook(document.documentElement, { ...app.look, listWidth: undefined });
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
    // The search field: Esc closes it, and the card does not take the key.
    await type('net');
    await pressKey('Escape');
    expect(root.querySelector('input[aria-label="Search sessions"]')).toBeNull();
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
    expect(root.querySelector('.rail-menu')).not.toBeNull();
    expect(root.querySelector('.rail-menu')!.contains(document.activeElement)).toBe(true);
    await pressKey('Enter');
    await pressKey('Escape');
    expect(root.querySelector('.rail-menu')).toBeNull();
    expect(actions.approve).not.toHaveBeenCalled();
    // The menu closed: the card has its keys.
    (document.activeElement as HTMLElement).blur();
    await pressKey('Enter');
    expect(actions.approve).toHaveBeenCalledWith('req-1', 'allow');
  });
});
