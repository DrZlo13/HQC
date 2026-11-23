// What the device keeps (`HistoryCache`, `ArtifactCache`): the same contract tests run against
// the in-memory implementation and the IndexedDB one (fake-indexeddb stands in for the browser).

import 'fake-indexeddb/auto';
import { describe, expect, it } from 'vitest';
import { deviceArtifactCache, idbArtifactCache, memoryArtifactCache, noArtifactCache, type ArtifactCache } from '../src/artifactcache';
import { deviceHistoryCache, idbHistoryCache, memoryHistoryCache, noHistoryCache, type HistoryCache } from '../src/historycache';
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
