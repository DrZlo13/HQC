// @vitest-environment happy-dom
// T26: the session list — the «Состояние» and «Недавние» orders (kept in localStorage), the
// search over title and path, the project's counters, workers with their active sessions,
// the project rail with its «ждёт вас» badge.

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { render } from 'preact';
import { act } from 'preact/test-utils';
import type { Api } from '../src/api';
import { AppState } from '../src/app';
import type { SessionInfo, SessionState } from '../src/proto';
import type { HubSocket } from '../src/ws';
import { ProjectRail } from '../src/ui/ProjectRail';
import { SessionList } from '../src/ui/SessionList';
import {
  activeLabel,
  bucketOf,
  groupSessions,
  headCounts,
  loadMode,
  matchesQuery,
  projectInitials,
  saveMode,
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
  sess('stones', 'Текстуры камней', 'running', at(0.01)),
  sess('det', 'Детерминизм шага', 'running', at(0.1), { worker_id: 'wk2', workspace_id: 'w2' }),
  sess('net', 'Синхронизация по сети', 'waiting', at(0.2), { cwd: 'net' }),
  sess('prof', 'Профилирование коллизий', 'idle', at(22)),
  sess('cave', 'Порт сцены пещеры', 'idle', at(60)),
];

let root: HTMLElement;
beforeEach(() => {
  vi.useFakeTimers({ toFake: ['Date'] });
  vi.setSystemTime(NOW);
  localStorage.clear();
});
afterEach(() => {
  if (root) render(null, root);
  document.body.innerHTML = '';
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
  });

  it('groups by state and by recency, newest first, empty groups left out', () => {
    expect(groupSessions(LIST, 'state', NOW).map((g) => [g.label, g.items.map((s) => s.id)])).toEqual([
      ['Ждёт вас', ['net']],
      ['Работает', ['stones', 'det']],
      ['Свободны', ['prof', 'cave']],
    ]);
    expect(groupSessions(LIST, 'recent', NOW).map((g) => [g.label, g.items.map((s) => s.id)])).toEqual([
      ['Сегодня', ['stones', 'det', 'net']],
      ['Вчера', ['prof']],
      ['Раньше', ['cave']],
    ]);
    expect(groupSessions(LIST.slice(3), 'state', NOW).map((g) => g.label)).toEqual(['Свободны']);
  });

  it('counts for the header: waiting, working, and the free ones only when one of those is zero', () => {
    expect(headCounts(LIST)).toBe('1 ждёт вас · 2 работают');
    expect(headCounts(LIST.filter((s) => s.state !== 'running'))).toBe('1 ждёт вас · 2 свободны');
    expect(headCounts(LIST.slice(3))).toBe('2 свободны');
    expect(headCounts(LIST.slice(0, 1))).toBe('1 работает');
    expect(headCounts([])).toBe('Сессий пока нет');
  });

  it('searches every word in the title or the path', () => {
    const s = LIST[2];
    expect(matchesQuery(s, '~/work/games/phys-mp/net', 'сети')).toBe(true);
    expect(matchesQuery(s, '~/work/games/phys-mp/net', 'PHYS-MP/NET')).toBe(true);
    expect(matchesQuery(s, '~/work/games/phys-mp/net', 'синхр net')).toBe(true);
    expect(matchesQuery(s, '~/work/games/phys-mp/net', 'синхр render')).toBe(false);
    expect(matchesQuery(s, '', '  ')).toBe(true);
  });

  it('names: initials, plurals', () => {
    expect(projectInitials('physics-mp')).toBe('PM');
    expect(projectInitials('HQC')).toBe('HQ');
    expect(projectInitials('Личный', true)).toBe('Л');
    expect([0, 1, 2, 5, 11, 21].map(activeLabel)).toEqual([
      'нет активных',
      '1 активная',
      '2 активные',
      '5 активных',
      '11 активных',
      '21 активная',
    ]);
  });

  it('keeps the mode in localStorage, «Состояние» by default', () => {
    expect(loadMode()).toBe('state');
    saveMode('recent');
    expect(localStorage.getItem('hqc.sessionOrder')).toBe('recent');
    expect(loadMode()).toBe('recent');
    localStorage.setItem('hqc.sessionOrder', 'custom');
    expect(loadMode()).toBe('state');
  });
});

