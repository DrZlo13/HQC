// @vitest-environment happy-dom
// A summary of a session this page does not have (SPEC §6): a `session.summary` in a project the
// page knows fetches only that session; in another project, or in the `summary` of every session,
// the lists are fetched.

import { beforeEach, expect, it, vi } from 'vitest';
import type { Api } from '../src/api';
import { AppState } from '../src/app';
import type { Project, SessionInfo, SessionSummary } from '../src/proto';
import type { HubSocket } from '../src/ws';
import type { Me } from '../src/proto';

// A list that landed in one test is kept on the device and would be the next one's.
beforeEach(() => localStorage.clear());

const socket = { onState: () => {}, onRequests: () => {}, onSummary: () => {}, start: () => {} } as unknown as HubSocket;
const project = { id: 'p1', name: 'boards', slug: 'boards' } as Project;
const info = (id: string, more: Partial<SessionInfo> = {}) =>
  ({ id, project_id: 'p1', title: 'from the hub', state: 'idle', queue_len: 0, unread: 0, last_seq: 1, last_read_seq: 1, archived: false, ...more }) as SessionInfo;
const summary = (id: string, more: Partial<SessionSummary> = {}): SessionSummary =>
  ({ session_id: id, project_id: 'p1', title: 'pushed', state: 'idle', queue_len: 0, unread: 0, last_seq: 1, last_read_seq: 1, ...more }) as SessionSummary;

function setup(session: (id: string) => Promise<SessionInfo>) {
  const api = {
    session: vi.fn(session),
    allSessions: vi.fn(async () => [] as SessionInfo[]),
    projects: vi.fn(async () => [project]),
    archivedProjects: vi.fn(async () => []),
  };
  const app = new AppState(api as unknown as Api, socket);
  app.me = { id: 'u', personal_project_id: 'p1' } as AppState['me'];
  app.projects.set('p1', project);
  return { app, api };
}

it('fetches only the session when its project is known', async () => {
  const { app, api } = setup(async (id) => info(id));
  app.mergeSummary([summary('s1')]);
  await vi.waitFor(() => expect(app.sessions.get('s1')?.title).toBe('from the hub'));
  expect(api.session).toHaveBeenCalledTimes(1);
  expect(api.session).toHaveBeenCalledWith('s1');
  expect(api.allSessions).not.toHaveBeenCalled();
});

it('fetches the lists for a summary of every session, whatever its projects', async () => {
  const { app, api } = setup(async (id) => info(id));
  app.mergeSummary([summary('s1'), summary('s2')], true);
  await vi.waitFor(() => expect(api.allSessions).toHaveBeenCalledTimes(1));
  expect(api.session).not.toHaveBeenCalled();
});

it('fetches the lists when the project is not known', async () => {
  const { app, api } = setup(async (id) => info(id));
  app.mergeSummary([summary('s1', { project_id: 'elsewhere' })]);
  await vi.waitFor(() => expect(api.allSessions).toHaveBeenCalledTimes(1));
  expect(api.session).not.toHaveBeenCalled();
});

it('fetches a session once at a time and merges the summary that came meanwhile after it', async () => {
  let land!: (s: SessionInfo) => void;
  const { app, api } = setup(() => new Promise<SessionInfo>((r) => (land = r)));
  app.mergeSummary([summary('s1', { title: 'first', state: 'running' })]);
  app.mergeSummary([summary('s1', { title: 'second', state: 'idle', last_seq: 5 })]);
  expect(api.session).toHaveBeenCalledTimes(1);
  land(info('s1', { title: 'older answer', state: 'running' }));
  await vi.waitFor(() => expect(app.sessions.get('s1')?.title).toBe('second'));
  expect(app.sessions.get('s1')?.last_seq).toBe(5);
  expect(api.session).toHaveBeenCalledTimes(1);
  expect(api.allSessions).not.toHaveBeenCalled();
});

it('does not roll back a session that came another way while its lone fetch was on its way', async () => {
  let fail!: (e: Error) => void;
  let listLand!: (l: SessionInfo[]) => void;
  const { app, api } = setup(() => new Promise<SessionInfo>((_, rej) => (fail = rej)));
  api.allSessions.mockImplementation(() => new Promise<SessionInfo[]>((r) => (listLand = r)));
  app.mergeSummary([summary('x', { state: 'running' })]);
  // Kept for after the fetch.
  app.mergeSummary([summary('x', { state: 'running', last_seq: 3 })]);
  // The socket reconnected: the whole summary names x, and the lists are fetched.
  app.mergeSummary([summary('x', { state: 'running', last_seq: 3 })], true);
  await vi.waitFor(() => expect(api.allSessions).toHaveBeenCalled());
  listLand([info('x', { state: 'idle', last_seq: 9 })]);
  await vi.waitFor(() => expect(app.sessions.get('x')?.state).toBe('idle'));
  // The fetch ends on the old connection.
  fail(new Error('offline'));
  await new Promise((r) => setTimeout(r, 20));
  expect(app.sessions.get('x')?.state).toBe('idle');
  expect(app.sessions.get('x')?.last_seq).toBe(9);
});

it('does not roll back a session this fetch did not add, when the fetch itself answers', async () => {
  let land!: (s: SessionInfo) => void;
  const { app } = setup(() => new Promise<SessionInfo>((r) => (land = r)));
  app.mergeSummary([summary('x', { title: 'first' })]);
  app.mergeSummary([summary('x', { title: 'old title', last_seq: 2 })]);
  // The reply to this client's own request put the session here, renamed.
  app.sessions.set('x', info('x', { title: 'renamed', last_seq: 4 }));
  land(info('x', { title: 'older answer' }));
  await new Promise((r) => setTimeout(r, 20));
  expect(app.sessions.get('x')?.title).toBe('renamed');
});

it('fetches the lists when the session it fetched has moved into a project the page does not know', async () => {
  const { app, api } = setup(async (id) => info(id, { project_id: 'elsewhere' }));
  app.mergeSummary([summary('s1')]);
  await vi.waitFor(() => expect(api.allSessions).toHaveBeenCalledTimes(1));
  expect(api.session).toHaveBeenCalledTimes(1);
});

/** An `AppState` that has taken its summaries from a socket's `onSummary`, as `init` wires it. */
async function wired() {
  const api = {
    me: vi.fn(async () => ({ id: 'u', email: 'u@example.com', personal_project_id: 'p1', theme: 'warm', look: 'spacious', project_sort: 'custom', new_session_project_id: null, quick_messages: [] }) as Me),
    session: vi.fn(async (id: string) => info(id)),
    allSessions: vi.fn(async () => [] as SessionInfo[]),
    projects: vi.fn(async () => [project]),
    archivedProjects: vi.fn(async () => []),
  };
  let listener!: (list: SessionSummary[], whole: boolean) => void;
  const sock = { ...socket, onSummary: (fn: typeof listener) => (listener = fn) } as unknown as HubSocket;
  const app = new AppState(api as unknown as Api, sock);
  await app.init();
  api.allSessions.mockClear();
  return { app, api, deliver: listener };
}

it('through the socket, a summary of every session fetches the lists, a session.summary only the session', async () => {
  const { api, deliver } = await wired();
  deliver([summary('s1'), summary('s2')], true);
  await vi.waitFor(() => expect(api.allSessions).toHaveBeenCalledTimes(1));
  expect(api.session).not.toHaveBeenCalled();
  deliver([summary('s3')], false);
  await vi.waitFor(() => expect(api.session).toHaveBeenCalledWith('s3'));
  expect(api.allSessions).toHaveBeenCalledTimes(1);
});
