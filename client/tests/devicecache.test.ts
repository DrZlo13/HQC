// @vitest-environment happy-dom
// The device's copies are the signed-in user's (SPEC §6, §12): a chat that opens before `/api/me`
// has answered, from the list the page kept at a reload, keeps its history and attachments in
// that user's copy once the answer is in.

import 'fake-indexeddb/auto';
import { expect, it } from 'vitest';
import { eventPageOf, type Api } from '../src/api';
import { AppState } from '../src/app';
import { deviceArtifactCache } from '../src/artifactcache';
import { deviceHistoryCache } from '../src/historycache';
import type { Artifact, Me, SessionInfo } from '../src/proto';
import type { HubSocket, SessionSubscriber } from '../src/ws';

const ME: Me = { id: 'u-device', email: 'me@example.com', personal_project_id: 'p', theme: 'warm', look: 'spacious', project_sort: 'custom', new_session_project_id: null };
const socket = { onState: () => {}, onRequests: () => {}, onSummary: () => {}, start: () => {} } as unknown as HubSocket;
const wire = (seq: number) => ({
  session_id: 's1',
  event_id: `e${seq}`,
  seq,
  ts: '2026-10-24T18:00:00Z',
  author: { kind: 'agent', id: 's1' },
  type: 'agent.message',
  payload: { text: `answer ${seq}`, final: true },
});

it("a chat opened before the user is known keeps its history and attachments in that user's copy", async () => {
  let answer!: (me: Me) => void;
  const api = {
    me: () => new Promise<Me>((r) => (answer = r)),
    projects: async () => [],
    archivedProjects: async () => [],
    allSessions: async () => [],
    eventsBefore: async () => eventPageOf([wire(1), wire(2)], 0),
  } as unknown as Api;
  const app = new AppState(api, socket);
  const loaded = app.chat('s1').load();
  const meta = { id: 'a1', name: 'shot.png' } as Artifact;
  const kept = app.artifactCache.putMeta(meta);
  const init = app.init();
  answer(ME);
  await Promise.all([init, loaded, kept]);
  // The load does not wait for the copy to be written.
  const history = deviceHistoryCache(ME.id);
  for (let i = 0; i < 50 && !(await history.range('s1')); i++) await new Promise((r) => setTimeout(r, 10));
  expect(await history.range('s1')).toEqual({ from: 1, to: 2 });
  expect(await deviceArtifactCache(ME.id).getMeta('a1')).toEqual(meta);
});

it("a deleted session leaves the device with its attachments, and its chat writes nothing back", async () => {
  const subs: SessionSubscriber[] = [];
  const sock = { ...socket, subscribe: (_id: string, sub: SessionSubscriber) => (subs.push(sub), () => {}) } as unknown as HubSocket;
  const message = { ...wire(2), type: 'user.message', author: { kind: 'user', id: 'u' }, payload: { text: 'look', artifact_ids: ['a1', 'a2'] } };
  const result = { ...wire(3), type: 'tool.result', payload: { call_id: 'c', artifact_id: 'a3' } };
  const api = {
    me: async () => ME,
    projects: async () => [],
    archivedProjects: async () => [],
    allSessions: async () => [],
    eventsBefore: async () => eventPageOf([wire(1), message, result], 0),
    deleteSession: async () => {},
  } as unknown as Api;
  const app = new AppState(api, sock);
  await app.init();
  app.sessions.set('s1', { id: 's1', project_id: 'p' } as SessionInfo);
  const chat = app.chat('s1');
  await chat.open();
  const history = deviceHistoryCache(ME.id);
  const artifacts = deviceArtifactCache(ME.id);
  for (let i = 0; i < 50 && !(await history.range('s1')); i++) await new Promise((r) => setTimeout(r, 10));
  for (const id of ['a1', 'a2', 'a3', 'other']) {
    await artifacts.putMeta({ id, name: id } as Artifact);
    await artifacts.putBlob(id, new Blob([id]));
  }
  // An event of the subscription not written yet when the session is deleted.
  subs[0].onEvent(eventPageOf([wire(4)], 0).events[0], wire(4));
  await app.deleteSession('s1');
  chat.close();
  // Past the chat's keep window; the device forgets behind the deletion, the attachments last.
  await new Promise((r) => setTimeout(r, 400));
  for (let i = 0; i < 100 && (await artifacts.getMeta('a3')); i++) await new Promise((r) => setTimeout(r, 10));
  expect(await history.range('s1')).toBeUndefined();
  for (const id of ['a1', 'a2', 'a3']) {
    expect(await artifacts.getMeta(id)).toBeUndefined();
    expect(await artifacts.getBlob(id)).toBeUndefined();
  }
  expect(await artifacts.getMeta('other')).toBeDefined();
  expect(await artifacts.getBlob('other')).toBeDefined();
});
