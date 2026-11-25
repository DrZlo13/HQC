// What this device already downloaded of the sessions' history (SPEC §6): each event as the hub
// sent it, by session and `seq`, and per session the one range of `seq` the copy holds whole
// (the hub's `seq` of a session have no gaps, so a range holds exactly `to - from + 1` events).
// Only `put` moves a range, with the newest page (which may be one the copy holds already), an
// older page or the fresh events of the subscription; an event the hub sends again goes through
// `update`, which rewrites a copy inside the range and leaves the range as it is. The record of a range
// also keeps when the session was last used (`touch`, a chat opening; a first `put` starts it)
// and how many bytes its events take (`JSON.stringify` of each, those outside the range a `put`
// found stored), for the clean-up of the device (`sessions`). A record without `used_at` counts
// as used at 0: the copies made before it was kept go at the first clean-up (nothing was in
// service then). One database per user (`hqc-history-<user id>`): the browser may be shared.
// Every failure of the browser's storage (no IndexedDB, a private window, a full quota) is
// swallowed: the chat then works as without the cache, downloading again.

import { deleteIdb, IdbConnection } from './idbconnection';
import type { DeliveryChange } from './proto';

export interface HistoryRange {
  from: number;
  to: number;
}

/** What the device keeps of one session or one attachment: when it was last used (ms) and its
 *  size in bytes. */
export interface CopyUse {
  id: string;
  used_at: number;
  bytes: number;
}

export interface HistoryCache {
  /** The range of `seq` the device holds of the session. */
  range(sessionId: string): Promise<HistoryRange | undefined>;
  /** The events `lo`..`hi` as the hub sent them, oldest first; undefined unless all of them
   *  (`hi - lo + 1`) came back. */
  read(sessionId: string, lo: number, hi: number): Promise<unknown[] | undefined>;
  /** Keeps `raws` (those with a numeric `seq` and this session's id). A range they overlap or
   *  adjoin grows to hold them; otherwise they become the range, and the session's other
   *  events go. */
  put(sessionId: string, raws: unknown[]): Promise<void>;
  /** Rewrites events the hub sent again, only those inside the stored range; never moves it. */
  update(sessionId: string, raws: unknown[]): Promise<void>;
  /** The new `delivery` of an input the device holds. */
  patch(sessionId: string, change: DeliveryChange): Promise<void>;
  /** Forgets the session. */
  drop(sessionId: string): Promise<void>;
  /** Marks the stored session as used now (nothing for one that is not stored). */
  touch(sessionId: string): Promise<void>;
  /** Every stored session with its last use and size. */
  sessions(): Promise<CopyUse[]>;
}

/** Keeps nothing. */
export const noHistoryCache: HistoryCache = {
  range: async () => undefined,
  read: async () => undefined,
  put: async () => {},
  update: async () => {},
  patch: async () => {},
  drop: async () => {},
  touch: async () => {},
  sessions: async () => [],
};

type Raw = Record<string, unknown>;

/** The events of `raws` that `put` keeps, and the lowest and highest `seq` among them. */
function keepable(sessionId: string, raws: unknown[]): { rows: Raw[]; a: number; b: number } {
  const rows: Raw[] = [];
  let a = Infinity;
  let b = -Infinity;
  for (const r of raws) {
    const o = r as Raw | null;
    if (!o || typeof o.seq !== 'number' || o.session_id !== sessionId) continue;
    rows.push(o);
    a = Math.min(a, o.seq);
    b = Math.max(b, o.seq);
  }
  return { rows, a, b };
}

/** What the events of `rows` outside `stored` take in the copy: the ones inside it are held
 *  already (the newest page is put again at every open). */
function sizeOf(rows: Raw[], stored?: HistoryRange): number {
  let n = 0;
  for (const r of rows) {
    const seq = r.seq as number;
    if (!stored || seq < stored.from || seq > stored.to) n += JSON.stringify(r).length;
  }
  return n;
}

/** The range after `put` of events `a`..`b`, and whether the other events of the session go. */
function grown(stored: HistoryRange | undefined, a: number, b: number): { range: HistoryRange; replace: boolean } {
  if (stored && a <= stored.to + 1 && b >= stored.from - 1) {
    return { range: { from: Math.min(stored.from, a), to: Math.max(stored.to, b) }, replace: false };
  }
  return { range: { from: a, to: b }, replace: true };
}

