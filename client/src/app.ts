// Application-wide state: who I am, my projects, the sessions of all of them and their live
// `state`/`queue_len`/unread from `subscribe_summary` (SPEC §6, §17.1), the workspaces and
// the list order (T50) of the projects that have been looked at, a session archived from the
// list that has not gone to the hub yet, and the archived projects the project search finds.

import { Api, ApiError, type NewSessionOptions, type SessionPatch } from './api';
import type {
  ClarifyAnswer,
  ListMode,
  LookName,
  Me,
  OpenRequest,
  PermissionMode,
  Project,
  ProjectColor,
  ProjectSort,
  SessionInfo,
  SessionOrder,
  SessionSummary,
  Theme,
  Workspace,
} from './proto';
import { PROJECT_SORT_NAMES } from './proto';
import { ChatSession, type OutgoingFile } from './session';
import { storedLook, storedTheme, type DeviceLayout } from './theme';
import { projectName } from './ui/Appearance';
import { arrange, bucketOf } from './ui/sessions';
import { HubSocket, hubSocketUrl, type ConnState } from './ws';

/** How long "Archived «…» · Undo" stays before the archive goes to the hub (T50). */
export const ARCHIVE_UNDO_MS = 5000;

/** A new session not on the hub yet (`#/p/<project>/new`): the project and what its chips
 *  chose; absent fields are the defaults (the project's default workspace, the worker's
 *  permission mode, the runtime's model). `started`: the session its first message created,
 *  which the list leaves out while the draft still stands for it. */
export interface NewSessionDraft {
  projectId: string;
  workspace_id?: string;
  cwd: string;
  permission_mode?: PermissionMode;
  model?: string;
  started?: string;
}

/** What a phone's first screen lists: the projects, or every project's sessions by state. */
export type PhoneView = 'projects' | 'sessions';

/** Chats kept in memory, the most recently used: one of them opens again at once (SPEC §6). */
const CHATS = 12;

/** Unsent messages by session id, on this device: a convenience, so storage that fails or is
 *  empty only loses them on a reload. */
const DRAFTS_KEY = 'hqc.drafts';
function loadDrafts(): Map<string, string> {
  try {
    const v = JSON.parse(localStorage.getItem(DRAFTS_KEY) ?? '{}');
    return new Map(Object.entries(v ?? {}).filter((e): e is [string, string] => typeof e[1] === 'string'));
  } catch {
    return new Map();
  }
}

/** What this device keeps whatever the look (`DeviceLayout`): a convenience, so storage that
 *  fails or is empty only means the default width. */
const DEVICE_KEY = 'hqc.look';
function loadDevice(): DeviceLayout {
  try {
    const v = JSON.parse(localStorage.getItem(DEVICE_KEY) ?? '{}') ?? {};
    return { listWidth: typeof v.listWidth === 'number' ? v.listWidth : undefined };
  } catch {
    return {};
  }
}

/** The project bar's order last shown on this device: storage that fails or holds something
 *  else means Custom. */
const PROJECT_SORT_KEY = 'hqc.projectSort';
function storedProjectSort(): ProjectSort {
  try {
    const v = localStorage.getItem(PROJECT_SORT_KEY);
    return (PROJECT_SORT_NAMES as readonly string[]).includes(v ?? '') ? (v as ProjectSort) : 'custom';
  } catch {
    return 'custom';
  }
}

/** The projects, sessions and list orders as last shown, on this device: shown at once on a
 *  reload, replaced when the hub answers. Storage that fails or holds something else: none. */
const LIST_KEY = 'hqc.list';
function loadList(): {
  home: string;
  projects: Project[];
  archived: Project[];
  sessions: SessionInfo[];
  orders: [string, SessionOrder][];
} | null {
  try {
    const v = JSON.parse(localStorage.getItem(LIST_KEY) ?? '');
    return {
      home: v.home,
      projects: [...v.projects],
      archived: [...(v.archived ?? [])],
      sessions: [...v.sessions],
      orders: Object.entries(v.orders),
    };
  } catch {
    return null;
  }
}

