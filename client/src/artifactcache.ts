// What this device already downloaded of the artifacts (SPEC §12): the record and the bytes by
// artifact id. The bytes under an id never change (the hub keeps one row per owner, sha256 and
// name), so they need no lifetime. The record may: a file attached again names its kind anew
// (SPEC 0.128), and an event naming another kind than the kept record makes the chat ask the
// hub again (`ArtifactStore.meta` with `fresh`), which rewrites the copy. Every failure of the
// browser's storage (no IndexedDB, a private window, a full quota) is swallowed: the chat then
// works as without the cache, downloading again. Per blob the cache also keeps when it was last
// used (a `putBlob`, a `getBlob` hit not noted in the last hour) and its size, for the clean-up
// of the device (`entries`). One database per user (`hqc-artifacts-<user id>`): the browser may
// be shared.

import { deleteIdb, IdbConnection } from './idbconnection';
import type { Artifact } from './proto';
import type { CopyUse } from './historycache';

/** A `getBlob` hit renews the blob's last use when the noted one is older than this. */
const USED_RENEW_MS = 3_600_000;

export interface ArtifactCache {
  getMeta(id: string): Promise<Artifact | undefined>;
  putMeta(meta: Artifact): Promise<void>;
  getBlob(id: string): Promise<Blob | undefined>;
  putBlob(id: string, blob: Blob): Promise<void>;
  /** Forgets the artifacts: their records and bytes. */
  forget(ids: string[]): Promise<void>;
  /** Every stored blob with its last use and size (a blob with no note: 0 and 0). */
  entries(): Promise<CopyUse[]>;
}

/** Keeps nothing. */
export const noArtifactCache: ArtifactCache = {
  getMeta: async () => undefined,
  putMeta: async () => {},
  getBlob: async () => undefined,
  putBlob: async () => {},
  forget: async () => {},
  entries: async () => [],
};

/** Kept in memory only: for tests. */
export function memoryArtifactCache(): ArtifactCache {
  const metas = new Map<string, Artifact>();
  const blobs = new Map<string, Blob>();
  const used = new Map<string, { used_at: number; bytes: number }>();
  return {
    getMeta: async (id) => metas.get(id),
    putMeta: async (m) => void metas.set(m.id, m),
    getBlob: async (id) => {
      const b = blobs.get(id);
      if (b && Date.now() - (used.get(id)?.used_at ?? 0) > USED_RENEW_MS) used.set(id, { used_at: Date.now(), bytes: b.size });
      return b;
    },
    putBlob: async (id, b) => {
      blobs.set(id, b);
      used.set(id, { used_at: Date.now(), bytes: b.size });
    },
    forget: async (ids) => {
      for (const id of ids) {
        metas.delete(id);
        blobs.delete(id);
        used.delete(id);
      }
    },
    entries: async () => [...blobs.keys()].map((id) => ({ id, used_at: used.get(id)?.used_at ?? 0, bytes: used.get(id)?.bytes ?? 0 })),
  };
}

type StoreName = 'meta' | 'blobs' | 'used';

/** In IndexedDB: store `meta` holds the records, store `blobs` the bytes, both by artifact id;
 *  store `used` the last use and size of each blob by the same id. */
