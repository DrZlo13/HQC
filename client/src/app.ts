// Application-wide state: who I am, the sessions of the personal project and their live
// `state`/`queue_len`/unread from `subscribe_summary` (SPEC §6, §17.1).

import { Api } from './api';
import type { Me, SessionInfo, SessionSummary } from './proto';
import { HubSocket, hubSocketUrl, type ConnState } from './ws';

export class AppState {
  me: Me | null = null;
  error: string | null = null;
  readonly sessions = new Map<string, SessionInfo>();
  conn: ConnState = 'closed';
  private readonly listeners = new Set<() => void>();
  private refreshing: Promise<void> | null = null;

  constructor(
    readonly api: Api,
    readonly socket: HubSocket,
  ) {
    socket.onState((s) => {
      this.conn = s;
      this.changed();
    });
  }

  onChange(fn: () => void): () => void {
    this.listeners.add(fn);
    return () => this.listeners.delete(fn);
  }

  private changed(): void {
    for (const fn of this.listeners) fn();
  }

  async init(): Promise<void> {
    try {
      this.me = await this.api.me();
      await this.refreshSessions();
      this.error = null;
    } catch (e) {
      this.error = (e as Error).message;
    }
    this.socket.onSummary((list) => this.mergeSummary(list));
    this.socket.start();
    this.changed();
  }

  async refreshSessions(): Promise<void> {
    if (!this.me) return;
    this.refreshing ??= this.api
      .sessions(this.me.personal_project_id)
      .then((list) => {
        this.sessions.clear();
        for (const s of list) this.sessions.set(s.id, s);
        this.changed();
      })
      .finally(() => (this.refreshing = null));
    return this.refreshing;
  }

  mergeSummary(list: SessionSummary[]): void {
    let unknown = false;
    for (const s of list) {
      const known = this.sessions.get(s.session_id);
      if (!known) {
        unknown = true;
        continue;
      }
      this.sessions.set(s.session_id, {
        ...known,
        title: s.title,
        state: s.state,
        queue_len: s.queue_len,
        unread: s.unread,
        last_seq: s.last_seq,
        last_read_seq: s.last_read_seq,
        // `archived` is the top state of SPEC §17.1, so the summary carries it.
        archived: s.state === 'archived',
      });
    }
    // A session created elsewhere: fetch the list again for its title.
    if (unknown) void this.refreshSessions().catch(() => {});
    this.changed();
  }

  /** Sessions newest-activity first, archived last. */
  sortedSessions(): SessionInfo[] {
    return [...this.sessions.values()]
      .filter((s) => !s.archived)
      .sort((a, b) => b.updated_at.localeCompare(a.updated_at) || b.last_seq - a.last_seq);
  }

  async createSession(title?: string): Promise<SessionInfo> {
    if (!this.me) throw new Error('not signed in');
    const s = await this.api.createSession(this.me.personal_project_id, title);
    this.sessions.set(s.id, s);
    this.changed();
    return s;
  }

  /** Local echo of the read cursor, so the badge clears before the summary arrives. */
  markRead(sessionId: string): void {
    const s = this.sessions.get(sessionId);
    if (s && s.unread) {
      this.sessions.set(sessionId, { ...s, unread: 0 });
      this.changed();
    }
  }
}

export function createApp(): AppState {
  return new AppState(new Api(), new HubSocket({ url: hubSocketUrl() }));
}