export class AppState {
  me: Me | null = null;
  /** The session column's dragged width, on this device; the look itself is the user's
   *  (`lookName`). */
  device: DeviceLayout = loadDevice();
  error: string | null = null;
  readonly projects = new Map<string, Project>();
  readonly sessions = new Map<string, SessionInfo>();
  /** By project id; filled by `loadWorkspaces`. */
  readonly workspaces = new Map<string, Workspace[]>();
  /** The project shown in the left column; the personal one until another is picked. */
  currentProjectId: string | null = null;
  conn: ConnState = 'closed';
  /** The socket's first connect has ended, open or closed: "offline" is not shown before. */
  connTried = false;
  /** Every open request of the user, oldest first, as the hub last sent it (T37). */
  requests: OpenRequest[] = [];
  /** By project id: the user's list order (T50), as the hub last gave it; `loadOrder`. */
  readonly orders = new Map<string, SessionOrder>();
  /** Projects whose order came from the hub on this page load, not only from storage. */
  private readonly loadedOrders = new Set<string>();
  /** Archived from the list, not sent yet: hidden at once, sent when the Undo toast ends,
   *  when another one is archived, before Reorder or archive mode, or when the page is left
   *  (T50). */
  pendingArchive: { id: string; title: string } | null = null;
  private archiveTimer: ReturnType<typeof setTimeout> | undefined;
  /** The list shows the project's archive (T50); back to the sessions on a project switch. */
  archiveMode = false;
  /** The archived projects, off the project bar: as `refreshSessions` last fetched them, with
   *  the ones archived or restored here since. The project search finds them; one opened from
   *  there, from an address or from a session of it is shown like any. */
  readonly archivedProjects = new Map<string, Project>();
  /** Projects archived here whose `/archive` is not answered yet, and ones being restored: a
   *  list read meanwhile may predate the hub's change, so it does not move them
   *  (`putProjects`, `placeProject`). */
  private readonly archiving = new Set<string>();
  private readonly restoring = new Set<string>();
  /** The bar's order in the hub's latest list (on page load, the bar kept on this device until
   *  the first list comes), with the projects this client put on the bar since last, as the hub
   *  orders a new one: a refused archive goes back to its place in it, whatever was archived,
   *  restored or reordered while the refusal was on its way. */
  private hubBar: string[] = [];
  /** A project archived here a moment ago: "Archived «…» · Undo" for `ARCHIVE_UNDO_MS`. */
  projectUndo: string | null = null;
  private projectUndoTimer: ReturnType<typeof setTimeout> | undefined;
  /** The last archive or restore of a project on its way: the next one waits for it, so an
   *  Undo never reaches the hub before the archive it undoes. */
  private projectArchiving: Promise<unknown> = Promise.resolve();
  /** A list action the hub refused (archive, restore, delete, rename, order). */
  listNotice: string | null = null;
  /** The new session open in the main column, one at a time; gone when it is left unsent. */
  newSession: NewSessionDraft | null = null;
  /** The view a phone's first screen shows, picked from its title's menu; kept while the page
   *  is open (a reload is back to Projects), not stored. */
  phoneView: PhoneView = 'projects';
  /** What a new session's window hands to the chat that replaces it, taken once
   *  (`takeHandover`). */
  private handover: { id: string; files: OutgoingFile[] } | null = null;
  private readonly listeners = new Set<() => void>();
  private readonly drafts = loadDrafts();
  private refreshing: Promise<void> | null = null;
  /** The sessions the hub pushed a summary of while the lists were on their way. */
  private pushedWhileRefreshing = new Set<string>();
  /** Counts the user's changes to projects and sessions (`edited`), for `refreshSessions`. */
  private edits = 0;
  /** By session id, the least recently used first. */
  private readonly chats = new Map<string, ChatSession>();

  constructor(
    readonly api: Api,
    readonly socket: HubSocket,
  ) {
    socket.onState((s) => {
      this.conn = s;
      if (s !== 'connecting') this.connTried = true;
      this.changed();
    });
    const list = loadList();
    if (list) {
      for (const p of list.projects) this.projects.set(p.id, p);
      this.hubBar = list.projects.map((p) => p.id);
      for (const p of list.archived) this.archivedProjects.set(p.id, p);
      for (const s of list.sessions) this.sessions.set(s.id, s);
      for (const [id, o] of list.orders) this.orders.set(id, o);
      this.currentProjectId = list.home;
    }
  }

  /** The socket is down: not while its first connect at page load is still pending. */
  get offline(): boolean {
    return this.connTried && this.conn !== 'open';
  }

  onChange(fn: () => void): () => void {
    this.listeners.add(fn);
    return () => this.listeners.delete(fn);
  }

  /** The message typed into session `id` and not sent yet; `''` when none. */
  draft(id: string): string {
    return this.drafts.get(id) ?? '';
  }

  /** Keeps what is typed into session `id` until it is sent; the list learns only when the
   *  session gets or loses a draft. */
  setDraft(id: string, text: string): void {
    const had = this.drafts.has(id);
    if (text.trim() === '') this.drafts.delete(id);
    else this.drafts.set(id, text);
    try {
      localStorage.setItem(DRAFTS_KEY, JSON.stringify(Object.fromEntries(this.drafts)));
    } catch {
      // Not kept past a reload.
    }
    if (had !== this.drafts.has(id)) this.changed();
  }

