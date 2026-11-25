// @vitest-environment happy-dom
// What the device does with its copies of history and attachments (SPEC §6, §12): it drops those of
// sessions the hub no longer lists, of what was not used for 30 days, and the least recently used
// beyond 200 MB; it wipes them when `/api/me` refuses (401/403, or a sign-in proxy's redirect) or
// the user is another; and Settings has "Forget this device".

import 'fake-indexeddb/auto';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { render } from 'preact';
import { act } from 'preact/test-utils';
import { Api, ApiError, eventPageOf, type Fetch } from '../src/api';
import { AppState } from '../src/app';
import { deviceArtifactCache } from '../src/artifactcache';
import { deviceHistoryCache, memoryHistoryCache, type HistoryCache } from '../src/historycache';
import type { Me, SessionInfo } from '../src/proto';
import { Settings } from '../src/ui/Settings';
import { defaultKey } from '../src/keys';
import { WEB_SEARCHES } from '../src/websearch';
import type { HubSocket, SessionSubscriber } from '../src/ws';

const DAY = 24 * 3_600_000;
let uid = 0;
const meOf = (id: string): Me => ({ id, email: 'me@example.com', personal_project_id: 'p', theme: 'warm', look: 'spacious', project_sort: 'custom', new_session_project_id: null, quick_messages: [] });
const socket = { onState: () => {}, onRequests: () => {}, onSummary: () => {}, start: () => {}, subscribe: (_id: string, _s: SessionSubscriber) => () => {} } as unknown as HubSocket;
const wire = (sid: string, seq: number) => ({
  session_id: sid,
  event_id: `${sid}-e${seq}`,
  seq,
  ts: '2026-10-24T18:00:00Z',
  author: { kind: 'agent', id: sid },
  type: 'agent.message',
  payload: { text: `answer ${seq}`, final: true },
});
const info = (id: string) => ({ id, project_id: 'p' }) as SessionInfo;
const apiOf = (me: () => Promise<Me>, sessions: string[] = []) =>
  ({
    me,
    projects: async () => [],
    archivedProjects: async () => [],
    allSessions: async () => sessions.map(info),
    eventsBefore: async (sid: string) => eventPageOf([wire(sid, 1)], 0),
  }) as unknown as Api;

const databases = async () => (await indexedDB.databases()).map((d) => d.name);
const until = async (what: () => Promise<boolean>) => {
  for (let i = 0; i < 100 && !(await what()); i++) await new Promise((r) => setTimeout(r, 10));
};
const stored = async (id: string) => (await deviceHistoryCache(id).sessions()).map((s) => s.id).sort();
const blobs = async (id: string) => (await deviceArtifactCache(id).entries()).map((e) => e.id).sort();

/** Writes the note of a stored session or blob by hand, as the device would have it after a
 *  long use: `bytes` of 200 MB are not worth making. */
async function note(userId: string, store: 'ranges' | 'used', key: string, value: Record<string, unknown>) {
  const db = await new Promise<IDBDatabase>((resolve, reject) => {
    const req = indexedDB.open(store === 'ranges' ? `hqc-history-${userId}` : `hqc-artifacts-${userId}`);
    req.onsuccess = () => resolve(req.result);
    req.onerror = () => reject(req.error);
  });
  await new Promise<void>((resolve, reject) => {
    const t = db.transaction(store, 'readwrite');
    const os = t.objectStore(store);
    if (store === 'ranges') {
      const g = os.get(key);
      g.onsuccess = () => os.put({ ...g.result, ...value });
    } else os.put(value, key);
    t.oncomplete = () => resolve();
    t.onerror = () => reject(t.error);
  });
  db.close();
}

afterEach(() => {
  vi.useRealTimers();
  localStorage.clear();
});

