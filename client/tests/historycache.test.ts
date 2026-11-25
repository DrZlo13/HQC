// What the device keeps (`HistoryCache`, `ArtifactCache`): the same contract tests run against
// the in-memory implementation and the IndexedDB one (fake-indexeddb stands in for the browser).

import 'fake-indexeddb/auto';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { deleteDeviceArtifacts, deviceArtifactCache, idbArtifactCache, memoryArtifactCache, noArtifactCache, type ArtifactCache } from '../src/artifactcache';
import { deleteDeviceHistory, deviceHistoryCache, idbHistoryCache, memoryHistoryCache, noHistoryCache, type HistoryCache } from '../src/historycache';
import type { Artifact } from '../src/proto';

const SID = 's1';
const wire = (seq: number, more: Record<string, unknown> = {}) => ({
  session_id: SID,
  event_id: `e${seq}`,
  seq,
  payload: { text: `t${seq}` },
  ...more,
});
const range = (a: number, b: number) => Array.from({ length: b - a + 1 }, (_, i) => wire(a + i));
const seqsOf = (raws: unknown[] | undefined) => raws?.map((r) => (r as { seq: number }).seq);

let n = 0;
const histories: [string, () => HistoryCache][] = [
  ['memory', memoryHistoryCache],
  ['indexeddb', () => idbHistoryCache(`test-history-${n++}`)],
];

describe.each(histories)('HistoryCache, %s', (_name, make) => {
  it('put grows a range it overlaps or adjoins, and replaces one it is apart from', async () => {
    const c = make();
    expect(await c.range(SID)).toBeUndefined();
    await c.put(SID, range(101, 200));
    await c.put(SID, range(150, 250));
    expect(await c.range(SID)).toEqual({ from: 101, to: 250 });
    await c.put(SID, range(251, 300));
    await c.put(SID, range(51, 100));
    expect(await c.range(SID)).toEqual({ from: 51, to: 300 });
    expect(seqsOf(await c.read(SID, 51, 300))).toEqual(range(51, 300).map((e) => e.seq));
    await c.put(SID, range(400, 410));
    expect(await c.range(SID)).toEqual({ from: 400, to: 410 });
    expect(await c.read(SID, 51, 300)).toBeUndefined();
    expect(await c.read(SID, 400, 410)).toHaveLength(11);
  });

  it('put keeps only events with a seq and the session id; read wants every one asked for', async () => {
    const c = make();
    await c.put(SID, [wire(1), { session_id: SID, event_id: 'x' }, { ...wire(2), session_id: 'other' }, wire(3), null, 'x']);
    expect(await c.range(SID)).toEqual({ from: 1, to: 3 });
    expect(await c.read(SID, 1, 3)).toBeUndefined();
    expect(seqsOf(await c.read(SID, 1, 1))).toEqual([1]);
    expect(await c.range('other')).toBeUndefined();
  });

  it('a session is kept apart from another, and drop forgets only its own', async () => {
    const c = make();
    await c.put(SID, range(1, 5));
    await c.put('s2', range(1, 3).map((e) => ({ ...e, session_id: 's2' })));
    await c.drop(SID);
    expect(await c.range(SID)).toBeUndefined();
    expect(await c.read(SID, 1, 5)).toBeUndefined();
    expect(await c.range('s2')).toEqual({ from: 1, to: 3 });
    expect(await c.read('s2', 1, 3)).toHaveLength(3);
  });

  it('patch changes the delivery of the event with that id, and nothing else', async () => {
    const c = make();
    await c.put(SID, range(1, 3));
    await c.patch(SID, { session_id: SID, event_id: 'e2', seq: 2, delivery: 'delivered', delivered_at: 'now', joined: true });
    const [e] = (await c.read(SID, 2, 2)) as { delivery: string; delivered_at: string; payload: { joined: boolean; text: string } }[];
    expect([e.delivery, e.delivered_at, e.payload.joined, e.payload.text]).toEqual(['delivered', 'now', true, 't2']);
    // Where the agent took it (SPEC 0.221) is kept too: the copy reopens the chat with it there.
    await c.patch(SID, { session_id: SID, event_id: 'e1', seq: 1, delivery: 'delivered', delivered_at: 'now', joined: true, joined_after: 3 });
    const [one] = (await c.read(SID, 1, 1)) as { payload: { joined: boolean; joined_after: number } }[];
    expect([one.payload.joined, one.payload.joined_after]).toEqual([true, 3]);
    // A message written quiet at a Stop (SPEC 0.231) keeps `quiet` and its place the same way.
    await c.patch(SID, { session_id: SID, event_id: 'e3', seq: 3, delivery: 'delivered', delivered_at: 'now', quiet: true, joined_after: 5 });
    const [q] = (await c.read(SID, 3, 3)) as { payload: { quiet: boolean; joined_after: number; joined?: boolean } }[];
    expect([q.payload.quiet, q.payload.joined_after, q.payload.joined]).toEqual([true, 5, undefined]);
    await c.patch(SID, { session_id: SID, event_id: 'other', seq: 2, delivery: 'failed' });
    await c.patch(SID, { session_id: SID, event_id: 'e9', seq: 9, delivery: 'failed' });
    expect(((await c.read(SID, 2, 2)) as { delivery: string }[])[0].delivery).toBe('delivered');
    expect(await c.range(SID)).toEqual({ from: 1, to: 3 });
  });

  it('update rewrites events inside the range, and neither adds one outside nor moves the range', async () => {
    const c = make();
    await c.update(SID, [wire(5)]);
    expect(await c.range(SID)).toBeUndefined();
    await c.put(SID, range(10, 20));
    await c.update(SID, [wire(15, { delivery: 'delivered' }), wire(5), wire(25), wire(21), { session_id: 'other', seq: 12 }]);
    expect(await c.range(SID)).toEqual({ from: 10, to: 20 });
    expect(await c.read(SID, 5, 5)).toBeUndefined();
    expect(await c.read(SID, 21, 21)).toBeUndefined();
    expect(((await c.read(SID, 15, 15)) as { delivery: string }[])[0].delivery).toBe('delivered');
    expect(await c.read(SID, 10, 20)).toHaveLength(11);
  });

  it('keeps when a session was used and what its events take: touch, put, sessions', async () => {
    vi.useFakeTimers({ toFake: ['Date'] });
    try {
      vi.setSystemTime(1_000);
      const c = make();
      await c.touch(SID);
      expect(await c.sessions()).toEqual([]);
      await c.put(SID, range(1, 3));
      const size = (rows: unknown[]) => rows.reduce<number>((n, r) => n + JSON.stringify(r).length, 0);
      expect(await c.sessions()).toEqual([{ id: SID, used_at: 1_000, bytes: size(range(1, 3)) }]);
      // A put that grows the range adds its rows; used_at stays until touch.
      vi.setSystemTime(2_000);
      await c.put(SID, range(4, 5));
      expect(await c.sessions()).toEqual([{ id: SID, used_at: 1_000, bytes: size(range(1, 5)) }]);
      // The same page put again adds nothing: its events are in the range.
      await c.put(SID, range(1, 5));
      await c.put(SID, range(1, 5));
      expect((await c.sessions())[0].bytes).toBe(size(range(1, 5)));
      // update and patch leave both as they are.
      await c.update(SID, [wire(2, { payload: { text: 'a much longer text than before, rewritten' } })]);
      await c.patch(SID, { event_id: 'e1', seq: 1, delivery: 'delivered' } as never);
      expect((await c.sessions())[0]).toEqual({ id: SID, used_at: 1_000, bytes: size(range(1, 5)) });
      vi.setSystemTime(3_000);
      await c.touch(SID);
      expect((await c.sessions())[0].used_at).toBe(3_000);
      // A put apart from the range replaces the events and restarts the sum.
      await c.put(SID, range(100, 101));
      expect(await c.sessions()).toEqual([{ id: SID, used_at: 3_000, bytes: size(range(100, 101)) }]);
      await c.drop(SID);
      expect(await c.sessions()).toEqual([]);
    } finally {
      vi.useRealTimers();
    }
  });
});