  setDevice(patch: Partial<DeviceLayout>): void {
    this.device = { ...this.device, ...patch };
    try {
      localStorage.setItem(DEVICE_KEY, JSON.stringify(this.device));
    } catch {
      // Kept until a reload.
    }
    this.changed();
  }

  private changed(): void {
    for (const fn of this.listeners) fn();
  }

  /** `changed` after the user changed a project or a session, from the hub's answer or as a
   *  local echo. */
  private edited(): void {
    this.edits++;
    this.changed();
  }

  /** The chat of session `id`, kept with what it has loaded for the `CHATS` most recently
   *  used; past that the least recently used one that is not open is dropped. */
  chat(id: string): ChatSession {
    let c = this.chats.get(id);
    if (c) {
      this.chats.delete(id);
    } else {
      c = new ChatSession(this.api, this.socket, id);
      // A typed `/model` switches the session's model on the hub, which the summary does not
      // carry: the session object is fetched again.
      c.onStored = (text) => {
        if (/^\/model\s+\S/.test(text.trim())) void this.refreshSession(id);
      };
    }
    this.chats.set(id, c);
    for (const [old, o] of this.chats) {
      if (this.chats.size <= CHATS) break;
      if (!o.isOpen) this.chats.delete(old);
    }
    return c;
  }

  /** Loads the newest page of session `id` before it is opened: no subscription, and the
   *  read cursor stays where it is. */
  prefetchChat(id: string): void {
    // A failed load is tried again when the chat opens.
    this.chat(id).load().catch(() => {});
  }