describe('the clean-up of the copies after the lists load', () => {
  it('drops the copies of a session the hub no longer lists, and keeps the listed ones', async () => {
    const id = `u-orphan-${uid++}`;
    const h = deviceHistoryCache(id);
    await h.put('listed', [wire('listed', 1)]);
    await h.put('gone', [wire('gone', 1)]);
    const app = new AppState(apiOf(async () => meOf(id), ['listed']), socket);
    await app.init();
    await until(async () => (await stored(id)).length === 1);
    expect(await stored(id)).toEqual(['listed']);
  });

  it('drops what was not used for 30 days, sessions and attachments alike', async () => {
    const id = `u-age-${uid++}`;
    vi.useFakeTimers({ toFake: ['Date'] });
    const t0 = Date.now();
    vi.setSystemTime(t0 - 31 * DAY);
    await deviceHistoryCache(id).put('old', [wire('old', 1)]);
    await deviceArtifactCache(id).putBlob('old-blob', new Blob(['x']));
    vi.setSystemTime(t0 - 29 * DAY);
    await deviceHistoryCache(id).put('recent', [wire('recent', 1)]);
    await deviceArtifactCache(id).putBlob('recent-blob', new Blob(['x']));
    vi.setSystemTime(t0);
    const app = new AppState(apiOf(async () => meOf(id), ['old', 'recent']), socket);
    await app.init();
    await until(async () => (await stored(id)).length === 1 && (await blobs(id)).length === 1);
    expect(await stored(id)).toEqual(['recent']);
    expect(await blobs(id)).toEqual(['recent-blob']);
  });

  it('beyond 200 MB drops the least recently used, a session or an attachment, until it fits', async () => {
    const id = `u-size-${uid++}`;
    const now = Date.now();
    const h = deviceHistoryCache(id);
    await h.put('a', [wire('a', 1)]);
    await h.put('b', [wire('b', 1)]);
    await deviceArtifactCache(id).putBlob('c', new Blob(['x']));
    await note(id, 'ranges', 'a', { used_at: now - 3_600_000, bytes: 120_000_000 });
    await note(id, 'ranges', 'b', { used_at: now - 2 * 3_600_000, bytes: 120_000_000 });
    await note(id, 'used', 'c', { used_at: now - 3 * 3_600_000, bytes: 50_000_000 });
    const app = new AppState(apiOf(async () => meOf(id), ['a', 'b']), socket);
    await app.init();
    await until(async () => (await blobs(id)).length === 0);
    await until(async () => (await stored(id)).length === 1);
    // The oldest, the attachment, goes first (290 → 240 MB), then b (→ 120 MB): a fits.
    expect(await blobs(id)).toEqual([]);
    expect(await stored(id)).toEqual(['a']);
  });

  it('keeps the copy of an open chat, though the hub does not list it and it is old', async () => {
    const id = `u-open-${uid++}`;
    vi.useFakeTimers({ toFake: ['Date'] });
    const t0 = Date.now();
    vi.setSystemTime(t0 - 40 * DAY);
    await deviceHistoryCache(id).put('open', [wire('open', 1)]);
    await deviceHistoryCache(id).put('other', [wire('other', 1)]);
    vi.setSystemTime(t0);
    const api = apiOf(async () => meOf(id));
    const app = new AppState(api, socket);
    const chat = app.chat('open');
    // Its copy waits for the user, so it opens while `init` runs.
    const opening = chat.open();
    await app.init();
    await opening;
    await until(async () => (await stored(id)).length === 1);
    expect(await stored(id)).toEqual(['open']);
    // Opening it marked it used now.
    expect((await deviceHistoryCache(id).sessions())[0].used_at).toBeGreaterThan(t0 - DAY);
    chat.close();
  });
});

describe('the clean-up and the chats', () => {
  it('runs at most once an hour', async () => {
    vi.useFakeTimers({ toFake: ['Date'] });
    let sweeps = 0;
    const history: HistoryCache = { ...memoryHistoryCache(), sessions: async () => (sweeps++, []) };
    const app = new AppState(apiOf(async () => meOf(`u-hourly-${uid++}`)), socket, history);
    await app.init();
    await until(async () => sweeps === 1);
    await app.reloadSessions();
    await new Promise((r) => setTimeout(r, 50));
    expect(sweeps).toBe(1);
    vi.setSystemTime(Date.now() + 3_600_000 + 1);
    await app.reloadSessions();
    await until(async () => sweeps === 2);
    expect(sweeps).toBe(2);
  });

  it('leaves the copy of a chat that opens while the sweep is on its way to drop it', async () => {
    const memory = memoryHistoryCache();
    await memory.put('x', [wire('x', 1), wire('x', 2), wire('x', 3)]);
    let release!: () => void;
    const gate = new Promise<void>((r) => (release = r));
    const history: HistoryCache = { ...memory, read: async (sid, lo, hi) => (await gate, memory.read(sid, lo, hi)) };
    // The hub does not list `x`; the chat loads nothing, so only the copy tells.
    const api = { ...apiOf(async () => meOf(`u-race-${uid++}`)), eventsBefore: async () => Promise.reject(new Error('down')) } as unknown as Api;
    const app = new AppState(api, socket, history);
    await app.init();
    await new Promise((r) => setTimeout(r, 20));
    const chat = app.chat('x');
    void chat.open();
    release();
    await new Promise((r) => setTimeout(r, 100));
    expect(await memory.range('x')).toEqual({ from: 1, to: 3 });
    chat.close();
  });

  it('a chat held at Forget this device goes on keeping what comes after it', async () => {
    const user = `u-keeps-${uid++}`;
    const subs: SessionSubscriber[] = [];
    const sock = { ...socket, subscribe: (_id: string, s: SessionSubscriber) => (subs.push(s), () => {}) } as unknown as HubSocket;
    const app = new AppState(apiOf(async () => meOf(user), ['s']), sock);
    await app.init();
    const chat = app.chat('s');
    await chat.open();
    await app.forgetDevice();
    chat.close();
    await chat.open();
    await until(async () => !!(await deviceHistoryCache(user).range('s')));
    expect(await deviceHistoryCache(user).range('s')).toEqual({ from: 1, to: 1 });
    subs[subs.length - 1].onEvent(eventPageOf([wire('s', 2)], 0).events[0], wire('s', 2));
    await until(async () => (await deviceHistoryCache(user).range('s'))?.to === 2);
    expect(await deviceHistoryCache(user).range('s')).toEqual({ from: 1, to: 2 });
    chat.close();
  });

  it('what a chat had not written yet when the device was forgotten does not come back', async () => {
    const user = `u-unwritten-${uid++}`;
    const subs: SessionSubscriber[] = [];
    const sock = { ...socket, subscribe: (_id: string, s: SessionSubscriber) => (subs.push(s), () => {}) } as unknown as HubSocket;
    const app = new AppState(apiOf(async () => meOf(user), ['s']), sock);
    await app.init();
    const chat = app.chat('s');
    await chat.open();
    await until(async () => !!(await deviceHistoryCache(user).range('s')));
    subs[0].onEvent(eventPageOf([wire('s', 2)], 0).events[0], wire('s', 2));
    await app.forgetDevice();
    await new Promise((r) => setTimeout(r, 400));
    expect(await deviceHistoryCache(user).range('s')).toBeUndefined();
    chat.close();
  });
});

