// @vitest-environment happy-dom
// T19: revoked workers in "Settings" get "Delete" (with the hub's `409
// worker_has_sessions` explained), and the "Workers" group lists only workers that are not
// revoked (their service sessions are archived, SPEC §10.2).

import { afterEach, expect, it, vi } from 'vitest';
import { render } from 'preact';
import { act } from 'preact/test-utils';
import { ApiError, type Api, type WorkerInfo } from '../src/api';
import { AppState } from '../src/app';
import type { SessionInfo } from '../src/proto';
import type { HubSocket } from '../src/ws';
import { Settings } from '../src/ui/Settings';
import { SessionList } from '../src/ui/SessionList';

let root: HTMLElement;
afterEach(() => {
  if (root) render(null, root);
  document.body.innerHTML = '';
  vi.unstubAllGlobals();
});

function mountRoot() {
  root = document.createElement('div');
  document.body.appendChild(root);
}

const buttons = (text: string) =>
  [...root.querySelectorAll<HTMLButtonElement>('button')].filter((b) => b.textContent?.trim() === text);

function settingsApp(api: Partial<Api>) {
  return {
    api,
    me: { email: 'me@example.com' },
    theme: () => 'warm',
    refreshSessions: vi.fn(async () => {}),
  } as unknown as AppState;
}

const live: WorkerInfo = { id: 'w-live', name: 'home-mac', os: 'macos', status: 'online' };
const gone: WorkerInfo = { id: 'w-gone', name: 'old-mac', os: 'macos', status: 'revoked', revoked_at: '2026-10-23T09:00:00Z' };

it('lists revoked workers with "Delete" and deletes them', async () => {
  vi.stubGlobal('confirm', () => true);
  let list = [live, gone];
  const workers = vi.fn(async () => list);
  const purgeWorker = vi.fn(async () => {
    list = [live];
  });
  const revokeWorker = vi.fn();
  mountRoot();
  const app = settingsApp({ workers, purgeWorker, revokeWorker });
  await act(async () => render(<Settings app={app} />, root));
  await act(async () => {});
  expect(workers).toHaveBeenCalledWith({ includeRevoked: true });
  const rows = [...root.querySelectorAll('.plain-list li')];
  expect(rows.map((r) => r.textContent)).toEqual([
    expect.stringContaining('Revoke'),
    expect.stringContaining('revoked'),
  ]);
  expect(rows[1].textContent).toContain('Delete');
  expect(rows[1].textContent).not.toContain('Revoke');

  await act(async () => buttons('Delete')[0].click());
  await act(async () => {});
  expect(purgeWorker).toHaveBeenCalledWith('w-gone');
  expect(revokeWorker).not.toHaveBeenCalled();
  expect(app.refreshSessions).toHaveBeenCalled();
  expect(root.querySelectorAll('.plain-list li')).toHaveLength(1);
  expect(buttons('Delete')).toHaveLength(0);
});

it('explains how to move sessions when the worker still has them', async () => {
  vi.stubGlobal('confirm', () => true);
  const purgeWorker = vi.fn(async () => {
    throw new ApiError(409, 'sessions still work', 'worker_has_sessions', {
      error: 'worker_has_sessions',
      message: 'sessions still work',
      sessions_count: 4,
    });
  });
  mountRoot();
  const app = settingsApp({ workers: vi.fn(async () => [gone]), purgeWorker });
  await act(async () => render(<Settings app={app} />, root));
  await act(async () => {});
  await act(async () => buttons('Delete')[0].click());
  await act(async () => {});
  const err = root.querySelector('.error-text')!.textContent!;
  expect(err).toContain('still has 4 unarchived sessions');
  expect(err).toContain('replace “old-mac”');
  expect(err).toContain('archive them');
});

it('revokes a worker and refreshes the sessions', async () => {
  vi.stubGlobal('confirm', () => true);
  const revokeWorker = vi.fn(async () => {});
  mountRoot();
  const app = settingsApp({ workers: vi.fn(async () => [live]), revokeWorker });
  await act(async () => render(<Settings app={app} />, root));
  await act(async () => {});
  await act(async () => buttons('Revoke')[0].click());
  expect(revokeWorker).toHaveBeenCalledWith('w-live');
  expect(app.refreshSessions).toHaveBeenCalled();
});

function serviceSession(id: string, title: string, archived: boolean): SessionInfo {
  return {
    id,
    project_id: 'p1',
    title,
    slug: `worker-${id}`,
    state: archived ? 'archived' : 'idle',
    queue_len: 0,
    unread: 0,
    last_seq: 1,
    archived,
    system: true,
    updated_at: '2026-10-23T09:00:00Z',
  } as unknown as SessionInfo;
}

it('leaves revoked workers out of the "Workers" group', async () => {
  const socket = { onState: () => {} } as unknown as HubSocket;
  const app = new AppState({} as Api, socket);
  app.sessions.set('s1', serviceSession('s1', 'home-mac', false));
  app.sessions.set('s2', serviceSession('s2', 'old-mac', true));
  expect(app.workerSessions().map((s) => s.title)).toEqual(['home-mac']);

  mountRoot();
  await act(async () => render(<SessionList app={app} />, root));
  const rows = [...root.querySelectorAll('.worker-list .worker-row')];
  expect(rows.map((r) => r.textContent)).toEqual([expect.stringContaining('home-mac')]);
  expect(root.querySelector('.worker-list')!.textContent).not.toContain('old-mac');
});