export function idbArtifactCache(name: string): ArtifactCache {
  // Version 2 added `used`; a database of version 1 gets just that.
  const connection = new IdbConnection(name, 2, (db) => {
    for (const store of ['meta', 'blobs', 'used']) {
      if (!db.objectStoreNames.contains(store)) db.createObjectStore(store);
    }
  });
  const open = () => connection.open();
  const get = async <T>(store: StoreName, key: string): Promise<T | undefined> => {
    try {
      const d = await open();
      if (!d) return undefined;
      return await new Promise<T | undefined>((resolve, reject) => {
        const r = d.transaction(store).objectStore(store).get(key);
        r.onsuccess = () => resolve(r.result as T | undefined);
        r.onerror = () => reject(r.error);
      });
    } catch {
      return undefined;
    }
  };
  const put = async (store: StoreName, key: string, value: unknown): Promise<void> => {
    try {
      const d = await open();
      if (!d) return;
      await new Promise<void>((resolve, reject) => {
        const tx = d.transaction(store === 'blobs' ? ['blobs', 'used'] : store, 'readwrite');
        tx.objectStore(store).put(value, key);
        if (store === 'blobs') tx.objectStore('used').put({ used_at: Date.now(), bytes: (value as Blob).size }, key);
        tx.oncomplete = () => resolve();
        tx.onerror = tx.onabort = () => reject(tx.error);
      });
    } catch {
      // Not kept: it is downloaded again next time.
    }
  };
  const forget = async (ids: string[]): Promise<void> => {
    if (!ids.length) return;
    try {
      const d = await open();
      if (!d) return;
      await new Promise<void>((resolve, reject) => {
        const tx = d.transaction(['meta', 'blobs', 'used'], 'readwrite');
        for (const id of ids) {
          tx.objectStore('meta').delete(id);
          tx.objectStore('blobs').delete(id);
          tx.objectStore('used').delete(id);
        }
        tx.oncomplete = () => resolve();
        tx.onerror = tx.onabort = () => reject(tx.error);
      });
    } catch {
      // Stays on the device: the next clean-up of the browser's storage takes it.
    }
  };
  const getBlob = async (id: string): Promise<Blob | undefined> => {
    try {
      const d = await open();
      if (!d) return undefined;
      const [b, note] = await new Promise<[Blob | undefined, { used_at: number } | undefined]>((resolve, reject) => {
        const tx = d.transaction(['blobs', 'used']);
        const blob = tx.objectStore('blobs').get(id);
        const used = tx.objectStore('used').get(id);
        tx.oncomplete = () => resolve([blob.result as Blob | undefined, used.result as { used_at: number } | undefined]);
        tx.onerror = tx.onabort = () => reject(tx.error);
      });
      // No note: made before the notes were kept, used at 0, so the first clean-up drops it.
      if (b && Date.now() - (note?.used_at ?? 0) > USED_RENEW_MS) void put('used', id, { used_at: Date.now(), bytes: b.size });
      return b;
    } catch {
      return undefined;
    }
  };
  const entries = async (): Promise<CopyUse[]> => {
    try {
      const d = await open();
      if (!d) return [];
      return await new Promise<CopyUse[]>((resolve, reject) => {
        const tx = d.transaction(['blobs', 'used']);
        const keys = tx.objectStore('blobs').getAllKeys();
        const notes = tx.objectStore('used').getAllKeys();
        const values = tx.objectStore('used').getAll();
        tx.oncomplete = () => {
          const byId = new Map<string, { used_at: number; bytes: number }>();
          (notes.result as string[]).forEach((id, i) => byId.set(id, values.result[i]));
          // No note: made before the notes were kept, used at 0, so the first clean-up drops it.
          resolve((keys.result as string[]).map((id) => ({ id, used_at: byId.get(id)?.used_at ?? 0, bytes: byId.get(id)?.bytes ?? 0 })));
        };
        tx.onerror = tx.onabort = () => reject(tx.error);
      });
    } catch {
      return [];
    }
  };
  return {
    getMeta: (id) => get<Artifact>('meta', id),
    putMeta: (m) => put('meta', m.id, m),
    getBlob,
    putBlob: (id, b) => put('blobs', id, b),
    forget,
    entries,
  };
}

/** The cache `c` resolves to, for a caller that has it before it is known: each call waits. */
export function laterArtifactCache(c: Promise<ArtifactCache>): ArtifactCache {
  return {
    getMeta: async (id) => (await c).getMeta(id),
    putMeta: async (m) => (await c).putMeta(m),
    getBlob: async (id) => (await c).getBlob(id),
    putBlob: async (id, b) => (await c).putBlob(id, b),
    forget: async (ids) => (await c).forget(ids),
    entries: async () => (await c).entries(),
  };
}

const devices = new Map<string, ArtifactCache>();

/** The cache of this device for the user: IndexedDB `hqc-artifacts-<user id>`, or nothing where
 *  there is none or the user is not known (the browser may be shared). One per user and page. */
export function deviceArtifactCache(userId?: string): ArtifactCache {
  if (!userId || typeof indexedDB === 'undefined') return noArtifactCache;
  let c = devices.get(userId);
  if (!c) devices.set(userId, (c = idbArtifactCache(`hqc-artifacts-${userId}`)));
  return c;
}

/** Deletes the user's database from this device: this page's connection is closed first, and the
 *  next call of the cache reopens an empty one (once the deletion is not blocked by another tab). */
export function deleteDeviceArtifacts(userId: string): Promise<void> {
  return deleteIdb(`hqc-artifacts-${userId}`);
}