/** `raw` with the change of a `delivery` frame, as `Timeline.applyDelivery` makes it; undefined
 *  when it is another event. */
function delivered(raw: Raw, d: DeliveryChange): Raw | undefined {
  if (raw.event_id !== d.event_id) return undefined;
  const next: Raw = { ...raw, delivery: d.delivery, delivered_at: d.delivered_at };
  if (d.delivered_at === undefined) delete next.delivered_at;
  const payload = raw.payload;
  if ((d.joined === true || d.quiet === true) && payload && typeof payload === 'object') {
    const flags: Raw = {};
    if (d.joined === true) flags.joined = true;
    if (d.quiet === true) flags.quiet = true;
    if (d.joined_after !== undefined) flags.joined_after = d.joined_after;
    next.payload = { ...payload, ...flags };
  }
  return next;
}

/** Kept in memory only: for tests. */
export function memoryHistoryCache(): HistoryCache {
  const sessions = new Map<string, { range: HistoryRange; events: Map<number, Raw>; used_at: number; bytes: number }>();
  return {
    range: async (sid) => sessions.get(sid)?.range,
    read: async (sid, lo, hi) => {
      const events = sessions.get(sid)?.events;
      const out: unknown[] = [];
      for (let seq = lo; seq <= hi; seq++) {
        const e = events?.get(seq);
        if (e) out.push(e);
      }
      return out.length === hi - lo + 1 ? out : undefined;
    },
    put: async (sid, raws) => {
      const { rows, a, b } = keepable(sid, raws);
      if (!rows.length) return;
      const stored = sessions.get(sid);
      const { range, replace } = grown(stored?.range, a, b);
      const events = replace ? new Map<number, Raw>() : (stored?.events ?? new Map<number, Raw>());
      for (const r of rows) events.set(r.seq as number, r);
      const bytes = (replace ? 0 : (stored?.bytes ?? 0)) + sizeOf(rows, replace ? undefined : stored?.range);
      sessions.set(sid, { range, events, used_at: stored?.used_at ?? Date.now(), bytes });
    },
    update: async (sid, raws) => {
      const s = sessions.get(sid);
      if (!s) return;
      for (const r of keepable(sid, raws).rows) {
        const seq = r.seq as number;
        if (seq >= s.range.from && seq <= s.range.to) s.events.set(seq, r);
      }
    },
    patch: async (sid, d) => {
      const events = sessions.get(sid)?.events;
      const next = delivered(events?.get(d.seq) ?? {}, d);
      if (next) events!.set(d.seq, next);
    },
    drop: async (sid) => void sessions.delete(sid),
    touch: async (sid) => {
      const s = sessions.get(sid);
      if (s) s.used_at = Date.now();
    },
    sessions: async () => [...sessions].map(([id, s]) => ({ id, used_at: s.used_at, bytes: s.bytes })),
  };
}

const settled = <T>(r: IDBRequest<T>): Promise<T> =>
  new Promise((resolve, reject) => {
    r.onsuccess = () => resolve(r.result);
    r.onerror = () => reject(r.error);
  });

/** In IndexedDB: store `events` holds the events by session id and `seq`, store `ranges` the
 *  range of each session by its id with its last use and size. */
