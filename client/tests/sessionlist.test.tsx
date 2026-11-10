// @vitest-environment happy-dom
// T26: the session list — the "State" and "Recent" orders, the search over title and path,
// the project's counters, workers with their active sessions, the project rail with its
// "needs you" badge. T50: the order on the hub (State / Recent / Custom, Reorder by the
// keyboard), the row actions, the delayed archive with Undo, archive mode, the phone's sheet.

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { render } from 'preact';
import { act } from 'preact/test-utils';
import type { Api } from '../src/api';
import { ARCHIVE_UNDO_MS, AppState } from '../src/app';
import type { ListMode, SessionInfo, SessionState } from '../src/proto';
import type { ConnState, HubSocket } from '../src/ws';
import { App } from '../src/ui/App';
import { ProjectRail } from '../src/ui/ProjectRail';
import { SessionList } from '../src/ui/SessionList';
import { activeLabel, arrange, bucketOf, groupSessions, headCounts, matchesQuery, moveTo, projectInitials, subLabel } from '../src/ui/sessions';

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
      ['Idle', ['free']],
    ]);
    expect(subLabel(busy)).toBe('Running');
    expect(subLabel(free)).toBe('Idle');
  });

  it('an idle session whose background task still runs, with no subagent, is in Running and says so', () => {
    const busy = sess('make', 'Two conversion runs', 'idle', at(1), { background_tasks: 1 });
    const free = sess('free', 'Nothing left', 'idle', at(2));
    expect(groupSessions([busy, free], 'state', NOW).map((g) => [g.label, g.items.map((s) => s.id)])).toEqual([
      ['Running', ['make']],
      ['Idle', ['free']],
    ]);
    expect(subLabel(busy)).toBe('Running');
  });

  it('groups by state and by recency, newest first, empty groups left out', () => {
    expect(groupSessions(LIST, 'state', NOW).map((g) => [g.label, g.items.map((s) => s.id)])).toEqual([
      ['Needs you', ['net']],
      ['Running', ['stones', 'det']],
      ['Idle', ['prof', 'cave']],
    ]);
    expect(groupSessions(LIST, 'recent', NOW).map((g) => [g.label, g.items.map((s) => s.id)])).toEqual([
      ['Today', ['stones', 'det', 'net']],
      ['Yesterday', ['prof']],
      ['Earlier', ['cave']],
    ]);
    expect(groupSessions(LIST.slice(3), 'state', NOW).map((g) => g.label)).toEqual(['Idle']);
  });

  it('counts for the header: waiting, working, and the idle ones only when one of those is zero', () => {
    expect(headCounts(LIST)).toBe('1 needs you · 2 running');
    expect(headCounts(LIST.filter((s) => s.state !== 'running'))).toBe('1 needs you · 2 idle');
    expect(headCounts(LIST.slice(3))).toBe('2 idle');
    expect(headCounts(LIST.slice(0, 1))).toBe('1 running');
    expect(headCounts([])).toBe('No sessions yet');
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
    expect([0, 1, 2, 5, 11, 21].map(activeLabel)).toEqual([
      'none active',
      '1 active',
      '2 active',
      '5 active',
      '11 active',
      '21 active',
    ]);
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

async function type(value: string) {
  const input = root.querySelector<HTMLInputElement>('input[aria-label="Search sessions"]')!;
  await act(() => {
    input.value = value;
    input.dispatchEvent(new Event('input', { bubbles: true }));
  });
}

describe('SessionList (desktop column)', () => {
  it('shows the project, its counters and the groups by state', async () => {
    await mount(<SessionList app={makeApp()} current="net" />);
    expect(root.querySelector('.project-title')!.textContent).toBe('physics-mp');
    expect(root.querySelector('.side-counts')!.textContent).toBe('1 needs you · 2 running');
    expect(groups()).toEqual([
      ['Needs you', '1', ['Network sync']],
      ['Running', '2', ['Stone textures', 'Step determinism']],
      ['Idle', '2', ['Collision profiling', 'Cave scene port']],
    ]);
    const net = root.querySelector('.session-row.is-current')!;
    expect(net.getAttribute('aria-current')).toBe('page');
    expect(net.querySelector('.session-sub')!.textContent).toBe('Waiting for reply');
    expect(net.querySelector('.session-where')!.textContent).toBe('home-mac · games/phys-mp/net');
    expect(net.querySelector('.dot-wait')).not.toBeNull();
    expect(root.querySelector('.session-when')).toBeNull();
    expect(net.getAttribute('title')).toBeNull();
  });

  it('in the compact list a row keeps its state and place in its tooltip (Settings → Session list)', async () => {
    const app = makeApp();
    app.look = { ...app.look, compactList: true };
    await mount(<SessionList app={app} current="net" />);
    const net = root.querySelector('.session-row.is-current')!;
    expect(net.getAttribute('title')).toBe('Waiting for reply\nhome-mac · games/phys-mp/net');
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

  it('switches to "Recent" with times, and keeps the choice on the hub (T50)', async () => {
    const api = hubApi();
    await mount(<SessionList app={makeApp(LIST, api)} />);
    expect(api.sessionOrder).toHaveBeenCalledWith('p1');
    const recent = [...root.querySelectorAll<HTMLButtonElement>('.seg')].find((b) => b.textContent === 'Recent')!;
    await act(async () => recent.click());
    expect(groups().map((g) => g[0])).toEqual(['Today', 'Yesterday', 'Earlier']);
    expect([...root.querySelectorAll('.session-when')].map((w) => w.textContent)).toEqual([
      'now',
      '6 min',
      '12 min',
      'yesterday',
      'Oct 22',
    ]);
    expect(api.putSessionOrder).toHaveBeenCalledWith('p1', { mode: 'recent' });
    render(null, root);
    // Another device: the hub's record.
    await mount(<SessionList app={makeApp(LIST, hubApi({ mode: 'recent', session_ids: [] }))} />);
    expect(root.querySelector('.seg[aria-pressed="true"]')!.textContent).toBe('Recent');
    expect(groups()[0][0]).toBe('Today');
  });

  it('searches by title and by path', async () => {
    await mount(<SessionList app={makeApp()} />);
    await type('scene');
    expect(groups()).toEqual([['Idle', '1', ['Cave scene port']]]);
    await type('phys-det');
    expect(groups()).toEqual([['Running', '1', ['Step determinism']]]);
    await type('no such thing');
    expect(groups()).toEqual([]);
    expect(root.querySelector('.empty')!.textContent).toBe('No sessions match');
  });

  it('counts an idle session whose subagents still run as active on its worker', async () => {
    const idle = LIST.find((s) => s.worker_id === 'wk1' && s.state === 'idle')!;
    const list = LIST.map((s) => (s.id === idle.id ? { ...s, subagents: 2 } : s));
    await mount(<SessionList app={makeApp(list)} />);
    const home = [...root.querySelectorAll('.worker-list .worker-row')].find((r) => r.querySelector('.worker-name')!.textContent === 'home-mac')!;
    expect(home.querySelector('.worker-state')!.textContent).toBe('3 active');
  });

  it('counts an idle session whose background tasks still run as active on its worker', async () => {
    const idle = LIST.find((s) => s.worker_id === 'wk1' && s.state === 'idle')!;
    const list = LIST.map((s) => (s.id === idle.id ? { ...s, background_tasks: 1 } : s));
    await mount(<SessionList app={makeApp(list)} />);
    const home = [...root.querySelectorAll('.worker-list .worker-row')].find((r) => r.querySelector('.worker-name')!.textContent === 'home-mac')!;
    expect(home.querySelector('.worker-state')!.textContent).toBe('3 active');
  });

  it('lists the workers with their active sessions, offline ones grey', async () => {
    await mount(<SessionList app={makeApp()} />);
    const rows = [...root.querySelectorAll('.worker-list .worker-row')];
    expect(rows.map((r) => [r.querySelector('.worker-name')!.textContent, r.querySelector('.worker-state')!.textContent])).toEqual([
      ['drzlo-mac', 'offline'],
      ['home-mac', '2 active'],
    ]);
    expect(rows[0].querySelector('.dot-offline')).not.toBeNull();
    expect(rows[1].getAttribute('href')).toBe('#/s/sys1');
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
    const net = rowOf('Network sync');
    expect(net.querySelector('.btn-row-archive')!.getAttribute('aria-label')).toBe('Archive «Network sync»');
    expect(net.querySelector('.btn-row-more')).not.toBeNull();

    await click(net.querySelector('.btn-row-archive'));
    expect(titles()).not.toContain('Network sync');
    expect(root.querySelector('.toast')!.textContent).toBe('Archived «Network sync»Undo');
    expect(root.querySelector('.side-counts')!.textContent).toBe('2 running · 2 idle');
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

    // Again, and the toast runs out: now the hub, and the session is in the archive.
    await click(rowOf('Network sync').querySelector('.btn-row-archive'));
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
    await click(rowOf('Network sync').querySelector('.btn-row-archive'));
    await click(rowOf('Cave scene port').querySelector('.btn-row-archive'));
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
    await click(rowOf('Stone textures').querySelector('.btn-row-archive'));
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
    await click(rowOf('Stone textures').querySelector('.btn-row-archive'));
    expect(titles()).toEqual(['Cave scene port', 'Network sync', 'Step determinism', 'Collision profiling']);
    await click(root.querySelector('.btn-undo'));
    expect(titles()).toEqual(['Cave scene port', 'Network sync', 'Stone textures', 'Step determinism', 'Collision profiling']);
    await act(async () => void (await app.createSession('New session', {}, 'p1')));
    expect(titles()[0]).toBe('New session');
  });

  it('deletes from "⋯" with a confirmation in the row, and renames', async () => {
    const api = hubApi();
    const app = makeApp(LIST, api);
    vi.stubGlobal('prompt', () => 'Net sync');
    await mount(<SessionList app={app} />);
    await click(rowOf('Network sync').querySelector('.btn-row-more'));
    expect([...root.querySelectorAll('.menu-item')].map((b) => b.textContent)).toEqual([
      'Add to favorites',
      'Rename',
      'Folder rules',
      'Delete…',
    ]);
    await click(root.querySelector('.menu .btn-rename'));
    expect(api.patchSession).toHaveBeenCalledWith('net', { title: 'Net sync' });
    vi.unstubAllGlobals();

    await click(rowOf('Cave scene port').querySelector('.btn-row-more'));
    await click(root.querySelector('.menu .btn-delete'));
    const row = rowOf('Cave scene port');
    expect(row.classList.contains('is-confirming')).toBe(true);
    expect(row.querySelector('.row-confirm')!.textContent).toBe('Delete forever?DeleteCancel');
    expect(row.querySelector('.row-acts')).toBeNull();
    await click([...row.querySelectorAll('button')].find((b) => b.textContent === 'Cancel')!);
    expect(api.deleteSession).not.toHaveBeenCalled();
    await click(rowOf('Cave scene port').querySelector('.btn-row-more'));
    await click(root.querySelector('.menu .btn-delete'));
    await click(rowOf('Cave scene port').querySelector('.btn-confirm-delete'));
    expect(api.deleteSession).toHaveBeenCalledWith('cave');
    expect(titles()).not.toContain('Cave scene port');
  });
});

describe('Reorder (T50)', () => {
  it('moves a row with the arrow keys on its handle; Done saves the order and turns Custom on', async () => {
    const api = hubApi();
    const app = makeApp(LIST, api);
    await mount(<SessionList app={app} />);
    await click(root.querySelector('.btn-reorder'));
    expect(root.querySelector('.reorder-bar')!.textContent).toBe('ReorderCancelDone');
    expect(root.querySelector('.seg-group')).toBeNull();
    expect(root.querySelector('.row-acts')).toBeNull();
    expect(root.querySelector('a.session-row')).toBeNull();
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
    expect(root.querySelector('.seg[aria-pressed="true"]')!.textContent).toBe('Custom');
    expect(groups()).toEqual([['Custom order', undefined, ['Step determinism', 'Stone textures', 'Cave scene port', 'Network sync', 'Collision profiling']]]);
  });

  it('drags a row by its handle with pointer events', async () => {
    await mount(<SessionList app={makeApp()} />);
    await click(root.querySelector('.btn-reorder'));
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
    await click(root.querySelector('.btn-reorder'));
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

describe('SessionList (phone)', () => {
  it('one screen: projects as chips, the same groups, a floating "New"', async () => {
    await mount(<SessionList app={makeApp()} phone />);
    expect(root.querySelector('.m-head h1')!.textContent).toBe('Sessions');
    const chips = [...root.querySelectorAll('.chips .chip.project-row')];
    // T49: the icon before the name; without one a dot of the project's color.
    expect(chips.map((c) => c.textContent)).toEqual(['Personal', '⚛️physics-mp1']);
    expect(chips[0].querySelector<HTMLElement>('.chip-dot')!.style.background).toBe('#8A93A6');
    expect(root.querySelector('.chip.is-current .chip-badge')!.textContent).toBe('1');
    expect(groups().map((g) => g[0])).toEqual(['Needs you', 'Running', 'Idle']);
    expect(root.querySelector('.fab')!.textContent).toBe('New');
    // The search is a screen of its own (T33, tests/search.test.tsx).
    expect(root.querySelector('input[aria-label="Search sessions"]')).toBeNull();
    expect(root.querySelector('a[aria-label="Search sessions and messages"]')!.getAttribute('href')).toBe('#/search');
    expect(root.querySelector('.row-acts')).toBeNull();
    expect(root.querySelector('.list-hint')!.textContent).toBe('Press and hold a session to archive, rename or delete it.');
  });

  it('a long press opens the actions sheet: Archive with Undo, Delete in a second step (T50)', async () => {
    vi.useFakeTimers({ toFake: ['Date', 'setTimeout', 'clearTimeout'] });
    const api = hubApi();
    await mount(<SessionList app={makeApp(LIST, api)} phone />);
    const press = (title: string) =>
      act(async () => {
        rowOf(title).dispatchEvent(new PointerEvent('pointerdown', { bubbles: true, clientX: 10, clientY: 10 }));
      });
    // A short press is a tap.
    await press('Network sync');
    await act(async () => {
      rowOf('Network sync').dispatchEvent(new PointerEvent('pointerup', { bubbles: true }));
    });
    await act(async () => {
      vi.advanceTimersByTime(1000);
    });
    expect(root.querySelector('.action-sheet')).toBeNull();

    await press('Network sync');
    await act(async () => {
      vi.advanceTimersByTime(450);
    });
    const sheet = root.querySelector('.action-sheet')!;
    expect(sheet.querySelector('.action-sheet-title')!.textContent).toBe('Network sync');
    expect([...sheet.querySelectorAll('button')].map((b) => b.textContent)).toEqual([
      'Archive',
      'Add to favorites',
      'Rename',
      'Delete…',
      'Cancel',
    ]);
    expect(sheet.querySelector('.btn-sheet-rules')!.getAttribute('href')).toBe('#/p/p1/rules/w1?session=net');
    await click(sheet.querySelector('.btn-sheet-archive'));
    expect(root.querySelector('.action-sheet')).toBeNull();
    expect(root.querySelector('.toast-text')!.textContent).toBe('Archived «Network sync»');
    await click(root.querySelector('.btn-undo'));
    expect(titles()).toContain('Network sync');

    // A right click opens it too; Delete asks once more.
    await act(async () => {
      rowOf('Cave scene port').dispatchEvent(new MouseEvent('contextmenu', { bubbles: true, cancelable: true }));
    });
    await click(root.querySelector('.btn-sheet-delete'));
    expect(root.querySelector('.action-sheet-note')!.textContent).toBe(
      'Delete this session forever? Messages and history go too. Files on disk stay.',
    );
    await click(root.querySelector('.btn-sheet-delete-forever'));
    expect(api.deleteSession).toHaveBeenCalledWith('cave');
    expect(api.archive).not.toHaveBeenCalled();
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
  const menuItems = () => [...root.querySelectorAll('.menu .menu-item')].map((b) => b.textContent);

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
    expect(rows[1].querySelector('.session-where')!.textContent).toBe('home-mac · games/phys-mp');
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
    await click(root.querySelector('.btn-reorder'));
    expect(root.querySelector('.group-favorites')).toBeNull();
  });

  it('"⋯" adds a session to the favorites and takes it out; the summary of another device too', async () => {
    const api = hubApi();
    const app = makeApp(LIST, api);
    await mount(<SessionList app={app} />);
    expect(root.querySelector('.group-favorites')).toBeNull();
    await click(listed('Cave scene port').querySelector('.btn-row-more'));
    expect(menuItems()[0]).toBe('Add to favorites');
    await click(root.querySelector('.menu .btn-favorite'));
    expect(api.setSessionFavorite).toHaveBeenCalledWith('cave', true);
    expect(favoriteTitles()).toEqual(['Cave scene port']);
    expect(listed('Cave scene port')).toBeUndefined();

    await click(starred('Cave scene port').querySelector('.btn-row-more'));
    expect(menuItems()[0]).toBe('Remove from favorites');
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
    await click(listed('Cave scene port').querySelector('.btn-row-more'));
    await click(root.querySelector('.menu .btn-favorite'));
    expect(root.querySelector('.notice')!.textContent).toBe('Favorites not saved: hub unavailable');
    expect(root.querySelector('.group-favorites')).toBeNull();
  });

  it('a project whose sessions are all favorites is not "No sessions yet"', async () => {
    await mount(<SessionList app={makeApp(LIST.map((s) => ({ ...s, favorite_at: at(1) })))} />);
    expect(groups().map((g) => g[0])).toEqual(['Favorites']);
    expect(root.querySelector('.empty')).toBeNull();
  });

  it('the phone sheet has it too', async () => {
    const api = hubApi();
    await mount(<SessionList app={makeApp(LIST, api)} phone />);
    await act(async () => {
      listed('Network sync').dispatchEvent(new MouseEvent('contextmenu', { bubbles: true, cancelable: true }));
    });
    await click(root.querySelector('.btn-sheet-favorite'));
    expect(api.setSessionFavorite).toHaveBeenCalledWith('net', true);
    expect(root.querySelector('.action-sheet')).toBeNull();
    expect(favoriteTitles()).toEqual(['Network sync']);
    await act(async () => {
      starred('Network sync').dispatchEvent(new MouseEvent('contextmenu', { bubbles: true, cancelable: true }));
    });
    expect(root.querySelector('.btn-sheet-favorite')!.textContent).toBe('Remove from favorites');
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
    expect(root.querySelector('.side-counts')!.textContent).toBe('1 needs you · 2 running');
    expect(groups()).toEqual([
      ['Today', '3', ['Stone textures', 'Step determinism', 'Network sync']],
      ['Yesterday', '1', ['Collision profiling']],
      ['Earlier', '1', ['Cave scene port']],
    ]);
  });

  it('with nothing remembered the head has no counts and no "offline" until the hub and the socket answer', async () => {
    const api = { ...hubApi(), me: vi.fn(pending), projects: vi.fn(pending), allSessions: vi.fn(pending) };
    const { socket, state } = quietSocket();
    const app = new AppState(api as unknown as Api, socket);
    void app.init();
    await mount(<SessionList app={app} />);
    expect(root.querySelector('.project-title')!.textContent).toBe('HQC');
    expect(root.querySelector('.side-counts')!.textContent).toBe('');
    await mount(<SessionList app={app} phone />);
    expect(root.querySelector('.m-head-state')).toBeNull();
    // The first connect is pending: still not offline.
    await act(async () => state('connecting'));
    expect(root.querySelector('.m-head-state')).toBeNull();
    // It failed: offline now.
    await act(async () => state('closed'));
    expect(root.querySelector('.m-head-state')!.textContent).toBe('offline');
    await mount(<SessionList app={app} />);
    expect(root.querySelector('.side-counts')!.textContent).toBe('offline');
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