describe('who the device keeps copies for', () => {
  const seed = async (user: string) => {
    await deviceHistoryCache(user).put('s', [wire('s', 1)]);
    await deviceArtifactCache(user).putMeta({ id: 'a', name: 'a.png' } as never);
  };
  const has = async (user: string) => {
    const names = await databases();
    return [names.includes(`hqc-history-${user}`), names.includes(`hqc-artifacts-${user}`)];
  };

  it.each([401, 403])('wipes both databases when /api/me answers %i', async (status) => {
    const user = `u-signed-out-${uid++}`;
    await seed(user);
    localStorage.setItem('hqc-device-user', user);
    const app = new AppState(apiOf(async () => Promise.reject(new ApiError(status, 'no'))), socket);
    await app.init();
    expect(await has(user)).toEqual([false, false]);
    expect(localStorage.getItem('hqc-device-user')).toBeNull();
  });

  // SPEC 0.249: a sign-in proxy (Cloudflare Access, oauth2-proxy) answers a signed-out user with a
  // redirect to its sign-in page, not 401. Followed, it fails `fetch` as offline and the copies
  // stayed; `/api/me` is asked with `redirect: 'manual'`, and a redirect is a sign-out.
  it.each([
    ['an opaque redirect, as a browser gives it', { type: 'opaqueredirect', status: 0 }],
    ['a 302, as a fetch outside a browser gives it', { type: 'basic', status: 302 }],
  ])('wipes both databases when /api/me is answered with %s', async (_, answer) => {
    const user = `u-redirected-${uid++}`;
    await seed(user);
    localStorage.setItem('hqc-device-user', user);
    const asked: (RequestInit | undefined)[] = [];
    const fetchImpl = (async (_url: unknown, init?: RequestInit) => {
      asked.push(init);
      return { ...answer, ok: false, statusText: '', text: async () => '' } as unknown as Response;
    }) as Fetch;
    const app = new AppState(new Api('', fetchImpl), socket);
    await app.init();
    expect(asked[0]?.redirect).toBe('manual');
    expect(await has(user)).toEqual([false, false]);
    expect(localStorage.getItem('hqc-device-user')).toBeNull();
  });

  it('keeps everything when /api/me fails for another reason', async () => {
    const user = `u-offline-${uid++}`;
    await seed(user);
    localStorage.setItem('hqc-device-user', user);
    const app = new AppState(apiOf(async () => Promise.reject(new TypeError('Failed to fetch'))), socket);
    await app.init();
    expect(await has(user)).toEqual([true, true]);
    expect(localStorage.getItem('hqc-device-user')).toBe(user);
    const hubError = new AppState(apiOf(async () => Promise.reject(new ApiError(500, 'boom'))), socket);
    await hubError.init();
    expect(await has(user)).toEqual([true, true]);
  });

  it('wipes the databases of the user stored before when it is another one, and stores the new', async () => {
    const before = `u-before-${uid++}`;
    const now = `u-now-${uid++}`;
    await seed(before);
    await seed(now);
    localStorage.setItem('hqc-device-user', before);
    const app = new AppState(apiOf(async () => meOf(now)), socket);
    await app.init();
    expect(await has(before)).toEqual([false, false]);
    expect(await has(now)).toEqual([true, true]);
    expect(localStorage.getItem('hqc-device-user')).toBe(now);
  });

  it('keeps the databases of the same user, and remembers a first one', async () => {
    const user = `u-same-${uid++}`;
    await seed(user);
    const app = new AppState(apiOf(async () => meOf(user), ['s']), socket);
    await app.init();
    expect(localStorage.getItem('hqc-device-user')).toBe(user);
    await new AppState(apiOf(async () => meOf(user), ['s']), socket).init();
    expect(await has(user)).toEqual([true, true]);
  });
});