const meta = (id: string): Artifact => ({ id, name: `${id}.png`, size: 3, kind: 'image' }) as unknown as Artifact;
const artifacts: [string, () => ArtifactCache][] = [
  ['memory', memoryArtifactCache],
  ['indexeddb', () => idbArtifactCache(`test-artifacts-${n++}`)],
];

describe.each(artifacts)('ArtifactCache, %s', (_name, make) => {
  it('gives back what was put, by id, and nothing for another id', async () => {
    const c = make();
    expect(await c.getMeta('a')).toBeUndefined();
    expect(await c.getBlob('a')).toBeUndefined();
    await c.putMeta(meta('a'));
    await c.putBlob('a', new Blob(['abc'], { type: 'image/png' }));
    expect(await c.getMeta('a')).toEqual(meta('a'));
    const blob = (await c.getBlob('a'))!;
    expect(blob.size).toBe(3);
    expect(await c.getMeta('b')).toBeUndefined();
    await c.putMeta({ ...meta('a'), name: 'renamed.png' });
    expect((await c.getMeta('a'))!.name).toBe('renamed.png');
  });

  it('lists the blobs with their last use and size; a hit renews a use older than an hour', async () => {
    vi.useFakeTimers({ toFake: ['Date'] });
    try {
      vi.setSystemTime(10_000_000);
      const c = make();
      expect(await c.entries()).toEqual([]);
      await c.putMeta(meta('a'));
      await c.putBlob('a', new Blob(['abc']));
      await c.putBlob('b', new Blob(['12345']));
      const byId = async () => Object.fromEntries((await c.entries()).map((e) => [e.id, e]));
      expect(await byId()).toEqual({
        a: { id: 'a', used_at: 10_000_000, bytes: 3 },
        b: { id: 'b', used_at: 10_000_000, bytes: 5 },
      });
      // A hit within the hour leaves the note; past it, the hit renews it.
      vi.setSystemTime(10_000_000 + 3_000_000);
      await c.getBlob('a');
      expect((await byId()).a.used_at).toBe(10_000_000);
      vi.setSystemTime(10_000_000 + 3_700_000);
      await c.getBlob('a');
      expect((await byId()).a.used_at).toBe(13_700_000);
      expect((await byId()).b.used_at).toBe(10_000_000);
      await c.forget(['a']);
      expect(Object.keys(await byId())).toEqual(['b']);
    } finally {
      vi.useRealTimers();
    }
  });
});

