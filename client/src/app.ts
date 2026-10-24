// Application-wide state: who I am, my projects, the sessions of all of them and their live
// `state`/`queue_len`/unread from `subscribe_summary` (SPEC §6, §17.1), and the workspaces
// of the projects that have been looked at.

import { Api, type NewSessionOptions } from './api';
import type { Me, Project, SessionInfo, SessionSummary, Workspace } from './proto';
import { HubSocket, hubSocketUrl, type ConnState } from './ws';

export class AppState {
  me: Me | null = null;
  error: string | null = null;
  readonly projects = new Map<string, Project>();
  readonly sessions = new Map<string, SessionInfo>();
  /** By project id; filled by `loadWorkspaces`. */
  readonly workspaces = new Map<string, Workspace[]>();
  /** The project shown in the left column; the personal one until another is picked. */
  currentProjectId: string | null = null;
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
      this.currentProjectId ??= this.me.personal_project_id;
      await this.refreshSessions();
      this.error = null;
    } catch (e) {
      this.error = (e as Error).message;
    }
    this.socket.onSummary((list) => this.mergeSummary(list));
    this.socket.start();
    this.changed();
  }

  /** Projects and the sessions of every project. */
  async refreshSessions(): Promise<void> {
    if (!this.me) return;
    this.refreshing ??= (async () => {
      const projects = await this.api.projects();
      const lists = await Promise.all(projects.map((p) => this.api.sessions(p.id)));
      this.projects.clear();
      for (const p of projects) this.projects.set(p.id, p);
      this.sessions.clear();
      for (const s of lists.flat()) this.sessions.set(s.id, s);
      if (!this.currentProjectId || !this.projects.has(this.currentProjectId)) {
        this.currentProjectId = this.me!.personal_project_id;
      }
      this.changed();
    })().finally(() => (this.refreshing = null));
    return this.refreshing;
  }

  /** One session from the hub again: its `commands` (T27) are not in the summary. Errors are
   *  dropped; the next refresh brings it. */
  async refreshSession(id: string): Promise<void> {
    try {
      this.sessions.set(id, await this.api.session(id));
      this.changed();
    } catch {
      // Offline or gone: the list and the summary still have the session.
    }
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
        held_by: s.held_by,
      });
    }
    // A session created elsewhere: fetch the list again for its title.
    if (unknown) void this.refreshSessions().catch(() => {});
    this.changed();
  }

  /** Projects in the hub's order (the personal one first, it is the oldest). */
  sortedProjects(): Project[] {
    return [...this.projects.values()];
  }

  currentProject(): Project | undefined {
    return this.currentProjectId ? this.projects.get(this.currentProjectId) : undefined;
  }

  selectProject(id: string): void {
    if (this.currentProjectId === id) return;
    this.currentProjectId = id;
    this.changed();
  }

  /** Sessions of the project (the current one by default), newest activity first, without
   *  archived ones and without the workers' service sessions (listed apart, SPEC §10.2). */
  sortedSessions(projectId = this.currentProjectId): SessionInfo[] {
    return [...this.sessions.values()]
      .filter((s) => !s.archived && !s.system && s.project_id === projectId)
      .sort((a, b) => b.updated_at.localeCompare(a.updated_at) || b.last_seq - a.last_seq);
  }

  /** Archived sessions of the project, newest first: the "Archive" view of the list. */
  archivedSessions(projectId = this.currentProjectId): SessionInfo[] {
    return [...this.sessions.values()]
      .filter((s) => s.archived && !s.system && s.project_id === projectId)
      .sort((a, b) => b.updated_at.localeCompare(a.updated_at) || b.last_seq - a.last_seq);
  }

  /** Forgets a session the hub has deleted. */
  removeSession(id: string): void {
    if (this.sessions.delete(id)) this.changed();
  }

  /** The workers' service sessions (SPEC §10.2), by worker name: the "Workers" group.
   *  Revoking a worker archives its service session, so revoked workers are not listed. */
  workerSessions(): SessionInfo[] {
    return [...this.sessions.values()]
      .filter((s) => s.system && !s.archived)
      .sort((a, b) => a.title.localeCompare(b.title));
  }

  /** Sessions of the project that wait for the user (a request or an error), for the badge
   *  "needs you" on the project. */
  projectWaiting(projectId: string): number {
    let n = 0;
    for (const s of this.sessions.values()) {
      if (s.project_id === projectId && !s.archived && !s.system && (s.state === 'waiting' || s.state === 'error')) n++;
    }
    return n;
  }

  /** A worker's name: the title of its service session (SPEC §10.2). */
  workerName(workerId: string | undefined): string | undefined {
    if (!workerId) return undefined;
    for (const s of this.sessions.values()) if (s.system && s.worker_id === workerId) return s.title;
    return undefined;
  }

  /** Sessions on the worker with a turn on, input queued or a request open. */
  workerActive(workerId: string | undefined): number {
    let n = 0;
    for (const s of this.sessions.values()) {
      if (s.system || s.archived || s.worker_id !== workerId) continue;
      if (s.state === 'running' || s.state === 'waiting' || s.state === 'queued') n++;
    }
    return n;
  }

  /** The session's folder on its worker: the workspace path and `cwd` inside it; undefined
   *  until the project's workspaces are loaded. */
  sessionPath(s: SessionInfo): string | undefined {
    const w = this.workspaces.get(s.project_id)?.find((x) => x.id === s.workspace_id);
    if (!w) return undefined;
    const cwd = s.cwd.replace(/^\.\/?/, '').replace(/\/+$/, '');
    return cwd ? `${w.path.replace(/\/+$/, '')}/${cwd}` : w.path;
  }

  async createSession(title?: string, opts: NewSessionOptions = {}, projectId = this.currentProjectId): Promise<SessionInfo> {
    if (!this.me || !projectId) throw new Error('not signed in');
    const s = await this.api.createSession(projectId, title, opts);
    this.sessions.set(s.id, s);
    this.changed();
    return s;
  }

  async createProject(name: string): Promise<Project> {
    const p = await this.api.createProject(name);
    this.projects.set(p.id, p);
    this.currentProjectId = p.id;
    this.changed();
    return p;
  }

  async renameProject(id: string, name: string): Promise<void> {
    const p = await this.api.patchProject(id, { name });
    this.projects.set(p.id, p);
    this.changed();
  }

  async deleteProject(id: string): Promise<void> {
    await this.api.deleteProject(id);
    this.projects.delete(id);
    this.workspaces.delete(id);
    for (const [sid, s] of this.sessions) if (s.project_id === id) this.sessions.delete(sid);
    if (this.currentProjectId === id) this.currentProjectId = this.me?.personal_project_id ?? null;
    this.changed();
  }

  async loadWorkspaces(projectId: string): Promise<Workspace[]> {
    const list = await this.api.workspaces(projectId);
    this.workspaces.set(projectId, list);
    this.changed();
    return list;
  }

  /** After a workspace was added or removed: the project's default may have changed. */
  async workspacesChanged(projectId: string): Promise<void> {
    const [list, projects] = await Promise.all([this.api.workspaces(projectId), this.api.projects()]);
    this.workspaces.set(projectId, list);
    for (const p of projects) this.projects.set(p.id, p);
    this.changed();
  }

  async setDefaultWorkspace(projectId: string, workspaceId: string): Promise<void> {
    const p = await this.api.patchProject(projectId, { default_workspace_id: workspaceId });
    this.projects.set(p.id, p);
    this.changed();
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