describe('Forget this device', () => {
  it('works without /api/me, for the user stored at the last sign-in', async () => {
    const user = `u-offline-forget-${uid++}`;
    await deviceHistoryCache(user).put('s', [wire('s', 1)]);
    localStorage.setItem('hqc-device-user', user);
    const app = new AppState(apiOf(async () => Promise.reject(new TypeError('Failed to fetch'))), socket);
    await app.init();
    expect(app.deviceUser()).toBe(user);
    await app.forgetDevice();
    expect(await databases()).not.toContain(`hqc-history-${user}`);
  });

  it('has nobody to forget before /api/me and with nothing stored: the button is disabled', async () => {
    const app = new AppState(apiOf(async () => Promise.reject(new TypeError('Failed to fetch'))), socket);
    await app.init();
    expect(app.deviceUser()).toBeUndefined();
    const settings = {
      api: { pushKey: vi.fn(async () => null), workers: vi.fn(async () => []) },
      me: { email: 'me@example.com' },
      device: {},
      theme: () => 'warm',
      lookName: () => 'spacious',
      newSessionProject: () => undefined,
      sortedProjects: () => [],
      webSearch: WEB_SEARCHES[0],
      keyFor: defaultKey,
      setKey: vi.fn(),
      providers: [],
      reloadSessions: vi.fn(async () => {}),
      deviceUser: () => app.deviceUser(),
    } as unknown as AppState;
    const root = document.createElement('div');
    document.body.appendChild(root);
    await act(async () => render(<Settings app={settings} />, root));
    const button = [...root.querySelectorAll<HTMLButtonElement>('button')].find((b) => b.textContent === 'Forget this device')!;
    expect(button.disabled).toBe(true);
    render(null, root);
  });

  it('AppState.forgetDevice deletes both databases, lets go of the held chats, and the copies start again', async () => {
    const user = `u-forget-${uid++}`;
    await deviceHistoryCache(user).put('s', [wire('s', 1)]);
    await deviceArtifactCache(user).putMeta({ id: 'a', name: 'a.png' } as never);
    const app = new AppState(apiOf(async () => meOf(user), ['s']), socket);
    await app.init();
    const held = app.chat('s');
    await app.forgetDevice();
    expect(await databases()).not.toContain(`hqc-history-${user}`);
    expect(await databases()).not.toContain(`hqc-artifacts-${user}`);
    // A chat that was held is a new one now, writing to a new copy.
    const fresh = app.chat('s');
    expect(fresh).not.toBe(held);
    await fresh.load();
    await until(async () => !!(await deviceHistoryCache(user).range('s')));
    expect(await deviceHistoryCache(user).range('s')).toEqual({ from: 1, to: 1 });
  });

  it('the Settings button asks the app, then says Forgotten', async () => {
    const forgetDevice = vi.fn(async () => {});
    const app = {
      api: { pushKey: vi.fn(async () => null), workers: vi.fn(async () => []) },
      me: { email: 'me@example.com' },
      device: {},
      theme: () => 'warm',
      lookName: () => 'spacious',
      newSessionProject: () => undefined,
      sortedProjects: () => [],
      webSearch: WEB_SEARCHES[0],
      keyFor: defaultKey,
      setKey: vi.fn(),
      providers: [],
      reloadSessions: vi.fn(async () => {}),
      forgetDevice,
      deviceUser: () => 'u-settings',
    } as unknown as AppState;
    const root = document.createElement('div');
    document.body.appendChild(root);
    await act(async () => render(<Settings app={app} />, root));
    const button = () => [...root.querySelectorAll<HTMLButtonElement>('button')].find((b) => /Forget this device|Forgotten/.test(b.textContent ?? ''))!;
    expect([...root.querySelectorAll('h2')].map((h) => h.textContent)).toContain('This device');
    expect(button().textContent).toBe('Forget this device');
    await act(async () => button().click());
    await act(async () => void (await new Promise((r) => setTimeout(r, 0))));
    expect(forgetDevice).toHaveBeenCalledTimes(1);
    expect(button().textContent).toBe('Forgotten');
    render(null, root);
  });
});