export function idbHistoryCache(name: string): HistoryCache {
  const connection = new IdbConnection(name, 1, (db) => {
    db.createObjectStore('events', { keyPath: ['session_id', 'seq'] });
    db.createObjectStore('ranges', { keyPath: 'session_id' });
  });
  const open = () => connection.open();
  /** `body` in one transaction over both stores; what it returns once the transaction is
   *  complete, or undefined when anything failed. */
  const tx = async <T>(
    mode: IDBTransactionMode,
    body: (events: IDBObjectStore, ranges: IDBObjectStore) => Promise<T>,
  ): Promise<T | undefined> => {
    try {
      const d = await open();
      if (!d) return undefined;
      const t = d.transaction(['events', 'ranges'], mode);
      const done = new Promise<void>((resolve, reject) => {
        t.oncomplete = () => resolve();
        t.onerror = t.onabort = () => reject(t.error);
      });
      // A failure of `body` aborts the transaction, which rejects `done` too.
      done.catch(() => {});
      try {
        const out = await body(t.objectStore('events'), t.objectStore('ranges'));
        await done;
        return out;
      } catch (e) {
        try {
          t.abort();
        } catch {
          // Finished already.
        }
        throw e;
      }
    } catch {
      return undefined;
    }
  };
  const whole = (sid: string) => IDBKeyRange.bound([sid, -Infinity], [sid, Infinity]);
  return {
    range: (sid) =>
      tx('readonly', async (_e, ranges) => {
        const r = (await settled(ranges.get(sid))) as (HistoryRange & { session_id: string }) | undefined;
        return r && { from: r.from, to: r.to };
      }),
    read: (sid, lo, hi) =>
      tx('readonly', async (events) => {
        const rows = (await settled(events.getAll(IDBKeyRange.bound([sid, lo], [sid, hi])))) as unknown[];
        return rows.length === hi - lo + 1 ? rows : undefined;
      }),
    put: async (sid, raws) => {
      const { rows, a, b } = keepable(sid, raws);
      if (!rows.length) return;
      await tx('readwrite', async (events, ranges) => {
        const stored = (await settled(ranges.get(sid))) as HistoryRange | undefined;
        const { range, replace } = grown(stored, a, b);
        if (replace) events.delete(whole(sid));
        for (const r of rows) events.put(r);
        const used = stored as (HistoryRange & { used_at?: number; bytes?: number }) | undefined;
        ranges.put({
          session_id: sid,
          ...range,
          used_at: used?.used_at ?? Date.now(),
          bytes: (replace ? 0 : (used?.bytes ?? 0)) + sizeOf(rows, replace ? undefined : stored),
        });
      });
    },
    update: async (sid, raws) => {
      const { rows } = keepable(sid, raws);
      if (!rows.length) return;
      await tx('readwrite', async (events, ranges) => {
        const stored = (await settled(ranges.get(sid))) as HistoryRange | undefined;
        if (!stored) return;
        for (const r of rows) {
          const seq = r.seq as number;
          if (seq >= stored.from && seq <= stored.to) events.put(r);
        }
      });
    },
    patch: async (sid, d) => {
      await tx('readwrite', async (events) => {
        const next = delivered(((await settled(events.get([sid, d.seq]))) as Raw | undefined) ?? {}, d);
        if (next) events.put(next);
      });
    },
    drop: async (sid) => {
      await tx('readwrite', async (events, ranges) => {
        events.delete(whole(sid));
        ranges.delete(sid);
      });
    },
    touch: async (sid) => {
      await tx('readwrite', async (_e, ranges) => {
        const r = (await settled(ranges.get(sid))) as Raw | undefined;
        if (r) ranges.put({ ...r, used_at: Date.now() });
      });
    },
    sessions: async () =>
      (await tx('readonly', async (_e, ranges) => {
        const rows = (await settled(ranges.getAll())) as { session_id: string; used_at?: number; bytes?: number }[];
        // No note: made before the notes were kept, used at 0, so the first clean-up drops it.
        return rows.map((r) => ({ id: r.session_id, used_at: r.used_at ?? 0, bytes: r.bytes ?? 0 }));
      })) ?? [],
  };
}

/** The cache `c` resolves to, for a caller that has it before it is known: each call waits. */
export function laterHistoryCache(c: Promise<HistoryCache>): HistoryCache {
  return {
    range: async (sid) => (await c).range(sid),
    read: async (sid, lo, hi) => (await c).read(sid, lo, hi),
    put: async (sid, raws) => (await c).put(sid, raws),
    update: async (sid, raws) => (await c).update(sid, raws),
    patch: async (sid, d) => (await c).patch(sid, d),
    drop: async (sid) => (await c).drop(sid),
    touch: async (sid) => (await c).touch(sid),
    sessions: async () => (await c).sessions(),
  };
}

const devices = new Map<string, HistoryCache>();

/** The cache of this device for the user: IndexedDB `hqc-history-<user id>`, or nothing where
 *  there is none or the user is not known (the browser may be shared). One per user and page. */
export function deviceHistoryCache(userId?: string): HistoryCache {
  if (!userId || typeof indexedDB === 'undefined') return noHistoryCache;
  let c = devices.get(userId);
  if (!c) devices.set(userId, (c = idbHistoryCache(`hqc-history-${userId}`)));
  return c;
}

/** Deletes the user's database from this device: this page's connection is closed first, and the
 *  next call of the cache reopens an empty one (once the deletion is not blocked by another tab). */
export function deleteDeviceHistory(userId: string): Promise<void> {
  return deleteIdb(`hqc-history-${userId}`);
}