describe('the device caches belong to one user', () => {
  it('keep nothing for no user, and a database of its own for each', async () => {
    expect(deviceHistoryCache()).toBe(noHistoryCache);
    expect(deviceArtifactCache()).toBe(noArtifactCache);
    const h = deviceHistoryCache('u1');
    expect(deviceHistoryCache('u1')).toBe(h);
    await h.put(SID, range(1, 3));
    await deviceHistoryCache('u2').put(SID, range(1, 9));
    await deviceArtifactCache('u1').putMeta(meta('a'));
    expect(await h.range(SID)).toEqual({ from: 1, to: 3 });
    expect(await deviceHistoryCache('u2').range(SID)).toEqual({ from: 1, to: 9 });
    expect(await deviceArtifactCache('u2').getMeta('a')).toBeUndefined();
    const names = (await indexedDB.databases()).map((d) => d.name);
    expect(names).toEqual(expect.arrayContaining(['hqc-history-u1', 'hqc-history-u2', 'hqc-artifacts-u1']));
  });
});

/** Opens a database as another tab or an older build of the page would. */
function openRaw(name: string, version?: number, upgrade?: (db: IDBDatabase) => void): Promise<IDBDatabase> {
  return new Promise((resolve, reject) => {
    const req = version ? indexedDB.open(name, version) : indexedDB.open(name);
    if (upgrade) req.onupgradeneeded = () => upgrade(req.result);
    req.onsuccess = () => resolve(req.result);
    req.onerror = () => reject(req.error);
  });
}

