// Application-wide state: who I am, my projects, the sessions of all of them and their live
// `state`/`queue_len`/unread from `subscribe_summary` (SPEC §6, §17.1), the workspaces and
// the list order (T50) of the projects that have been looked at, and a session archived from
// the list that has not gone to the hub yet.

import { Api, ApiError, type NewSessionOptions, type SessionPatch } from './api';
import type {
  ClarifyAnswer,
  ListMode,
  Me,
  OpenRequest,
  PermissionMode,
  Project,
  ProjectColor,
  SessionInfo,
  SessionOrder,
  SessionSummary,
  Theme,
  Workspace,
} from './proto';
import { arrange } from './ui/sessions';
import { HubSocket, hubSocketUrl, type ConnState } from './ws';

/** How long "Archived «…» · Undo" stays before the archive goes to the hub (T50). */
export const ARCHIVE_UNDO_MS = 5000;

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
  /** Every open request of the user, oldest first, as the hub last sent it (T37). */
  requests: OpenRequest[] = [];
  /** By project id: the user's list order (T50), as the hub last gave it; `loadOrder`. */
  readonly orders = new Map<string, SessionOrder>();
  /** Archived from the list, not sent yet: hidden at once, sent when the Undo toast ends,
   *  when another one is archived, before Reorder or archive mode, or when the page is left
   *  (T50). */
  pendingArchive: { id: string; title: string } | null = null;
  private archiveTimer: ReturnType<typeof setTimeout> | undefined;
  /** The list shows the project's archive (T50); back to the sessions on a project switch. */
  archiveMode = false;
  /** A list action the hub refused (archive, restore, delete, rename, order). */
  listNotice: string | null = null;
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
      this.syncTimezone();
      await this.refreshSessions();
      this.error = null;
    } catch (e) {
      this.error = (e as Error).message;
    }
    this.socket.onRequests((list) => this.setRequests(list));
    this.socket.onSummary((list) => this.mergeSummary(list));
    this.socket.start();
    this.watchPageLeave(globalThis as unknown as Window);
    this.changed();
  }

  /** Leaving the page (or hiding it: a phone switching apps) within the Undo window archives
   *  at once; `keepalive` lets the request outlive the page (T50). */
  watchPageLeave(win: Window): void {
    win.addEventListener('pagehide', () => this.commitArchive(true));
    win.document.addEventListener('visibilitychange', () => {
      if (win.document.visibilityState === 'hidden') this.commitArchive(true);
    });
  }

  /** The hub's zone of the user follows the browser's: schedules default to it (SPEC §16). */
  private syncTimezone(): void {
    const tz = Intl.DateTimeFormat().resolvedOptions().timeZone;
    if (!tz || !this.me || this.me.timezone === undefined || this.me.timezone === tz) return;
    this.api.setTimezone(tz).then(
      (me) => (this.me = me),
      () => {},
    );
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
      // Archived or restored elsewhere: `archived_at` is not in the summary (T50).
      if (known.archived !== (s.state === 'archived')) void this.refreshSession(s.session_id);
      // Moved elsewhere: its slug, workspace and folder are not in the summary, and a new
      // project is not on the rail yet.
      if (known.project_id !== s.project_id) {
        this.workspaces.delete(s.project_id);
        if (this.projects.has(s.project_id)) void this.refreshSession(s.session_id);
        else unknown = true;
      }
      this.sessions.set(s.session_id, {
        ...known,
        project_id: s.project_id,
        title: s.title,
        state: s.state,
        queue_len: s.queue_len,
        unread: s.unread,
        last_seq: s.last_seq,
        last_read_seq: s.last_read_seq,
        // `archived` is the top state of SPEC §17.1, so the summary carries it.
        archived: s.state === 'archived',
        held_by: s.held_by,
        permission_mode: s.permission_mode,
        effective_permission_mode: s.effective_permission_mode,
        permission_capped_by: s.permission_capped_by,
        subagents: s.subagents,
        favorite_at: s.favorite_at,
      });
    }
    // A session created elsewhere: fetch the list again for its title.
    if (unknown) void this.refreshSessions().catch(() => {});
    this.changed();
  }

  setRequests(list: OpenRequest[]): void {
    this.requests = list;
    this.changed();
  }

  /** The open requests of the sessions `sessionId` waits on through its asks, along the
   *  chain (SPEC §10.1): shown and answered in its feed. */
  waitedRequests(sessionId: string): OpenRequest[] {
    return this.requests.filter((r) => r.waiting.some((w) => w.session_id === sessionId));
  }

  /** Answers an approval wherever it is shown: the request's own session gets the answer,
   *  as from its feed. Once the hub took it, it leaves every list at once; the hub's next
   *  `requests` says the same. */
  approveRequest(r: OpenRequest, optionId: string, message?: string): Promise<void> {
    return this.closing(r, this.api.approve(r.session_id, r.request_id, optionId, message));
  }

  answerRequest(r: OpenRequest, answers: Record<string, ClarifyAnswer>): Promise<void> {
    return this.closing(r, this.api.clarify(r.session_id, r.request_id, answers));
  }

  /** An answer taken, or the request closed elsewhere first (`409 request_closed`): it is
   *  gone either way. */
  private async closing(r: OpenRequest, answer: Promise<void>): Promise<void> {
    try {
      await answer;
    } catch (e) {
      if (!(e instanceof ApiError && e.code === 'request_closed')) throw e;
    }
    this.dropRequest(r);
  }

  private dropRequest(r: OpenRequest): void {
    const left = this.requests.filter((x) => x.session_id !== r.session_id || x.request_id !== r.request_id);
    if (left.length !== this.requests.length) this.setRequests(left);
  }

  /** The user's favorite projects first, then the others, each in the hub's order (the
   *  person's own order from Reorder, else the personal one first, it is the oldest). */
  sortedProjects(): Project[] {
    const all = [...this.projects.values()];
    return [...all.filter((p) => p.favorite_at), ...all.filter((p) => !p.favorite_at)];
  }

  /** Marks the project a favorite or not; another device of the user sees it after a reload. */
  async setProjectFavorite(id: string, on: boolean): Promise<void> {
    const favorite_at = await this.api.setProjectFavorite(id, on);
    const p = this.projects.get(id);
    if (p) this.projects.set(id, { ...p, favorite_at });
    this.changed();
  }

  /** The project bar in the order of `ids` at once, then as the hub keeps it. */
  async saveProjectOrder(ids: string[]): Promise<void> {
    const put = (list: Project[]) => {
      this.projects.clear();
      for (const p of list) this.projects.set(p.id, p);
      this.changed();
    };
    const known = ids.map((id) => this.projects.get(id)).filter((p): p is Project => !!p);
    put([...known, ...[...this.projects.values()].filter((p) => !ids.includes(p.id))]);
    put(await this.api.putProjectOrder(ids));
  }

  currentProject(): Project | undefined {
    return this.currentProjectId ? this.projects.get(this.currentProjectId) : undefined;
  }

  selectProject(id: string): void {
    if (this.currentProjectId === id) return;
    this.currentProjectId = id;
    this.archiveMode = false;
    this.changed();
  }

  setArchiveMode(on: boolean): void {
    if (this.archiveMode === on) return;
    this.archiveMode = on;
    this.changed();
  }

  /** Sessions of the project (the current one by default), newest activity first, without
   *  archived ones, the one waiting to be archived and the workers' service sessions
   *  (listed apart, SPEC §10.2). */
  sortedSessions(projectId = this.currentProjectId): SessionInfo[] {
    return [...this.sessions.values()]
      .filter((s) => !s.archived && !s.system && s.project_id === projectId && s.id !== this.pendingArchive?.id)
      .sort((a, b) => b.updated_at.localeCompare(a.updated_at) || b.last_seq - a.last_seq);
  }

  /** The user's favorite sessions of every project, the newest favorite first, without
   *  archived ones and the one waiting to be archived. */
  favoriteSessions(): SessionInfo[] {
    return [...this.sessions.values()]
      .filter((s) => s.favorite_at && !s.archived && s.id !== this.pendingArchive?.id)
      .sort((a, b) => Date.parse(b.favorite_at!) - Date.parse(a.favorite_at!));
  }

  /** Marks the session a favorite or not; the hub's `session.summary` says the same to the
   *  user's other clients. */
  async setSessionFavorite(id: string, on: boolean): Promise<void> {
    const favorite_at = await this.api.setSessionFavorite(id, on);
    const s = this.sessions.get(id);
    if (s) this.sessions.set(id, { ...s, favorite_at });
    this.changed();
  }

  /** Archived sessions of the project, the last archived first: archive mode (T50). */
  archivedSessions(projectId = this.currentProjectId): SessionInfo[] {
    const at = (s: SessionInfo) => s.archived_at ?? '';
    return [...this.sessions.values()]
      .filter((s) => s.archived && !s.system && s.project_id === projectId)
      .sort((a, b) => at(b).localeCompare(at(a)) || b.updated_at.localeCompare(a.updated_at));
  }

  /** The list's mode for the project (T50): `state` until the hub's record is loaded. */
  listMode(projectId = this.currentProjectId): ListMode {
    return (projectId && this.orders.get(projectId)?.mode) || 'state';
  }

  /** The project's sessions in the user's custom order, a new one on top. */
  orderedSessions(projectId = this.currentProjectId): SessionInfo[] {
    return arrange(this.sortedSessions(projectId), (projectId && this.orders.get(projectId)?.session_ids) || []);
  }

  /** Errors are dropped: the list then stays in `state`, the next look tries again. */
  async loadOrder(projectId: string): Promise<void> {
    try {
      this.orders.set(projectId, await this.api.sessionOrder(projectId));
      this.changed();
    } catch {
      // Offline: the default order.
    }
  }

  /** State / Recent / Custom: shown at once, stored on the hub. */
  setListMode(projectId: string, mode: ListMode): Promise<void> {
    return this.putOrder(projectId, { mode });
  }

  /** Reorder's "Done": the order, and Custom with it. */
  saveOrder(projectId: string, sessionIds: string[]): Promise<void> {
    return this.putOrder(projectId, { mode: 'custom', session_ids: sessionIds });
  }

  private async putOrder(projectId: string, body: { mode: ListMode; session_ids?: string[] }): Promise<void> {
    const before = this.orders.get(projectId);
    this.orders.set(projectId, { mode: body.mode, session_ids: body.session_ids ?? before?.session_ids ?? [] });
    this.changed();
    try {
      this.orders.set(projectId, await this.api.putSessionOrder(projectId, body));
    } catch (e) {
      if (before) this.orders.set(projectId, before);
      else this.orders.delete(projectId);
      this.listNotice = `Order not saved: ${(e as Error).message}`;
    }
    this.changed();
  }

  /** Archives from the list: hidden at once, sent when the Undo toast ends (T50). The one
   *  waiting before goes to the hub first. */
  archiveLater(s: SessionInfo): void {
    this.commitArchive();
    this.pendingArchive = { id: s.id, title: s.title || s.slug };
    this.archiveTimer = setTimeout(() => this.commitArchive(), ARCHIVE_UNDO_MS);
    this.changed();
  }

  /** Undo: nothing has reached the hub, the row is back in its place. */
  undoArchive(): void {
    clearTimeout(this.archiveTimer);
    if (!this.pendingArchive) return;
    this.pendingArchive = null;
    this.changed();
  }

  /** Sends the waiting archive now; `keepalive` while the page is being left. */
  commitArchive(keepalive = false): void {
    const p = this.pendingArchive;
    clearTimeout(this.archiveTimer);
    if (!p) return;
    this.pendingArchive = null;
    const s = this.sessions.get(p.id);
    // In the archive at once; the hub's object replaces this.
    if (s) this.sessions.set(p.id, { ...s, archived: true, state: 'archived', archived_at: new Date().toISOString() });
    this.changed();
    this.api.archive(p.id, keepalive).then(
      (v) => {
        this.sessions.set(v.id, v);
        this.changed();
      },
      (e: Error) => {
        if (s) this.sessions.set(p.id, s);
        this.listNotice = `Not archived «${p.title}»: ${e.message}`;
        this.changed();
      },
    );
  }

  /** Back from the archive (`409 no_workspace` once its workspace was removed). */
  async restoreSession(id: string): Promise<void> {
    const s = await this.api.unarchive(id);
    this.sessions.set(s.id, s);
    this.changed();
  }

  async deleteSession(id: string): Promise<void> {
    await this.api.deleteSession(id);
    this.removeSession(id);
  }

  async renameSession(id: string, title: string): Promise<void> {
    const s = await this.api.patchSession(id, { title });
    this.sessions.set(s.id, s);
    this.changed();
  }

  setListNotice(notice: string | null): void {
    this.listNotice = notice;
    this.changed();
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

  /** Whether one of the project's sessions has unread events, among the ones
   *  [`projectWaiting`] counts (no archived or service sessions). */
  projectUnread(projectId: string): boolean {
    for (const s of this.sessions.values()) {
      if (s.project_id === projectId && !s.archived && !s.system && s.unread > 0) return true;
    }
    return false;
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

  /** A session of the user by id or address (`<project-slug>/<session-slug>`, SPEC §7), with
   *  its address and title; undefined for a deleted or unknown one. */
  findSession(ref: string): { id: string; address: string; title: string } | undefined {
    const address = (s: SessionInfo) => {
      const p = this.projects.get(s.project_id);
      return p ? `${p.slug}/${s.slug}` : undefined;
    };
    const byId = this.sessions.get(ref);
    if (byId) {
      const a = address(byId);
      return a ? { id: byId.id, address: a, title: byId.title || byId.slug } : undefined;
    }
    for (const s of this.sessions.values())
      if (address(s) === ref) return { id: s.id, address: ref, title: s.title || s.slug };
    return undefined;
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

  /** Moves the session to folder `path` of its worker, in `project` or a new one (a new
   *  project joins the rail); its list follows it there. */
  async moveSession(sessionId: string, body: { path: string; project?: string; create?: boolean }): Promise<SessionInfo> {
    const s = await this.api.moveSession(sessionId, body);
    if (!this.projects.has(s.project_id)) {
      for (const p of await this.api.projects()) this.projects.set(p.id, p);
    }
    this.sessions.set(s.id, s);
    this.workspaces.delete(s.project_id);
    this.changed();
    return s;
  }

  /** Switches the session's permission mode (T38); the hub tells a running agent at once. */
  async setPermissionMode(sessionId: string, mode: PermissionMode): Promise<void> {
    const s = await this.api.patchSession(sessionId, { permission_mode: mode });
    this.sessions.set(s.id, s);
    this.changed();
  }

  /** Changes the session's model (`null`: the runtime's default). A running agent switches
   *  to a named model at once; `null` takes effect at its next start. */
  async setAgentSettings(sessionId: string, patch: Pick<SessionPatch, 'model'>): Promise<void> {
    const s = await this.api.patchSession(sessionId, patch);
    this.sessions.set(s.id, s);
    this.changed();
  }

  async createProject(name: string): Promise<Project> {
    const p = await this.api.createProject(name);
    this.projects.set(p.id, p);
    this.currentProjectId = p.id;
    this.changed();
    return p;
  }

  /** The app theme (T49): the user's, `warm` until `/api/me` answers. */
  theme(): Theme {
    return this.me?.theme ?? 'warm';
  }

  async setTheme(theme: Theme): Promise<void> {
    if (theme === this.theme()) return;
    this.me = await this.api.setTheme(theme);
    this.changed();
  }

  /** A project's color and emoji or letters (T49); the personal project's too. */
  async setAppearance(id: string, patch: { color?: ProjectColor; icon?: { kind: 'emoji' | 'letters'; value: string } }): Promise<void> {
    const p = await this.api.patchProject(id, patch);
    this.projects.set(p.id, p);
    this.changed();
  }

  /** An image as the project's icon; the hub checks it (`422 invalid_icon`). */
  async uploadProjectIcon(id: string, file: File): Promise<void> {
    const p = await this.api.uploadProjectIcon(id, file, file.name);
    this.projects.set(p.id, p);
    this.changed();
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