  async init(): Promise<void> {
    try {
      this.me = await this.api.me();
      this.keepProjectSort(this.me.project_sort);
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
   *  at once; `keepalive` lets the request outlive the page (T50). The list is kept for the
   *  next load then too. */
  watchPageLeave(win: Window): void {
    win.addEventListener('pagehide', () => {
      this.commitArchive(true);
      this.rememberList();
    });
    win.document.addEventListener('visibilitychange', () => {
      if (win.document.visibilityState !== 'hidden') return;
      this.commitArchive(true);
      this.rememberList();
    });
  }

  /** Keeps the list for the next load of the page; one that cannot be kept is forgotten. */
  private rememberList(): void {
    if (!this.me) return;
    try {
      localStorage.setItem(
        LIST_KEY,
        JSON.stringify({
          home: this.me.personal_project_id,
          projects: [...this.projects.values()],
          archived: [...this.archivedProjects.values()],
          sessions: [...this.sessions.values()],
          // Only the hub's orders of this page load: a kept one is not written back unchecked.
          orders: Object.fromEntries([...this.orders].filter(([id]) => this.loadedOrders.has(id))),
        }),
      );
    } catch {
      try {
        localStorage.removeItem(LIST_KEY);
      } catch {
        // No storage: nothing is remembered.
      }
    }
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

  /** Projects (on the bar and archived) and the sessions of every project, in three requests;
   *  the list is kept for the next load of the page. The archived projects are kept as they
   *  were when their request fails. A change the user made while the lists were on their way
   *  is newer than them: they are fetched again, else they would undo it (a project's change
   *  has no `session.summary` to bring it back). */
  async refreshSessions(): Promise<void> {
    if (!this.me) return;
    this.refreshing ??= (async () => {
      this.pushedWhileRefreshing.clear();
      let seen: number;
      let projects: Project[];
      let archived: Project[] | null;
      let sessions: SessionInfo[];
      do {
        seen = this.edits;
        [projects, archived, sessions] = await Promise.all([
          this.api.projects(),
          this.api.archivedProjects().catch(() => null),
          this.api.allSessions(),
        ]);
      } while (seen !== this.edits);
      this.putProjects(projects, archived);
      const fresh = sessions.map((s) => this.keepRead(s));
      this.sessions.clear();
      for (const s of fresh) this.sessions.set(s.id, s);
      // Gone: the personal project. An archived one stays open.
      if (!this.currentProject()) this.currentProjectId = this.me!.personal_project_id;
      // A summary pushed meanwhile may be newer than the lists or older: the session itself,
      // asked now, is newer than both ("closed in VS Code" would otherwise come back open).
      for (const id of this.pushedWhileRefreshing) void this.refreshSession(id);
      this.rememberList();
      this.changed();
    })().finally(() => (this.refreshing = null));
    return this.refreshing;
  }

  /** `refreshSessions` after the user changed on the hub what only the lists bring back (a
   *  takeover, a revoked or deleted worker): a refresh on its way fetches them again. */
  reloadSessions(): Promise<void> {
    this.edits++;
    return this.refreshSessions();
  }

  /** One session from the hub again: its `commands` (T27) are not in the summary. Errors are
   *  dropped; the next refresh brings it. */
  async refreshSession(id: string): Promise<void> {
    try {
      this.sessions.set(id, this.keepRead(await this.api.session(id)));
      this.changed();
    } catch {
      // Offline or gone: the list and the summary still have the session.
    }
  }

  mergeSummary(list: SessionSummary[]): void {
    let unknown = false;
    for (const s of list) {
      if (this.refreshing) this.pushedWhileRefreshing.add(s.session_id);
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
        if (this.project(s.project_id)) void this.refreshSession(s.session_id);
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
        background_tasks: s.background_tasks,
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

  /** The user's favorite projects first, then the others, each in the bar's order (`sort`, the
   *  user's by default, SPEC §3 User): Custom — the hub's (the person's own order from Reorder,
   *  else the personal one first, it is the oldest); Name — by the shown name; Recent — the
   *  newest activity of the project's sessions as the session list counts them
   *  (`sortedSessions`) first, the projects without one after them, the newest project first. */
  sortedProjects(sort: ProjectSort = this.projectSort()): Project[] {
    const all = [...this.projects.values()];
    if (sort === 'name') {
      all.sort((a, b) => projectName(a).localeCompare(projectName(b), undefined, { numeric: true, sensitivity: 'accent' }));
    } else if (sort === 'recent') {
      const last = new Map<string, string>();
      for (const s of this.sessions.values()) {
        const t = last.get(s.project_id);
        if (this.listed(s) && (t === undefined || s.updated_at.localeCompare(t) > 0)) last.set(s.project_id, s.updated_at);
      }
      all.sort((a, b) => {
        // '' (no session) goes after any time.
        const x = last.get(a.id) ?? '';
        const y = last.get(b.id) ?? '';
        return y.localeCompare(x) || (x || y ? 0 : b.created_at.localeCompare(a.created_at));
      });
    }
    return [...all.filter((p) => p.favorite_at), ...all.filter((p) => !p.favorite_at)];
  }

  /** The project bar's order (SPEC §3 User): the user's; until `/api/me` answers, the one
   *  last shown on this device. */
  projectSort(): ProjectSort {
    return this.me?.project_sort ?? this.projectSortCopy;
  }

  /** Applied at once, so the bar does not show the old order until the hub answers (Reorder's
   *  Done would jump back for a moment); what it was comes back if the hub refuses. */
  async setProjectSort(sort: ProjectSort): Promise<void> {
    const before = this.me?.project_sort;
    if (!this.me || before === undefined || sort === before) return;
    this.me = { ...this.me, project_sort: sort };
    this.keepProjectSort(sort);
    this.changed();
    try {
      this.me = await this.api.setProjectSort(sort);
    } catch (e) {
      if (this.me) this.me = { ...this.me, project_sort: before };
      this.keepProjectSort(before);
      throw e;
    } finally {
      this.changed();
    }
  }

  /** A copy of the bar's order on this device, for the first paint after a reload. */
  private projectSortCopy: ProjectSort = storedProjectSort();
  private keepProjectSort(sort: ProjectSort): void {
    this.projectSortCopy = sort;
    try {
      localStorage.setItem(PROJECT_SORT_KEY, sort);
    } catch {
      // Not kept: the next load starts Custom until `/api/me` answers.
    }
  }

  /** Marks the project a favorite or not; another device of the user sees it after a reload. */
  async setProjectFavorite(id: string, on: boolean): Promise<void> {
    const favorite_at = await this.api.setProjectFavorite(id, on);
    for (const list of [this.projects, this.archivedProjects]) {
      const p = list.get(id);
      if (p) list.set(id, { ...p, favorite_at });
    }
    this.edited();
  }

  /** A project of the user, on the bar or archived. */
  project(id: string): Project | undefined {
    return this.projects.get(id) ?? this.archivedProjects.get(id);
  }

  /** The lists as the hub sent them: `live` (the bar, in its order) and, when fetched, the
   *  archived ones. A project whose archive or restore is on its way stays where this client
   *  put it: the list may have been read before the hub stored the change. */
  private putProjects(live: Project[], archived?: Project[] | null, local = false): void {
    if (!local) this.hubBar = live.map((p) => p.id);
    const held = new Map<string, Project>();
    for (const id of [...this.archiving, ...this.restoring]) {
      const p = this.archivedProjects.get(id);
      if (p) held.set(id, p);
    }
    this.projects.clear();
    for (const p of live) if (!held.has(p.id)) this.projects.set(p.id, p);
    if (archived) {
      this.archivedProjects.clear();
      for (const p of archived) if (!this.projects.has(p.id)) this.archivedProjects.set(p.id, p);
    } else {
      for (const id of this.projects.keys()) this.archivedProjects.delete(id);
    }
    for (const [id, p] of held) this.archivedProjects.set(id, p);
  }

  /** A project as the hub answered it, into the list its `archived_at` says; one whose
   *  archive or restore is on its way stays in the list it is in. */
  private placeProject(p: Project): void {
    const held = this.archiving.has(p.id) || this.restoring.has(p.id);
    const before = this.archivedProjects.get(p.id);
    if (held ? !!before : !!p.archived_at) {
      this.projects.delete(p.id);
      this.archivedProjects.set(p.id, { ...p, archived_at: p.archived_at ?? before?.archived_at });
    } else {
      this.archivedProjects.delete(p.id);
      this.projects.set(p.id, p.archived_at ? { ...p, archived_at: undefined } : p);
      if (!this.hubBar.includes(p.id)) this.hubBar.push(p.id);
    }
  }

  /** Archives a project at once, no question: off the bar, the open one gives its place to the
   *  personal project, "Archived «…» · Undo" for `ARCHIVE_UNDO_MS`. Its sessions stay as they
   *  are. A refusal of the hub puts it back on the bar. */
  archiveProject(id: string): void {
    const p = this.projects.get(id);
    if (!p) return;
    // The bar now: the place to go back to when the hub's latest list does not have it.
    const order = [...this.projects.keys()];
    this.archiving.add(id);
    this.projects.delete(id);
    this.archivedProjects.set(id, { ...p, archived_at: new Date().toISOString() });
    if (this.currentProjectId === id) this.currentProjectId = this.me?.personal_project_id ?? null;
    clearTimeout(this.projectUndoTimer);
    this.projectUndo = id;
    this.projectUndoTimer = setTimeout(() => {
      this.projectUndo = null;
      this.changed();
    }, ARCHIVE_UNDO_MS);
    this.changed();
    this.projectArchiving = this.projectArchiving.then(() =>
      this.api.archiveProject(id).then(
        (v) => {
          this.archiving.delete(id);
          if (this.archivedProjects.has(id)) {
            this.projects.delete(id);
            this.archivedProjects.set(id, v);
          }
          // A list on its way was read before the hub stored it: it is read again.
          this.edited();
        },
        (e: Error) => {
          this.archiving.delete(id);
          // Its copy now (renamed or made a favorite from the search meanwhile), back on the bar
          // in the hub's order, which still has it (before the first list, in the bar's order
          // when it was archived); one neither order has goes last.
          const back = { ...(this.archivedProjects.get(id) ?? p), archived_at: undefined };
          this.archivedProjects.delete(id);
          const now = new Map(this.projects).set(id, back);
          this.projects.clear();
          for (const k of this.hubBar.includes(id) ? this.hubBar : order) {
            const q = now.get(k);
            if (q) this.projects.set(k, q);
          }
          for (const [k, q] of now) if (!this.projects.has(k)) this.projects.set(k, q);
          if (this.projectUndo === id) this.projectUndo = null;
          this.listNotice = `Not archived «${p.name}»: ${e.message}`;
          this.changed();
        },
      ),
    );
  }

  /** Back from the archive (Undo, or the menu of a project the search found), in its place on
   *  the bar as the hub keeps the order. */
  restoreProject(id: string): Promise<void> {
    if (this.projectUndo === id) {
      clearTimeout(this.projectUndoTimer);
      this.projectUndo = null;
      this.changed();
    }
    this.restoring.add(id);
    const run = this.projectArchiving.then(async () => {
      try {
        await this.api.unarchiveProject(id);
        const list = await this.api.projects();
        this.restoring.delete(id);
        this.archivedProjects.delete(id);
        // Another one archived meanwhile is still on its way: it stays off the bar. A list on
        // its way was read before the hub restored it: it is read again.
        this.putProjects(list);
        this.edited();
      } finally {
        this.restoring.delete(id);
      }
    });
    this.projectArchiving = run.catch(() => {});
    return run;
  }

  /** The project bar in the order of `ids` at once, then as the hub keeps it. */
  async saveProjectOrder(ids: string[]): Promise<void> {
    const put = (list: Project[], local = false) => {
      this.putProjects(list, undefined, local);
      this.edited();
    };
    const known = ids.map((id) => this.projects.get(id)).filter((p): p is Project => !!p);
    put([...known, ...[...this.projects.values()].filter((p) => !ids.includes(p.id))], true);
    put(await this.api.putProjectOrder(ids));
  }

  /** The project of the left column: one on the bar, or an archived one. */
  currentProject(): Project | undefined {
    const id = this.currentProjectId;
    return id ? this.project(id) : undefined;
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

  setPhoneView(view: PhoneView): void {
    if (this.phoneView === view) return;
    this.phoneView = view;
    this.changed();
  }

  /** Sessions of the project (the current one by default), newest activity first, without
   *  archived ones, the one waiting to be archived and the workers' service sessions
   *  (listed apart, SPEC §10.2). */
  sortedSessions(projectId = this.currentProjectId): SessionInfo[] {
    return [...this.sessions.values()]
      .filter((s) => s.project_id === projectId && this.listed(s))
      .sort((a, b) => b.updated_at.localeCompare(a.updated_at) || b.last_seq - a.last_seq);
  }

  /** A session the list shows: not archived, not waiting to be archived, not a service one. */
  private listed(s: SessionInfo): boolean {
    return !s.archived && !s.system && s.id !== this.pendingArchive?.id;
  }

  /** The user's favorite sessions of the project (the current one by default), the newest
   *  favorite first, without archived ones and the one waiting to be archived. */
  favoriteSessions(projectId = this.currentProjectId): SessionInfo[] {
    return [...this.sessions.values()]
      .filter((s) => s.favorite_at && !s.archived && s.project_id === projectId && s.id !== this.pendingArchive?.id)
      .sort((a, b) => Date.parse(b.favorite_at!) - Date.parse(a.favorite_at!));
  }

  /** Marks the session a favorite or not; the hub's `session.summary` says the same to the
   *  user's other clients. */
  async setSessionFavorite(id: string, on: boolean): Promise<void> {
    const favorite_at = await this.api.setSessionFavorite(id, on);
    const s = this.sessions.get(id);
    if (s) this.sessions.set(id, { ...s, favorite_at });
    this.edited();
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

  /** The project's order came from the hub on this page load: a remembered one is asked again. */
  orderLoaded(projectId: string): boolean {
    return this.loadedOrders.has(projectId);
  }

  /** Errors are dropped: the list then stays in `state`, the next look tries again. */
  async loadOrder(projectId: string): Promise<void> {
    try {
      this.orders.set(projectId, await this.api.sessionOrder(projectId));
      this.loadedOrders.add(projectId);
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
      this.loadedOrders.add(projectId);
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
    this.edited();
    this.api.archive(p.id, keepalive).then(
      (v) => {
        this.sessions.set(v.id, v);
        this.edited();
      },
      (e: Error) => {
        if (s) this.sessions.set(p.id, s);
        this.listNotice = `Not archived «${p.title}»: ${e.message}`;
        this.edited();
      },
    );
  }

  /** Back from the archive (`409 no_workspace` once its workspace was removed). */
  async restoreSession(id: string): Promise<void> {
    const s = await this.api.unarchive(id);
    this.sessions.set(s.id, s);
    this.edited();
  }

  async deleteSession(id: string): Promise<void> {
    await this.api.deleteSession(id);
    this.removeSession(id);
  }

  async renameSession(id: string, title: string): Promise<void> {
    const s = await this.api.patchSession(id, { title });
    this.sessions.set(s.id, s);
    this.edited();
  }

  setListNotice(notice: string | null): void {
    this.listNotice = notice;
    this.changed();
  }

  /** Forgets a session the hub has deleted. */
  removeSession(id: string): void {
    if (this.sessions.delete(id)) this.edited();
  }

  /** The workers' service sessions (SPEC §10.2), by worker name: the "Workers" list (above the
   *  workers line at the bottom of a desktop column and of a phone's first screen).
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

  /** Whether one of the project's sessions works: the same rule as a session row's pulsing
   *  dot ([`bucketOf`] "run"), among the ones [`projectWaiting`] counts (no archived or
   *  service sessions). */
  projectRunning(projectId: string): boolean {
    for (const s of this.sessions.values()) {
      if (s.project_id === projectId && !s.archived && !s.system && bucketOf(s) === 'run') return true;
    }
    return false;
  }

  /** A worker's name: the title of its service session (SPEC §10.2). */
  workerName(workerId: string | undefined): string | undefined {
    if (!workerId) return undefined;
    for (const s of this.sessions.values()) if (s.system && s.worker_id === workerId) return s.title;
    return undefined;
  }

  /** Sessions on the worker that work, by the same rule as the list's Running group
   *  ([`bucketOf`] "run"). It counts the worker's sessions in every project, favorites too, so
   *  it can be more than the open project's Running group shows. */
  workerRunning(workerId: string | undefined): number {
    let n = 0;
    for (const s of this.sessions.values()) {
      if (s.system || s.archived || s.worker_id !== workerId) continue;
      if (bucketOf(s) === 'run') n++;
    }
    return n;
  }

  /** A session of the user by id or address (`<project-slug>/<session-slug>`, SPEC §7), with
   *  its address and title; undefined for a deleted or unknown one. */
  findSession(ref: string): { id: string; address: string; title: string } | undefined {
    const address = (s: SessionInfo) => {
      const p = this.project(s.project_id);
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
  sessionPath(s: Pick<SessionInfo, 'project_id' | 'workspace_id' | 'cwd'>): string | undefined {
    const w = this.workspaces.get(s.project_id)?.find((x) => x.id === s.workspace_id);
    if (!w) return undefined;
    const cwd = s.cwd.replace(/^\.\/?/, '').replace(/\/+$/, '');
    return cwd ? `${w.path.replace(/\/+$/, '')}/${cwd}` : w.path;
  }

  async createSession(title?: string, opts: NewSessionOptions = {}, projectId = this.currentProjectId): Promise<SessionInfo> {
    if (!this.me || !projectId) throw new Error('not signed in');
    const s = await this.api.createSession(projectId, title, opts);
    this.sessions.set(s.id, s);
    this.edited();
    return s;
  }

  /** Opens a new session of project `projectId` in the main column: nothing on the hub yet,
   *  the defaults chosen. Another draft is dropped. */
  openNewSession(projectId: string): void {
    this.newSession = { projectId, cwd: '' };
    this.changed();
  }

  /** What a chip of the open new session chose. */
  setNewSession(patch: Partial<Omit<NewSessionDraft, 'projectId' | 'started'>>): void {
    if (!this.newSession) return;
    this.newSession = { ...this.newSession, ...patch };
    this.changed();
  }

  /** The new session was left: unsent, it is gone; sent, its session stands in the list. */
  closeNewSession(): void {
    if (!this.newSession) return;
    this.newSession = null;
    this.changed();
  }

  /** A new session's window gives its place to the chat of session `id`: what is typed there
   *  and not sent becomes the chat's draft, and the chat takes `files` and the caret. */
  handOver(id: string, text: string, files: OutgoingFile[]): void {
    this.setDraft(id, text);
    this.handover = { id, files };
  }

  /** What was handed over to chat `id`, once; nothing for another chat. */
  takeHandover(id: string): { files: OutgoingFile[] } | undefined {
    const h = this.handover;
    if (h?.id !== id) return undefined;
    this.handover = null;
    return h;
  }

  /** The workspace a new session starts in: the one its chip picked, else the project's
   *  default (SPEC §13.4), else the project's first. */
  newSessionWorkspace(d: NewSessionDraft): string | undefined {
    return d.workspace_id || this.project(d.projectId)?.default_workspace_id || this.workspaces.get(d.projectId)?.[0]?.id;
  }

  /** The first message of the open new session: the session is created on the hub with the
   *  draft's choices, and the draft stands for it in the list until its chat replaces the
   *  draft. */
  async startNewSession(): Promise<SessionInfo> {
    const d = this.newSession;
    if (!d) throw new Error('no new session open');
    if (!this.me) throw new Error('not signed in');
    const s = await this.api.createSession(d.projectId, undefined, {
      workspace_id: this.newSessionWorkspace(d),
      cwd: d.cwd.trim(),
      permission_mode: d.permission_mode,
      model: d.model,
    });
    this.sessions.set(s.id, s);
    // In the same change: the list never shows both the draft and its session.
    if (this.newSession === d) this.newSession = { ...d, started: s.id };
    this.edited();
    return s;
  }

  /** Moves the session to folder `path` of its worker, in `project` or a new one (a new
   *  project joins the rail); its list follows it there. */
  async moveSession(sessionId: string, body: { path: string; project?: string; create?: boolean }): Promise<SessionInfo> {
    const s = await this.api.moveSession(sessionId, body);
    if (!this.project(s.project_id)) {
      for (const p of await this.api.projects()) this.placeProject(p);
    }
    this.sessions.set(s.id, s);
    this.workspaces.delete(s.project_id);
    this.edited();
    return s;
  }

  /** Forks the session before the user's message `eventId`: the new session joins the list
   *  with `text`, the message left out, as its draft. */
  async forkSession(sessionId: string, eventId: string, text: string): Promise<SessionInfo> {
    const s = await this.api.forkSession(sessionId, eventId);
    this.sessions.set(s.id, s);
    this.setDraft(s.id, text);
    this.edited();
    return s;
  }

  /** Switches the session's permission mode (T38); the hub tells a running agent at once. */
  async setPermissionMode(sessionId: string, mode: PermissionMode): Promise<void> {
    const s = await this.api.patchSession(sessionId, { permission_mode: mode });
    this.sessions.set(s.id, s);
    this.edited();
  }

  /** Changes the session's model (`null`: the runtime's default). A running agent switches
   *  to a named model at once; `null` takes effect at its next start. */
  async setAgentSettings(sessionId: string, patch: Pick<SessionPatch, 'model'>): Promise<void> {
    const s = await this.api.patchSession(sessionId, patch);
    this.sessions.set(s.id, s);
    this.edited();
  }

  async createProject(name: string): Promise<Project> {
    const p = await this.api.createProject(name);
    this.projects.set(p.id, p);
    this.hubBar.push(p.id);
    this.currentProjectId = p.id;
    this.edited();
    return p;
  }

  /** The app theme (T49): the user's; until `/api/me` answers, the one last applied on this
   *  device (Warm without one), so App does not repaint a light theme dark at start. */
  theme(): Theme {
    return this.me?.theme ?? storedTheme();
  }

  async setTheme(theme: Theme): Promise<void> {
    if (theme === this.theme()) return;
    this.me = await this.api.setTheme(theme);
    this.changed();
  }

  /** The look, Compact or Spacious (SPEC §3 User): the user's; until `/api/me` answers, the one
   *  last applied on this device (Spacious without one), so the first paint is not the wrong
   *  look. */
  lookName(): LookName {
    return this.me?.look ?? storedLook();
  }

  async setLookName(look: LookName): Promise<void> {
    if (look === this.lookName()) return;
    this.me = await this.api.setLook(look);
    this.changed();
  }

  /** A project's color and emoji or letters (T49); the personal project's too. */
  async setAppearance(id: string, patch: { color?: ProjectColor; icon?: { kind: 'emoji' | 'letters'; value: string } }): Promise<void> {
    const p = await this.api.patchProject(id, patch);
    this.placeProject(p);
    this.edited();
  }

  /** An image as the project's icon; the hub checks it (`422 invalid_icon`). */
  async uploadProjectIcon(id: string, file: File): Promise<void> {
    const p = await this.api.uploadProjectIcon(id, file, file.name);
    this.placeProject(p);
    this.edited();
  }

  async renameProject(id: string, name: string): Promise<void> {
    const p = await this.api.patchProject(id, { name });
    this.placeProject(p);
    this.edited();
  }

  async deleteProject(id: string): Promise<void> {
    await this.api.deleteProject(id);
    this.projects.delete(id);
    this.archivedProjects.delete(id);
    this.workspaces.delete(id);
    for (const [sid, s] of this.sessions) if (s.project_id === id) this.sessions.delete(sid);
    if (this.currentProjectId === id) this.currentProjectId = this.me?.personal_project_id ?? null;
    this.edited();
  }

  async loadWorkspaces(projectId: string): Promise<Workspace[]> {
    const list = await this.api.workspaces(projectId);
    this.workspaces.set(projectId, list);
    this.changed();
    return list;
  }

  /** After a workspace was added or removed: the project's default may have changed (an
   *  archived project's too). */
  async workspacesChanged(projectId: string): Promise<void> {
    const archived = this.archivedProjects.has(projectId);
    const [list, projects] = await Promise.all([
      this.api.workspaces(projectId),
      archived ? this.api.archivedProjects() : this.api.projects(),
    ]);
    this.workspaces.set(projectId, list);
    for (const p of projects) this.placeProject(p);
    this.edited();
  }

  async setDefaultWorkspace(projectId: string, workspaceId: string): Promise<void> {
    const p = await this.api.patchProject(projectId, { default_workspace_id: workspaceId });
    this.placeProject(p);
    this.edited();
  }

  /** Local echo of the read cursor up to `seq`, so the badge clears before the summary
   *  arrives; the cursor is kept for `keepRead`. Not `edited`: it runs on every event of an
   *  open chat, a refresh would start over and over. */
  markRead(sessionId: string, seq: number): void {
    const s = this.sessions.get(sessionId);
    if (!s || (!s.unread && s.last_read_seq >= seq)) return;
    this.sessions.set(sessionId, { ...s, unread: 0, last_read_seq: Math.max(s.last_read_seq, seq) });
    if (s.unread) this.changed();
  }

  /** A session as the hub sent it over HTTP, unless it was read here further than the hub
   *  knew when it answered: a request sent just before the read cursor moved may arrive after
   *  the summary that followed the move, and its unread count would stay. */
  private keepRead(s: SessionInfo): SessionInfo {
    const known = this.sessions.get(s.id);
    return known && known.last_read_seq > s.last_read_seq
      ? { ...s, unread: known.unread, last_read_seq: known.last_read_seq }
      : s;
  }
}

export function createApp(): AppState {
  return new AppState(new Api(), new HubSocket({ url: hubSocketUrl() }));
}