describe('the device databases', () => {
  afterEach(() => vi.useRealTimers());

  it('an artifacts database made at version 1 gets the `used` store, and its blobs are listed as used at 0', async () => {
    const name = 'test-artifacts-v1';
    const old = await openRaw(name, 1, (db) => {
      db.createObjectStore('meta');
      db.createObjectStore('blobs');
    });
    await new Promise<void>((resolve) => {
      const t = old.transaction(['meta', 'blobs'], 'readwrite');
      t.objectStore('meta').put(meta('a'), 'a');
      t.objectStore('blobs').put(new Blob(['abcd']), 'a');
      t.oncomplete = () => resolve();
    });
    old.close();
    const c = idbArtifactCache(name);
    expect(await c.getMeta('a')).toEqual(meta('a'));
    // No note yet: 0 and 0 until a hit writes one.
    expect(await c.entries()).toEqual([{ id: 'a', used_at: 0, bytes: 0 }]);
    expect((await c.getBlob('a'))!.size).toBe(4);
    const [e] = await c.entries();
    expect(e.bytes).toBe(4);
    expect(e.used_at).toBeGreaterThan(0);
    const db = await openRaw(name);
    expect(db.version).toBe(2);
    expect([...db.objectStoreNames].sort()).toEqual(['blobs', 'meta', 'used']);
    db.close();
  });

  it('a delete from another tab is not blocked, and the cache is off until reload; this tab\'s own delete lets it reopen', async () => {
    const h = deviceHistoryCache('u-versionchange');
    const a = deviceArtifactCache('u-versionchange');
    await h.put(SID, range(1, 2));
    await a.putBlob('a', new Blob(['x']));
    const deleted = (name: string) =>
      new Promise<string>((resolve) => {
        const req = indexedDB.deleteDatabase(name);
        req.onsuccess = () => resolve('deleted');
        req.onblocked = () => resolve('blocked');
        req.onerror = () => resolve('failed');
      });
    expect(await deleted('hqc-history-u-versionchange')).toBe('deleted');
    expect(await deleted('hqc-artifacts-u-versionchange')).toBe('deleted');
    // A tab of the previous user cannot recreate the database it was deleted from.
    expect(await h.range(SID)).toBeUndefined();
    expect(await a.getBlob('a')).toBeUndefined();
    await h.put(SID, range(5, 6));
    expect(await h.range(SID)).toBeUndefined();
    expect((await indexedDB.databases()).map((d) => d.name)).not.toContain('hqc-history-u-versionchange');
    // The deletion this page asks for is its own doing: afterwards the cache works again.
    await deleteDeviceHistory('u-versionchange');
    await h.put(SID, range(5, 6));
    expect(await h.range(SID)).toEqual({ from: 5, to: 6 });
  });

  it('an open held back by a tab of an older build answers nothing instead of hanging', async () => {
    const old = await openRaw('hqc-artifacts-u-blocked-open', 1, (db) => {
      db.createObjectStore('meta');
      db.createObjectStore('blobs');
    });
    const a = deviceArtifactCache('u-blocked-open');
    const answered = (p: Promise<unknown>) => Promise.race([p.then(() => 'answered'), new Promise((r) => setTimeout(() => r('hung'), 500))]);
    expect(await answered(a.getMeta('a'))).toBe('answered');
    expect(await answered(a.entries())).toBe('answered');
    old.close();
  });

  it('a delete held back by another tab does not hang the cache, which reopens once the delete is done', async () => {
    const h = deviceHistoryCache('u-blocked-delete');
    await h.put(SID, range(1, 2));
    // A tab of an older build: it holds the database and does not close it on versionchange.
    const old = await openRaw('hqc-history-u-blocked-delete');
    const answered = (p: Promise<unknown>) => Promise.race([p.then(() => 'answered'), new Promise((r) => setTimeout(() => r('hung'), 500))]);
    expect(await answered(deleteDeviceHistory('u-blocked-delete'))).toBe('answered');
    expect(await answered(h.range(SID))).toBe('answered');
    old.close();
    // The queued delete goes through, then the cache works on a new database.
    for (let i = 0; i < 100 && (await indexedDB.databases()).some((d) => d.name === 'hqc-history-u-blocked-delete'); i++) await new Promise((r) => setTimeout(r, 10));
    await h.put(SID, range(7, 8));
    expect(await h.range(SID)).toEqual({ from: 7, to: 8 });
  });

  it('a call made while this page\'s delete waits behind another tab answers at once', async () => {
    const h = deviceHistoryCache('u-delete-same-tick');
    await h.put(SID, range(1, 2));
    const old = await openRaw('hqc-history-u-delete-same-tick');
    const answered = (p: Promise<unknown>) => Promise.race([p.then(() => 'answered'), new Promise((r) => setTimeout(() => r('hung'), 500))]);
    // No wait between them: the call comes before the delete's `blocked`.
    const deleting = deleteDeviceHistory('u-delete-same-tick');
    const reading = h.range(SID);
    expect(await answered(reading)).toBe('answered');
    expect(await answered(deleting)).toBe('answered');
    old.close();
  });

  it('Forget behind an upgrade held back by an older build answers, and so does the cache after it', async () => {
    const old = await openRaw('hqc-artifacts-u-forget-queued', 1, (db) => {
      db.createObjectStore('meta');
      db.createObjectStore('blobs');
    });
    const a = deviceArtifactCache('u-forget-queued');
    const answered = (p: Promise<unknown>) => Promise.race([p.then(() => 'answered'), new Promise((r) => setTimeout(() => r('hung'), 500))]);
    // The upgrade to version 2 is given up for this page load: the cache answers nothing.
    expect(await answered(a.getMeta('a'))).toBe('answered');
    expect(await answered(deleteDeviceArtifacts('u-forget-queued'))).toBe('answered');
    expect(await answered(a.getMeta('a'))).toBe('answered');
    expect(await answered(a.getBlob('a'))).toBe('answered');
    old.close();
    // The queued upgrade and delete go through, then the cache works on a new database.
    let kept: Blob | undefined;
    for (let i = 0; i < 100 && !kept; i++) {
      await new Promise((r) => setTimeout(r, 10));
      await a.putBlob('b', new Blob(['xy']));
      kept = await a.getBlob('b');
    }
    expect(kept?.size).toBe(2);
  });

  it('deleteDeviceHistory and deleteDeviceArtifacts close this page\'s connection and delete the user\'s database', async () => {
    const h = deviceHistoryCache('u-delete');
    const a = deviceArtifactCache('u-delete');
    await h.put(SID, range(1, 2));
    await a.putMeta(meta('a'));
    await deleteDeviceHistory('u-delete');
    await deleteDeviceArtifacts('u-delete');
    const names = (await indexedDB.databases()).map((d) => d.name);
    expect(names).not.toContain('hqc-history-u-delete');
    expect(names).not.toContain('hqc-artifacts-u-delete');
    // Another user's database is not touched, and a user with none is no error.
    await h.put(SID, range(1, 2));
    await deleteDeviceHistory('u-nobody');
    expect(await h.range(SID)).toEqual({ from: 1, to: 2 });
  });
});
