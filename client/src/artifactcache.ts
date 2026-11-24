// What this device already downloaded of the artifacts (SPEC §12): the record and the bytes by
// artifact id. The bytes under an id never change (the hub keeps one row per owner, sha256 and
// name), so they need no lifetime. The record may: a file attached again names its kind anew
// (SPEC 0.128), and an event naming another kind than the kept record makes the chat ask the
// hub again (`ArtifactStore.meta` with `fresh`), which rewrites the copy. Every failure of the
// browser's storage (no IndexedDB, a private window, a full quota) is swallowed: the chat then
// works as without the cache, downloading again. One database per user
// (`hqc-artifacts-<user id>`): the browser may be shared.

import type { Artifact } from './proto';

export interface ArtifactCache {
  getMeta(id: string): Promise<Artifact | undefined>;
  putMeta(meta: Artifact): Promise<void>;
  getBlob(id: string): Promise<Blob | undefined>;
  putBlob(id: string, blob: Blob): Promise<void>;
  /** Forgets the artifacts: their records and bytes. */
  forget(ids: string[]): Promise<void>;
}

/** Keeps nothing. */
export const noArtifactCache: ArtifactCache = {
  getMeta: async () => undefined,
  putMeta: async () => {},
  getBlob: async () => undefined,
  putBlob: async () => {},
  forget: async () => {},
};

/** Kept in memory only: for tests. */
export function memoryArtifactCache(): ArtifactCache {
  const metas = new Map<string, Artifact>();
  const blobs = new Map<string, Blob>();
  return {
    getMeta: async (id) => metas.get(id),
    putMeta: async (m) => void metas.set(m.id, m),
    getBlob: async (id) => blobs.get(id),
    putBlob: async (id, b) => void blobs.set(id, b),
    forget: async (ids) => {
      for (const id of ids) {
        metas.delete(id);
        blobs.delete(id);
      }
    },
  };
}

type StoreName = 'meta' | 'blobs';

function openDb(name: string): Promise<IDBDatabase> {
  return new Promise((resolve, reject) => {
    const req = indexedDB.open(name, 1);
    req.onupgradeneeded = () => {
      req.result.createObjectStore('meta');
      req.result.createObjectStore('blobs');
    };
    req.onsuccess = () => resolve(req.result);
    req.onerror = () => reject(req.error);
  });
}

/** In IndexedDB: store `meta` holds the records, store `blobs` the bytes, both by artifact id. */
export function idbArtifactCache(name: string): ArtifactCache {
  let db: Promise<IDBDatabase | null> | undefined;
  // A database that does not open is the same as none: every call then answers nothing.
  const open = () => (db ??= openDb(name).catch(() => null));
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
        const tx = d.transaction(store, 'readwrite');
        tx.objectStore(store).put(value, key);
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
        const tx = d.transaction(['meta', 'blobs'], 'readwrite');
        for (const id of ids) {
          tx.objectStore('meta').delete(id);
          tx.objectStore('blobs').delete(id);
        }
        tx.oncomplete = () => resolve();
        tx.onerror = tx.onabort = () => reject(tx.error);
      });
    } catch {
      // Stays on the device: the next clean-up of the browser's storage takes it.
    }
  };
  return {
    getMeta: (id) => get<Artifact>('meta', id),
    putMeta: (m) => put('meta', m.id, m),
    getBlob: (id) => get<Blob>('blobs', id),
    putBlob: (id, b) => put('blobs', id, b),
    forget,
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
