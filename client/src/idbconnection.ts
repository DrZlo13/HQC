// This page's one connection to an IndexedDB database of the device's copies (SPEC §6, §12), shared
// by the history cache and the artifact cache. A database that does not open is the same as none:
// every call of the cache then answers nothing. That holds for an open that is blocked too (a tab
// of an older build holds the database at a lower version and does not close it): it is given up
// for this page load, and the connection that may still come after is closed at once.
// Another tab deleting or upgrading the database closes the connection and switches this page's
// cache off until reload, so that a tab of the previous user cannot recreate what was deleted. The
// deletion this page asks for (`deleteIdb`) closes its connection itself and switches the cache off
// until the deletion is done, then lets it reopen; it is not waited for while another tab holds it.

const connections = new Map<string, IdbConnection>();

export class IdbConnection {
  private db: Promise<IDBDatabase | null> | undefined;
  private conn: IDBDatabase | null = null;
  /** What `db` is while the cache is off. */
  private off: Promise<IDBDatabase | null> | undefined;
  /** This page's open was given up on `blocked` and is still queued behind the other tab: a
   *  deletion asked for now waits behind it and gets no event until that tab lets go. */
  queuedOpen = false;

  constructor(
    private readonly name: string,
    private readonly version: number,
    private readonly upgrade: (db: IDBDatabase) => void,
  ) {
    connections.set(name, this);
  }

  /** The connection, or null where there is none. */
  open(): Promise<IDBDatabase | null> {
    return (this.db ??= this.openDb().then((d) => (this.conn = d), () => null));
  }

  private openDb(): Promise<IDBDatabase> {
    return new Promise((resolve, reject) => {
      const req = indexedDB.open(this.name, this.version);
      let given = false;
      req.onupgradeneeded = () => this.upgrade(req.result);
      req.onblocked = () => {
        given = true;
        this.queuedOpen = true;
        reject(new Error('blocked'));
      };
      req.onsuccess = () => {
        const d = req.result;
        if (given) {
          this.queuedOpen = false;
          d.close();
          return;
        }
        d.onversionchange = () => {
          d.close();
          if (this.conn === d) this.conn = null;
          this.switchOff();
        };
        resolve(d);
      };
      req.onerror = () => reject(req.error);
    });
  }

  /** This page is about to delete the database: closes the connection and keeps the cache off
   *  until `deleted`, since a call that opened it meanwhile would wait behind the
   *  deletion. */
  closeForDelete(): void {
    this.conn?.close();
    this.conn = null;
    this.switchOff();
  }

  private switchOff(): void {
    this.db = this.off = Promise.resolve(null);
  }

  /** The deletion is over (or failed): the cache may reopen. No connection is open while it
   *  runs, so no other tab can switch the cache off meanwhile. */
  deleted(): void {
    if (this.db === this.off) this.db = undefined;
  }
}

/** Deletes the database `name` from this device. Resolves once it is deleted or failed, or at
 *  once when another tab holds it (the deletion goes on when that tab lets go, the cache stays
 *  off until then). */
export async function deleteIdb(name: string): Promise<void> {
  const c = connections.get(name);
  try {
    c?.closeForDelete();
    if (typeof indexedDB === 'undefined') return c?.deleted();
    await new Promise<void>((resolve) => {
      const req = indexedDB.deleteDatabase(name);
      req.onsuccess = req.onerror = () => {
        c?.deleted();
        resolve();
      };
      req.onblocked = () => resolve();
      // Behind this page's own queued open no `blocked` comes: the other tab holds it all the same.
      if (c?.queuedOpen) resolve();
    });
  } catch {
    // Stays on the device: the next clean-up of the browser's storage takes it.
    c?.deleted();
  }
}