function makeApp(list = LIST) {
  const socket = { onState: () => () => {} } as unknown as HubSocket;
  const app = new AppState({ workspaces: async () => [] } as unknown as Api, socket);
  app.me = { id: 'u', email: 'who@example.com', personal_project_id: 'p0' };
  app.projects.set('p0', { id: 'p0', name: 'Личный', slug: 'personal', description: '', is_personal: true, created_at: at(100) });
  app.projects.set('p1', { id: 'p1', name: 'physics-mp', slug: 'physics-mp', description: '', is_personal: false, created_at: at(90) });
  app.currentProjectId = 'p1';
  app.conn = 'open';
  const ws = (id: string, worker: string, path: string) => ({
    id,
    project_id: 'p1',
    worker_id: worker,
    path,
    label: '',
    kind: 'dir',
    mirror_enabled: true,
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
    g.querySelector('.group-count')!.textContent,
    [...g.querySelectorAll('.session-title')].map((t) => t.textContent),
  ]);

async function type(value: string) {
  const input = root.querySelector<HTMLInputElement>('input[aria-label="Поиск сессий"]')!;
  await act(() => {
    input.value = value;
    input.dispatchEvent(new Event('input', { bubbles: true }));
  });
}

describe('SessionList (desktop column)', () => {
  it('shows the project, its counters and the groups by state', async () => {
    await mount(<SessionList app={makeApp()} current="net" />);
    expect(root.querySelector('.project-title')!.textContent).toBe('physics-mp');
    expect(root.querySelector('.side-counts')!.textContent).toBe('1 ждёт вас · 2 работают');
    expect(groups()).toEqual([
      ['Ждёт вас', '1', ['Синхронизация по сети']],
      ['Работает', '2', ['Текстуры камней', 'Детерминизм шага']],
      ['Свободны', '2', ['Профилирование коллизий', 'Порт сцены пещеры']],
    ]);
    const net = root.querySelector('.session-row.is-current')!;
    expect(net.getAttribute('aria-current')).toBe('page');
    expect(net.querySelector('.session-sub')!.textContent).toBe('Ждёт ответа');
    expect(net.querySelector('.session-where')!.textContent).toBe('home-mac · ~/work/games/phys-mp/net');
    expect(net.querySelector('.dot-wait')).not.toBeNull();
    expect(root.querySelector('.session-when')).toBeNull();
  });

  it('switches to «Недавние» with times, and keeps the choice in this browser', async () => {
    await mount(<SessionList app={makeApp()} />);
    const recent = [...root.querySelectorAll<HTMLButtonElement>('.seg')].find((b) => b.textContent === 'Недавние')!;
    await act(async () => recent.click());
    expect(groups().map((g) => g[0])).toEqual(['Сегодня', 'Вчера', 'Раньше']);
    expect([...root.querySelectorAll('.session-when')].map((w) => w.textContent)).toEqual([
      'сейчас',
      '6 мин',
      '12 мин',
      'вчера',
      '22 окт',
    ]);
    expect(localStorage.getItem('hqc.sessionOrder')).toBe('recent');
    render(null, root);
    await mount(<SessionList app={makeApp()} />);
    expect(root.querySelector('.seg[aria-pressed="true"]')!.textContent).toBe('Недавние');
    expect(groups()[0][0]).toBe('Сегодня');
  });

  it('searches by title and by path', async () => {
    await mount(<SessionList app={makeApp()} />);
    await type('пещер');
    expect(groups()).toEqual([['Свободны', '1', ['Порт сцены пещеры']]]);
    await type('phys-det');
    expect(groups()).toEqual([['Работает', '1', ['Детерминизм шага']]]);
    await type('нет такого');
    expect(groups()).toEqual([]);
    expect(root.querySelector('.empty')!.textContent).toBe('Ничего не найдено');
  });

  it('lists the workers with their active sessions, offline ones grey', async () => {
    await mount(<SessionList app={makeApp()} />);
    const rows = [...root.querySelectorAll('.worker-list .worker-row')];
    expect(rows.map((r) => [r.querySelector('.worker-name')!.textContent, r.querySelector('.worker-state')!.textContent])).toEqual([
      ['drzlo-mac', 'офлайн'],
      ['home-mac', '2 активные'],
    ]);
    expect(rows[0].querySelector('.dot-offline')).not.toBeNull();
    expect(rows[1].getAttribute('href')).toBe('#/s/sys1');
  });
});

describe('ProjectRail', () => {
  it('shows initials, the current project and a «ждёт вас» badge', async () => {
    await mount(<ProjectRail app={makeApp()} />);
    const rows = [...root.querySelectorAll('.rail-project')];
    expect(rows.map((r) => [r.getAttribute('aria-label'), r.textContent])).toEqual([
      ['Личный', 'Л'],
      ['physics-mp', 'PM'],
    ]);
    expect(root.querySelector('.rail-project.is-current')!.getAttribute('aria-label')).toBe('physics-mp');
    const badges = [...root.querySelectorAll('.rail-badge')];
    expect(badges.map((b) => b.textContent)).toEqual(['1']);
    expect(badges[0].getAttribute('aria-label')).toBe('ждёт вас: 1');
    expect(root.querySelector('.avatar')!.getAttribute('title')).toBe('who@example.com');
  });
});

describe('SessionList (phone)', () => {
  it('one screen: projects as chips, the same groups, a floating «Новая»', async () => {
    await mount(<SessionList app={makeApp()} phone />);
    expect(root.querySelector('.m-head h1')!.textContent).toBe('Сессии');
    const chips = [...root.querySelectorAll('.chips .chip.project-row')];
    expect(chips.map((c) => c.textContent)).toEqual(['Личный', 'physics-mp1']);
    expect(root.querySelector('.chip.is-current .chip-badge')!.textContent).toBe('1');
    expect(groups().map((g) => g[0])).toEqual(['Ждёт вас', 'Работает', 'Свободны']);
    expect(root.querySelector('.fab')!.textContent).toBe('Новая');
    // The search opens from its button.
    expect(root.querySelector('input[aria-label="Поиск сессий"]')).toBeNull();
    await act(async () => root.querySelector<HTMLButtonElement>('button[aria-label="Поиск по названию и пути"]')!.click());
    await type('сети');
    expect(groups()).toEqual([['Ждёт вас', '1', ['Синхронизация по сети']]]);
  });
});
