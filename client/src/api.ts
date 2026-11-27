// REST client for the hub (SPEC §6). Auth is Cloudflare Access in front of the hub; the
// client sends cookies along and never handles tokens itself.

import {
  parseArtifact,
  parseArtifactUrl,
  parseCandidateList,
  parseDirList,
  parseEnrollConfirmed,
  parseEnrollInfo,
  parseEvent,
  parseEventPage,
  parseFavorite,
  parseInputResponse,
  parseMe,
  parseProject,
  parseProjectList,
  parseSearchPage,
  parseScheduleList,
  parseSchedule,
  parseSessionInfo,
  parseSessionList,
  parseSessionOrder,
  parseSubagentHistory,
  parseVapidKey,
  parseWorkspace,
  parseWorkspaceInspection,
  parseWorkspaceList,
  parseWorkspacePermissions,
  parseWorkspaceRuleList,
  type Artifact,
  type ArtifactKind,
  type ArtifactUrl,
  type Candidate,
  type ClarifyAction,
  type ClarifyAnswer,
  type DirEntry,
  type EnrollConfirmed,
  type EnrollInfo,
  type HqcEvent,
  type LookName,
  type Me,
  type ModelProvider,
  parseModelProvider,
  type McpServer,
  type McpServerList,
  parseMcpServer,
  parseLocalMcpServer,
  type Skill,
  type SkillFile,
  parseSkill,
  type PermissionMode,
  type Project,
  type ProjectSort,
  type ProjectColor,
  type SearchKind,
  type SearchPage,
  type Schedule,
  type ListMode,
  type RoomMode,
  type SessionInfo,
  type SessionOrder,
  type SubagentHistory,
  type Theme,
  type Workspace,
  type WorkspaceInspection,
  type WorkspacePermissions,
  type WorkspaceRule,
} from './proto';

/** The page of events `raws` (oldest first); `from` is the `maxSeq` of an empty one. */
export function eventPageOf(raws: unknown[], from: number): EventPage {
  const page: EventPage = { events: [], invalid: [], raw: raws, maxSeq: from, minSeq: undefined, count: 0 };
  for (const raw of raws) {
    page.count++;
    const seq = (raw as { seq?: unknown })?.seq;
    if (typeof seq === 'number') {
      page.maxSeq = Math.max(page.maxSeq, seq);
      page.minSeq = Math.min(page.minSeq ?? seq, seq);
    }
    try {
      page.events.push(parseEvent(raw));
    } catch (e) {
      page.invalid.push({ raw, error: (e as Error).message });
    }
  }
  return page;
}

export class ApiError extends Error {
  constructor(
    readonly status: number,
    message: string,
    /** The hub's error code (`{error, message}`, SPEC §6), e.g. `request_closed`. */
    readonly code?: string,
    /** The whole error body, for codes with more fields (`workspace_conflict`). */
    readonly body?: unknown,
  ) {
    super(message);
    this.name = 'ApiError';
  }
}

export type Fetch = typeof fetch;

export interface NewSessionOptions {
  workspace_id?: string;
  cwd?: string;
  /** Absent: the worker's `[permissions] mode` (T38). */
  permission_mode?: PermissionMode;
  /** Absent: the runtime's default model. */
  model?: string;
  /** A provider the workspace's worker allows (SPEC 0.144); absent: the runtime's own
   *  endpoint. */
  provider_id?: string;
  /** The session starts in a new worktree of the workspace (SPEC 0.265): the request waits
   *  until the worker made it. */
  worktree?: NewWorktree;
}

/** A new session's worktree (SPEC 0.265): its branch (`hqc/session-N` after the session when
 *  empty), new from `base` (the workspace's `HEAD` when empty), or the existing branch with
 *  `existing`. */
export interface NewWorktree {
  branch: string;
  base: string;
  existing?: boolean;
}

/** `POST /api/projects/{id}/rooms`: a room and one member session per agent, at least one. The
 *  workspace, folder and permission mode are its agents', with the defaults of a new session;
 *  an agent's absent `model` is the runtime's default, its `provider_id` one the workspace's
 *  worker allows, as for a new session; its `name` is what the room's feed calls it. */
export interface NewRoom {
  title?: string;
  mode: RoomMode;
  workspace_id?: string;
  cwd?: string;
  permission_mode?: PermissionMode;
  agents: { name?: string; model?: string; provider_id?: string; system_prompt?: string }[];
}

/** The filters, order and page of `GET /api/search` (SPEC §15). */
export interface SearchOptions {
  fuzzy?: boolean;
  project?: string;
  session?: string;
  kind?: SearchKind;
  /** ISO time: hits at or after it. */
  since?: string;
  newest?: boolean;
  perSession?: number;
  limit?: number;
  cursor?: string;
}

/** `PATCH /api/sessions/{id}`: absent fields stay; `null` is the runtime's default. */
export interface SessionPatch {
  title?: string;
  permission_mode?: PermissionMode;
  model?: string | null;
  /** `null`: back to the runtime's own endpoint; a change restarts a running agent. */
  provider_id?: string | null;
  /** The owner's Chrome for the agent (SPEC 0.148); a change restarts a running agent. */
  browser?: boolean;
}

/** The fields of `POST /api/providers` and `PATCH /api/providers/{id}` (all of them). */
export interface ProviderFields {
  name: string;
  label: string;
  base_url: string;
  model: string;
  context_tokens?: number;
}

/** `POST /api/providers/probe` (SPEC 0.236): the models the endpoint lists, as one of the
 *  owner's online workers read them; `error` when none did. */
export interface ProviderProbe {
  models: { id: string; context_tokens?: number }[];
  error?: string;
  worker_id?: string;
}

/** `POST /api/mcp/probe` (SPEC 0.236): what a server says before any sign-in. `token`: it
 *  refuses without credentials and names no authorization server. */
export interface McpProbe {
  name?: string;
  server_name?: string;
  auth: 'none' | 'oauth' | 'token' | 'unknown';
  error?: string;
}

/** One header of a remote MCP server as the owner gives it; the value is write-only. */
export interface McpHeader {
  name: string;
  value: string;
}

/** `POST /api/skills` (SPEC 0.245): a `.zip`, or a folder's files, `path` relative to the
 *  folder (without the folder's own name). */
export type SkillUpload = { zip: Blob; name: string } | { files: { file: Blob; path: string }[]; name: string };

/** `POST /api/mcp/servers` and `PATCH /api/mcp/servers/{id}` (SPEC 0.235): in a `PATCH` an
 *  absent field stays as it is, and `headers` replaces all of the server's headers. */
export interface McpServerFields {
  name?: string;
  url?: string;
  headers?: McpHeader[];
  timeout_s?: number;
  all_projects?: boolean;
  project_ids?: string[];
  /** T65: `oauth` signs in at the server's authorization server; the client secret is
   *  write-only. */
  auth?: 'headers' | 'oauth';
  oauth_client_id?: string;
  oauth_client_secret?: string;
  oauth_scope?: string;
}

/** A page of events; `invalid` are raw events that failed validation (shown, not dropped). */
export interface EventPage {
  events: HqcEvent[];
  invalid: { raw: unknown; error: string }[];
  /** Every event as received, valid or not: what the device keeps (`HistoryCache`). */
  raw: unknown[];
  /** Highest `seq` in the page, valid or not. */
  maxSeq: number;
  /** Lowest `seq` in the page, valid or not. */
  minSeq: number | undefined;
  count: number;
}

export interface WorkerInfo {
  id: string;
  name: string;
  os?: string;
  /** `online` | `offline` | `revoked`. */
  status?: string;
  revoked_at?: string | null;
  last_seen?: string;
  /** The usage limits of the worker's account as it last reported them (SPEC §6); absent
   *  until a turn has run there since the hub started. */
  limits?: WorkerLimits | null;
  /** `session_secrets = true` in its `worker.toml` (SPEC 0.147): it takes secrets and ssh
   *  keys for its owner's sessions. */
  session_secrets?: boolean;
  /** `browser = true` in its `worker.toml` (SPEC 0.148): a session of its owner may run with
   *  the user's Chrome on that machine. */
  browser?: boolean;
  /** Its `hello.capabilities`: `claude_code.login` (SPEC 0.185) — its Claude Code is signed
   *  in from hqc. */
  capabilities?: { claude_code?: { version?: string; login?: boolean } };
}

/** How a worker's own Claude Code is signed in (SPEC 0.185, `claude auth status`). */
export interface ClaudeAuthStatus {
  logged_in: boolean;
  auth_method?: string;
  email?: string;
  org_name?: string;
  subscription_type?: string;
  expires_at?: string;
  /** The status could not be read. */
  error?: string;
}

/** A sign-in under way: the page to sign in on, which gives the code to send back. */
export interface ClaudeLogin {
  login_id: string;
  url: string;
}

/** How a sign-in ended. */
export interface ClaudeLoginDone {
  ok: boolean;
  message?: string;
  status: ClaudeAuthStatus;
}

/** What `POST /api/sessions/{id}/secrets` sends (SPEC 0.147): never kept by the client. */
export interface SecretFields {
  kind: 'secret' | 'ssh_key';
  name: string;
  /** The value typed, or: */
  value?: string;
  /** the Vaultwarden entry the worker reads it from itself (exactly one of the two). */
  vault?: string;
  /** The field of that entry to read instead of its password (only with `vault`). */
  vault_field?: string;
  passphrase?: string;
  remember?: boolean;
}

/** The answer: what the agent's side was told, never the value. */
export interface SecretGiven {
  note: string;
  path?: string;
  fingerprint?: string;
  public_key?: string;
  remembered?: boolean;
}

/** An ssh key the worker keeps for a workspace (SPEC 0.147). */
export interface WorkspaceKey {
  name: string;
  fingerprint: string;
}

/** `utilization` 0..1, `resets_at` Unix seconds. */
export interface LimitWindow {
  utilization: number;
  resets_at: number;
}

export interface WorkerLimits {
  /** The 5-hour window ("Current session" of Claude Code's `/usage`). */
  five_hour?: LimitWindow | null;
  /** The 7-day window of all models. */
  seven_day?: LimitWindow | null;
  /** The state of the limit nearest (since SPEC 0.179): `allowed`, `allowed_warning`,
   *  `rejected`; which limit it is (`five_hour`, `seven_day`, …) and when it resets. */
  status?: string | null;
  rate_limit_type?: string | null;
  resets_at?: number | null;
  reported_at: string;
}

export class Api {
  constructor(
    private readonly base = '',
    private readonly fetchImpl: Fetch = (...a) => fetch(...a),
  ) {}

  /** `body`: JSON, or `FormData` sent as multipart (the browser sets its content type).
   *  `keepalive`: the request outlives the page (sent while it is being left). `redirect`:
   *  `manual` for `/api/me`, see `send`. */
  private async req(
    method: string,
    path: string,
    body?: unknown,
    keepalive = false,
    redirect?: RequestRedirect,
  ): Promise<unknown> {
    return (await this.send(method, path, body, keepalive, redirect)).data;
  }

  /** `req` with the answer's status. A redirect comes back only with `redirect: 'manual'`, and
   *  then it is a sign-in proxy's answer to a signed-out user (Cloudflare Access, oauth2-proxy),
   *  not the hub's: it fails as `401`, as the hub's own answer would (SPEC §6). Followed, it
   *  would lead to a sign-in page on another origin, which `fetch` cannot read, and fail as
   *  offline. */
  private async send(
    method: string,
    path: string,
    body?: unknown,
    keepalive = false,
    redirect?: RequestRedirect,
  ): Promise<{ status: number; data: unknown }> {
    const form = typeof FormData !== 'undefined' && body instanceof FormData;
    const res = await this.fetchImpl(this.base + path, {
      method,
      credentials: 'include',
      ...(keepalive ? { keepalive } : {}),
      ...(redirect ? { redirect } : {}),
      headers: body === undefined || form ? undefined : { 'content-type': 'application/json' },
      body: body === undefined ? undefined : form ? (body as FormData) : JSON.stringify(body),
    });
    // A browser gives an opaque answer with status 0; a fetch outside one, the 3xx itself.
    if (res.type === 'opaqueredirect' || (res.status >= 300 && res.status < 400)) {
      throw new ApiError(401, 'Signed out: reload the page to sign in');
    }
    const text = await res.text();
    let data: unknown = undefined;
    if (text) {
      try {
        data = JSON.parse(text);
      } catch {
        data = text;
      }
    }
    if (!res.ok) {
      const body = typeof data === 'object' && data !== null ? (data as Record<string, unknown>) : {};
      const msg =
        typeof body.message === 'string'
          ? body.message
          : typeof data === 'string' && data
            ? data
            : res.statusText;
      const code = typeof body.error === 'string' ? body.error : undefined;
      throw new ApiError(res.status, msg || `HTTP ${res.status}`, code, data);
    }
    return { status: res.status, data };
  }

  async me(): Promise<Me> {
    return parseMe(await this.req('GET', '/api/me', undefined, false, 'manual'));
  }

  /** The default zone of new schedules (SPEC §16). */
  async setTimezone(timezone: string): Promise<Me> {
    return parseMe(await this.req('PATCH', '/api/me', { timezone }));
  }

  /** The app theme (T49), the same on every device. */
  async setTheme(theme: Theme): Promise<Me> {
    return parseMe(await this.req('PATCH', '/api/me', { theme }));
  }

  /** The client's look, the same on every device. */
  async setLook(look: LookName): Promise<Me> {
    return parseMe(await this.req('PATCH', '/api/me', { look }));
  }

  /** The project bar's order, the same on every device. */
  async setProjectSort(project_sort: ProjectSort): Promise<Me> {
    return parseMe(await this.req('PATCH', '/api/me', { project_sort }));
  }

  /** The project a new session is quick-started in, the same on every device. */
  async setNewSessionProject(new_session_project_id: string): Promise<Me> {
    return parseMe(await this.req('PATCH', '/api/me', { new_session_project_id }));
  }

  /** The user's own quick messages: the whole list replaces the stored one. */
  async setUserQuickMessages(quick_messages: string[]): Promise<Me> {
    return parseMe(await this.req('PATCH', '/api/me', { quick_messages }));
  }

  async schedules(sessionId: string): Promise<Schedule[]> {
    return parseScheduleList(await this.req('GET', `/api/schedules?session_id=${enc(sessionId)}`));
  }

  async patchSchedule(id: string, patch: { enabled: boolean }): Promise<Schedule> {
    return parseSchedule(await this.req('PATCH', `/api/schedules/${enc(id)}`, patch));
  }

  async deleteSchedule(id: string): Promise<void> {
    await this.req('DELETE', `/api/schedules/${enc(id)}`);
  }

  async sessions(projectId: string): Promise<SessionInfo[]> {
    return parseSessionList(await this.req('GET', `/api/projects/${enc(projectId)}/sessions`));
  }

  /** The sessions of every project of the user in one request. */
  async allSessions(): Promise<SessionInfo[]> {
    return parseSessionList(await this.req('GET', '/api/sessions'));
  }

  /** No `title`: the hub's default "Session N", which the runtime's title may replace. No
   *  `workspace_id`: the project's default workspace; `cwd` is relative to it (SPEC §13.4). */
  async createSession(projectId: string, title?: string, opts: NewSessionOptions = {}): Promise<SessionInfo> {
    const body: Record<string, string | boolean> = {};
    if (title) body.title = title;
    if (opts.workspace_id) body.workspace_id = opts.workspace_id;
    if (opts.cwd && opts.cwd !== '.') body.cwd = opts.cwd;
    if (opts.permission_mode) body.permission_mode = opts.permission_mode;
    if (opts.model) body.model = opts.model;
    if (opts.provider_id) body.provider_id = opts.provider_id;
    if (opts.worktree) {
      body.isolation = 'worktree';
      const branch = opts.worktree.branch.trim();
      const base = opts.worktree.base.trim();
      if (branch) body.branch = branch;
      if (opts.worktree.existing && branch) body.existing_branch = true;
      else if (base) body.base = base;
    }
    const data = await this.req('POST', `/api/projects/${enc(projectId)}/sessions`, body);
    return parseSessionInfo(data);
  }

  /** The room's session object. */
  async createRoom(projectId: string, body: NewRoom): Promise<SessionInfo> {
    return parseSessionInfo(await this.req('POST', `/api/projects/${enc(projectId)}/rooms`, body));
  }

  async patchSession(id: string, patch: SessionPatch): Promise<SessionInfo> {
    return parseSessionInfo(await this.req('PATCH', `/api/sessions/${enc(id)}`, patch));
  }

  /** The user's order of the project's session list (T50). */
  async sessionOrder(projectId: string): Promise<SessionOrder> {
    return parseSessionOrder(await this.req('GET', `/api/projects/${enc(projectId)}/session_order`));
  }

  async putSessionOrder(projectId: string, body: { mode?: ListMode; session_ids?: string[] }): Promise<SessionOrder> {
    return parseSessionOrder(await this.req('PUT', `/api/projects/${enc(projectId)}/session_order`, body));
  }

  async project(id: string): Promise<Project> {
    return parseProject(await this.req('GET', `/api/projects/${enc(id)}`));
  }

  async projects(): Promise<Project[]> {
    return parseProjectList(await this.req('GET', '/api/projects'));
  }

  /** The archived projects, which `projects` leaves out: the project search finds them. */
  async archivedProjects(): Promise<Project[]> {
    return parseProjectList(await this.req('GET', '/api/projects?archived=1'));
  }

  /** Off the project bar; its sessions stay as they are. */
  async archiveProject(id: string): Promise<Project> {
    return parseProject(await this.req('POST', `/api/projects/${enc(id)}/archive`));
  }

  async unarchiveProject(id: string): Promise<Project> {
    return parseProject(await this.req('POST', `/api/projects/${enc(id)}/unarchive`));
  }

  /** The order of the project bar (since SPEC 0.64): the reply is the list in it. */
  async putProjectOrder(projectIds: string[]): Promise<Project[]> {
    return parseProjectList(await this.req('PUT', '/api/me/project_order', { project_ids: projectIds }));
  }

  async createProject(name: string, description?: string): Promise<Project> {
    const body = description ? { name, description } : { name };
    return parseProject(await this.req('POST', '/api/projects', body));
  }

  async patchProject(
    id: string,
    patch: {
      name?: string;
      description?: string;
      default_workspace_id?: string;
      color?: ProjectColor;
      /** An emoji or letters; an image goes through {@link Api.uploadProjectIcon}. */
      icon?: { kind: 'emoji' | 'letters'; value: string };
      /** Replaces the whole list. */
      quick_messages?: string[];
    },
  ): Promise<Project> {
    return parseProject(await this.req('PATCH', `/api/projects/${enc(id)}`, patch));
  }

  /** The project's icon image (T49): PNG, JPEG or WebP, square, 128–1024 px, at most 256 KB;
   *  anything else `422 invalid_icon` with the reason. Makes it the icon at once. */
  async uploadProjectIcon(id: string, file: Blob, name: string): Promise<Project> {
    const form = new FormData();
    form.append('file', file, name);
    return parseProject(await this.req('PUT', `/api/projects/${enc(id)}/icon`, form));
  }

  /** Deletes the project with its workspace records and sessions (folders stay). */
  async deleteProject(id: string): Promise<void> {
    await this.req('DELETE', `/api/projects/${enc(id)}`);
  }

  async workspaces(projectId: string): Promise<Workspace[]> {
    return parseWorkspaceList(await this.req('GET', `/api/workspaces?project_id=${enc(projectId)}`));
  }

  /** The worker checks the folder (`fs.stat`); `409 workspace_conflict` carries the
   *  workspace that already holds it in `ApiError.body` (`parseWorkspaceConflict`). `kind:
   *  clone` (T45): `path` is the new folder, the worker clones `url` into it and the workspace
   *  is `creating` until it is done. `kind: worktree` (T46): a worktree of
   *  `origin_workspace_id` on `branch` (`new_branch`: made from the origin's HEAD); without
   *  `path` the hub puts it next to the origin, `<origin-name>-<branch>`. */
  async createWorkspace(w: {
    project_id: string;
    worker_id: string;
    path?: string;
    label?: string;
    kind?: 'clone' | 'worktree';
    url?: string;
    branch?: string;
    origin_workspace_id?: string;
    new_branch?: boolean;
    /** What a worktree's new branch starts from instead of the origin's `HEAD` (SPEC 0.265). */
    base?: string;
    /** A missing folder is made by the worker (since SPEC 0.64). */
    create?: boolean;
    /** The worker mirrors the folder's Claude Code sessions (SPEC §9.1); on by default
     *  since T21. */
    mirror_enabled?: boolean;
  }): Promise<Workspace> {
    return parseWorkspace(await this.req('POST', '/api/workspaces', w));
  }

  /** `PATCH /api/workspaces/{id}`: the label, or the switch "VS Code sessions" (T21). */
  async patchWorkspace(id: string, patch: { label?: string; mirror_enabled?: boolean }): Promise<Workspace> {
    return parseWorkspace(await this.req('PATCH', `/api/workspaces/${enc(id)}`, patch));
  }

  /** hqc's allow rules of the folder (T48), newest first. */
  async workspaceRules(id: string): Promise<WorkspaceRule[]> {
    return parseWorkspaceRuleList(await this.req('GET', `/api/workspaces/${enc(id)}/rules`));
  }

  /** Adds rules (ones the folder has are kept); `422 invalid_rule` saves none. Running
   *  sessions in the folder use them at their next request. */
  async addWorkspaceRules(id: string, rules: string[]): Promise<WorkspaceRule[]> {
    return parseWorkspaceRuleList(await this.req('POST', `/api/workspaces/${enc(id)}/rules`, { rules }));
  }

  async deleteWorkspaceRule(id: string, ruleId: string): Promise<void> {
    await this.req('DELETE', `/api/workspaces/${enc(id)}/rules/${enc(ruleId)}`);
  }

  /** The rules the worker reads for the folder and, with `check`, how it decides a command or
   *  a tool; `409 worker_offline` when the worker is not connected. */
  async workspacePermissions(id: string, check?: { command: string } | { tool: string }): Promise<WorkspacePermissions> {
    const q = check ? `?${new URLSearchParams(check)}` : '';
    return parseWorkspacePermissions(await this.req('GET', `/api/workspaces/${enc(id)}/permissions${q}`));
  }

  /** Folders with Claude Code sessions on the worker (`workspace.list_candidates`, SPEC §9.1). */
  async candidates(workerId: string): Promise<Candidate[]> {
    return parseCandidateList(await this.req('GET', `/api/workers/${enc(workerId)}/candidates`));
  }

  /** Continue a session VS Code drove (SPEC §9.3); `409 vscode_active` within 30 s of its
   *  last write. */
  async takeover(id: string): Promise<SessionInfo> {
    return parseSessionInfo(await this.req('POST', `/api/sessions/${enc(id)}/takeover`));
  }

  /** The session continues in folder `path` of its worker, in `project` (an id) or a new
   *  project named after the folder; the errors of `createWorkspace` (`path_not_found`:
   *  `create` makes the folder). No input is queued. */
  async moveSession(id: string, body: { path: string; project?: string; create?: boolean }): Promise<SessionInfo> {
    return parseSessionInfo(await this.req('POST', `/api/sessions/${enc(id)}/move`, body));
  }

  /** A new session in the same project and folder whose conversation is this one's before
   *  the user's message `eventId` (SPEC §6). */
  async forkSession(id: string, eventId: string): Promise<SessionInfo> {
    return parseSessionInfo(await this.req('POST', `/api/sessions/${enc(id)}/fork`, { event_id: eventId }));
  }

  /** Removes the record; the folder is not touched (SPEC §13.2) unless `removeFolder` is set
   *  for a folder hqc created, which the worker then deletes (T45). `force`: the user saw and
   *  confirmed the uncommitted files, which `git worktree remove` keeps otherwise (T46).
   *  `archiveSessions`: the user confirmed archiving the sessions that work in it. */
  async deleteWorkspace(id: string, removeFolder = false, force = false, archiveSessions = false): Promise<void> {
    const q = new URLSearchParams();
    if (removeFolder) q.set('remove_folder', 'true');
    if (removeFolder && force) q.set('force', 'true');
    if (archiveSessions) q.set('archive_sessions', 'true');
    await this.req('DELETE', `/api/workspaces/${enc(id)}${q.size ? `?${q}` : ''}`);
  }

  /** Uncommitted changes and unpushed commits of a folder hqc created (T45); `409
   *  worker_offline` when the worker is not connected. */
  async inspectWorkspace(id: string): Promise<WorkspaceInspection> {
    return parseWorkspaceInspection(await this.req('GET', `/api/workspaces/${enc(id)}/inspect`));
  }

  /** Folders on the worker (`fs.list_dirs`): its allowed_roots without `path`. */
  async workerDirs(workerId: string, path?: string): Promise<DirEntry[]> {
    const q = path ? `?path=${enc(path)}` : '';
    return parseDirList(await this.req('GET', `/api/workers/${enc(workerId)}/dirs${q}`));
  }

  async session(id: string): Promise<SessionInfo> {
    return parseSessionInfo(await this.req('GET', `/api/sessions/${enc(id)}`));
  }

  /** `GET /api/search` (SPEC §15): the user's sessions, best hits first (`newest`: newest
   *  first), within a project, a session, a kind of hit and a time (`since`, ISO), at most
   *  `perSession` hits of each session. */
  async search(q: string, opts: SearchOptions = {}): Promise<SearchPage> {
    const p = new URLSearchParams({ q });
    if (opts.fuzzy) p.set('fuzzy', '1');
    if (opts.project) p.set('project', opts.project);
    if (opts.session) p.set('session', opts.session);
    if (opts.kind) p.set('kind', opts.kind);
    if (opts.since) p.set('since', opts.since);
    if (opts.newest) p.set('order', 'newest');
    if (opts.perSession) p.set('per_session', String(opts.perSession));
    if (opts.limit) p.set('limit', String(opts.limit));
    if (opts.cursor) p.set('cursor', opts.cursor);
    return parseSearchPage(await this.req('GET', `/api/search?${p}`));
  }

  async events(id: string, afterSeq: number, limit = 200): Promise<EventPage> {
    return this.eventPage(id, `after_seq=${afterSeq}&limit=${limit}`, afterSeq);
  }

  /** The last `limit` events before `beforeSeq` (all of them: the newest), oldest first. */
  async eventsBefore(id: string, beforeSeq = Number.MAX_SAFE_INTEGER, limit = 200): Promise<EventPage> {
    return this.eventPage(id, `before_seq=${beforeSeq}&limit=${limit}`, 0);
  }

  private async eventPage(id: string, q: string, from: number): Promise<EventPage> {
    const data = await this.req('GET', `/api/sessions/${enc(id)}/events?${q}`);
    return eventPageOf(parseEventPage(data).events, from);
  }

  /** Returns the stored `user.message`; a repeated `client_op_id` returns the same event.
   *  `artifactIds` come from {@link Api.uploadArtifact} (SPEC §12). */
  async input(id: string, text: string, clientOpId: string, artifactIds: string[] = []): Promise<HqcEvent> {
    const data = await this.req('POST', `/api/sessions/${enc(id)}/input`, {
      text,
      artifact_ids: artifactIds,
      client_op_id: clientOpId,
    });
    return parseInputResponse(data);
  }

  /** Asks the session's agent a side question (SPEC 0.232); returns the stored `side.question`. */
  async sideQuestion(id: string, text: string): Promise<HqcEvent> {
    return parseInputResponse(await this.req('POST', `/api/sessions/${enc(id)}/side_question`, { text }));
  }

  /** `POST /api/artifacts` (multipart `file`, `kind?`); the hub derives `kind` from the type
   *  and extension unless given. The same file uploaded again is the same artifact. */
  async uploadArtifact(file: Blob, name: string, kind?: ArtifactKind): Promise<Artifact> {
    const form = new FormData();
    form.append('file', file, name);
    if (kind && kind !== 'unknown') form.append('kind', kind);
    return parseArtifact(await this.req('POST', '/api/artifacts', form));
  }

  async artifact(id: string): Promise<Artifact> {
    return parseArtifact(await this.req('GET', `/api/artifacts/${enc(id)}`));
  }

  /** `DELETE /api/artifacts/{id}/publication`: the owner unpublishes a `site` (SPEC 0.209). */
  async unpublishArtifact(id: string): Promise<void> {
    await this.req('DELETE', `/api/artifacts/${enc(id)}/publication`);
  }

  /** A signed link, valid for 10 minutes (SPEC §12). */
  async artifactLink(id: string): Promise<ArtifactUrl> {
    return parseArtifactUrl(await this.req('GET', `/api/artifacts/${enc(id)}/url`));
  }

  async stop(id: string): Promise<void> {
    await this.req('POST', `/api/sessions/${enc(id)}/stop`);
  }

  /** Moves the agent's running call `callId`, one of the session's `foreground_calls`, to the
   *  background. */
  async background(id: string, callId: string): Promise<void> {
    await this.req('POST', `/api/sessions/${enc(id)}/background`, { call_id: callId });
  }

  /** `POST .../inputs/{eventId}/withdraw`: takes back a message of the user's that the agent has
   *  not taken (SPEC 0.219); `409 already_taken` when it has. */
  async withdrawInput(id: string, eventId: string): Promise<void> {
    await this.req('POST', `/api/sessions/${enc(id)}/inputs/${enc(eventId)}/withdraw`);
  }

  /** `POST .../inputs/{eventId}/now`: sends a message of the user's that waits into the running
   *  turn at once (SPEC 0.219); `false` when the agent had taken it already, `409 not_now` when
   *  it cannot be taken now. */
  async inputNow(id: string, eventId: string): Promise<boolean> {
    const r = (await this.req('POST', `/api/sessions/${enc(id)}/inputs/${enc(eventId)}/now`)) as { sent_now?: unknown };
    return r?.sent_now === true;
  }

  /** Ends the agent's process and starts it again, resuming the conversation (SPEC §17.3). */
  async restartSession(id: string): Promise<void> {
    await this.req('POST', `/api/sessions/${enc(id)}/restart`);
  }

  /** Stops a running turn at once, as Stop does, and the agent's process with it (at once when
   *  no turn runs); holds input until {@link Api.resumeSession} (SPEC §17.3). */
  async pauseSession(id: string): Promise<void> {
    await this.req('POST', `/api/sessions/${enc(id)}/pause`);
  }

  /** Starts a paused session's agent again; it hears first how long the pause was. */
  async resumeSession(id: string): Promise<void> {
    await this.req('POST', `/api/sessions/${enc(id)}/resume`);
  }

  /** Stops the agent's task `taskId` alone, a `stoppable` row of its `subagents.live`. */
  async stopTask(id: string, taskId: string): Promise<void> {
    await this.req('POST', `/api/sessions/${enc(id)}/tasks/${enc(taskId)}/stop`);
  }

  /** What subagent `taskId` (a `task_id` of `subagents.live`) has done, read from its
   *  transcript on the worker; `after`: the `event_id` of the last event shown, for only the
   *  ones after it (`continues`); `409 worker_offline`, `504 worker_timeout`. */
  async subagentHistory(id: string, taskId: string, after?: string): Promise<SubagentHistory> {
    const q = after ? `?${new URLSearchParams({ after })}` : '';
    return parseSubagentHistory(await this.req('GET', `/api/sessions/${enc(id)}/subagents/${enc(taskId)}/history${q}`));
  }

  async retry(id: string): Promise<void> {
    await this.req('POST', `/api/sessions/${enc(id)}/retry`);
  }

  /** `keepalive`: sent while the page is left (T50, the delayed archive). */
  async archive(id: string, keepalive = false): Promise<SessionInfo> {
    return parseSessionInfo(await this.req('POST', `/api/sessions/${enc(id)}/archive`, undefined, keepalive));
  }

  async unarchive(id: string): Promise<SessionInfo> {
    return parseSessionInfo(await this.req('POST', `/api/sessions/${enc(id)}/unarchive`));
  }

  /** Deletes a session with its history (SPEC §17.4). */
  async deleteSession(id: string): Promise<void> {
    await this.req('DELETE', `/api/sessions/${enc(id)}`);
  }

  /** `message`: a text answer instead of the call, with a deny option (SPEC §10.1). */
  async approve(id: string, requestId: string, optionId: string, message?: string): Promise<void> {
    await this.req('POST', `/api/sessions/${enc(id)}/requests/${enc(requestId)}`, {
      option_id: optionId,
      ...(message !== undefined ? { message } : {}),
    });
  }

  /** `action`: the person's choice on an MCP server's request (SPEC §6). */
  async clarify(id: string, requestId: string, answers: Record<string, ClarifyAnswer>, action?: ClarifyAction) {
    await this.req('POST', `/api/sessions/${enc(id)}/requests/${enc(requestId)}`, {
      answers,
      ...(action !== undefined ? { action } : {}),
    });
  }

  async readCursor(id: string, lastReadSeq: number): Promise<void> {
    await this.req('PUT', `/api/sessions/${enc(id)}/read_cursor`, { last_read_seq: lastReadSeq });
  }

  /** The session is one of the user's favorites, or no longer: the time it became one. */
  async setSessionFavorite(id: string, on: boolean): Promise<string | undefined> {
    return this.favorite(`/api/sessions/${enc(id)}/favorite`, on);
  }

  /** The same for a project. */
  async setProjectFavorite(id: string, on: boolean): Promise<string | undefined> {
    return this.favorite(`/api/projects/${enc(id)}/favorite`, on);
  }

  private async favorite(path: string, on: boolean): Promise<string | undefined> {
    if (!on) {
      await this.req('DELETE', path);
      return undefined;
    }
    return parseFavorite(await this.req('PUT', path));
  }

  async artifactUrl(id: string): Promise<string> {
    return (await this.artifactLink(id)).url;
  }

  /** Workers that are not revoked; `includeRevoked` adds revoked ones (`status: revoked`). */
  /** The owner's model providers (SPEC 0.144). */
  async providers(): Promise<ModelProvider[]> {
    const data = (await this.req('GET', '/api/providers')) as { providers?: unknown };
    if (!Array.isArray(data?.providers)) throw new ApiError(500, 'providers: expected a list');
    return data.providers.map(parseModelProvider);
  }

  async createProvider(f: ProviderFields): Promise<ModelProvider> {
    return parseModelProvider(await this.req('POST', '/api/providers', f));
  }

  async updateProvider(id: string, f: ProviderFields): Promise<ModelProvider> {
    return parseModelProvider(await this.req('PATCH', `/api/providers/${enc(id)}`, f));
  }

  async deleteProvider(id: string): Promise<void> {
    await this.req('DELETE', `/api/providers/${enc(id)}`);
  }

  /** The models at `base_url`; `409 worker_offline` when no worker of the owner is online. */
  async probeProvider(base_url: string, name?: string): Promise<ProviderProbe> {
    const v = (await this.req('POST', '/api/providers/probe', name ? { base_url, name } : { base_url })) as Record<string, unknown>;
    const models = Array.isArray(v?.models)
      ? v.models.flatMap((m: unknown) => {
          const o = m as { id?: unknown; context_tokens?: unknown };
          if (typeof o?.id !== 'string' || !o.id) return [];
          return [typeof o.context_tokens === 'number' ? { id: o.id, context_tokens: o.context_tokens } : { id: o.id }];
        })
      : [];
    return {
      models,
      ...(typeof v?.error === 'string' ? { error: v.error } : {}),
      ...(typeof v?.worker_id === 'string' ? { worker_id: v.worker_id } : {}),
    };
  }

  /** How the MCP server at `url` signs in, and a name for it; `400` for a URL the hub refuses. */
  async probeMcp(url: string): Promise<McpProbe> {
    const v = (await this.req('POST', '/api/mcp/probe', { url })) as Record<string, unknown>;
    const auth = ['none', 'oauth', 'token'].includes(v?.auth as string) ? (v.auth as McpProbe['auth']) : 'unknown';
    return {
      auth,
      ...(typeof v?.name === 'string' && v.name ? { name: v.name } : {}),
      ...(typeof v?.server_name === 'string' && v.server_name ? { server_name: v.server_name } : {}),
      ...(typeof v?.error === 'string' ? { error: v.error } : {}),
    };
  }

  /** The owner's remote MCP servers (SPEC 0.235) and, since 0.262, the servers its workers'
   *  `worker.toml` declare (`local`; a hub without them: none). */
  async mcpServers(): Promise<McpServerList> {
    const data = (await this.req('GET', '/api/mcp/servers')) as { servers?: unknown; local?: unknown };
    if (!Array.isArray(data?.servers)) throw new ApiError(500, 'mcp servers: expected a list');
    return {
      servers: data.servers.map(parseMcpServer),
      local: Array.isArray(data.local) ? data.local.map(parseLocalMcpServer) : [],
    };
  }

  async createMcpServer(f: McpServerFields): Promise<McpServer> {
    return parseMcpServer(await this.req('POST', '/api/mcp/servers', f));
  }

  async updateMcpServer(id: string, f: McpServerFields): Promise<McpServer> {
    return parseMcpServer(await this.req('PATCH', `/api/mcp/servers/${enc(id)}`, f));
  }

  async deleteMcpServer(id: string): Promise<void> {
    await this.req('DELETE', `/api/mcp/servers/${enc(id)}`);
  }

  /** Runs `initialize` and `tools/list` against the server: it with a fresh status and tools. */
  async testMcpServer(id: string): Promise<McpServer> {
    return parseMcpServer(await this.req('POST', `/api/mcp/servers/${enc(id)}/test`));
  }

  /** T65: the page of the server's authorization server to sign in on. */
  async signInMcpServer(id: string): Promise<string> {
    const v = (await this.req('POST', `/api/mcp/servers/${enc(id)}/sign_in`)) as { url?: unknown };
    if (typeof v?.url !== 'string') throw new Error('no sign-in page');
    return v.url;
  }

  async signOutMcpServer(id: string): Promise<void> {
    await this.req('DELETE', `/api/mcp/servers/${enc(id)}/sign_in`);
  }

  /** The owner's skills (SPEC 0.245). */
  async skills(): Promise<Skill[]> {
    const data = (await this.req('GET', '/api/skills')) as { skills?: unknown };
    if (!Array.isArray(data?.skills)) throw new ApiError(500, 'skills: expected a list');
    return data.skills.map(parseSkill);
  }

  /** Uploads a skill: a `.zip`, or a folder's files with their paths inside it. `name`: the
   *  folder's or the `.zip`'s, for a `SKILL.md` without one. `replaced`: the hub answered 200,
   *  the owner's skill of that name has the new content (and keeps its projects). */
  async uploadSkill(u: SkillUpload): Promise<{ skill: Skill; replaced: boolean }> {
    const form = new FormData();
    if ('zip' in u) form.append('zip', u.zip, `${u.name}.zip`);
    else for (const f of u.files) form.append('file', f.file, f.path);
    if (u.name) form.append('name', u.name);
    const r = await this.send('POST', '/api/skills', form);
    return { skill: parseSkill(r.data), replaced: r.status === 200 };
  }

  async updateSkill(id: string, f: { all_projects?: boolean; project_ids?: string[] }): Promise<Skill> {
    return parseSkill(await this.req('PATCH', `/api/skills/${enc(id)}`, f));
  }

  async deleteSkill(id: string): Promise<void> {
    await this.req('DELETE', `/api/skills/${enc(id)}`);
  }

  async skillFiles(id: string): Promise<SkillFile[]> {
    const data = (await this.req('GET', `/api/skills/${enc(id)}/files`)) as { files?: unknown };
    if (!Array.isArray(data?.files)) throw new ApiError(500, 'skill files: expected a list');
    return data.files.map((v) => {
      const o = (v ?? {}) as Record<string, unknown>;
      return { path: String(o.path ?? ''), size: Number(o.size ?? 0), ...(o.executable === true ? { executable: true } : {}) };
    });
  }

  /** A secret or an ssh key for the session's running agent (SPEC 0.147); `requestId`
   *  answers the agent's `hqc_secret`. Only the fields go: the hub refuses others. */
  async giveSecret(sessionId: string, f: SecretFields, requestId?: string): Promise<SecretGiven> {
    const body: Record<string, unknown> = { kind: f.kind, name: f.name };
    if (f.value !== undefined) body.value = f.value;
    if (f.vault !== undefined) body.vault = f.vault;
    if (f.vault_field !== undefined) body.vault_field = f.vault_field;
    if (f.kind === 'ssh_key') {
      if (f.passphrase) body.passphrase = f.passphrase;
      if (f.remember) body.remember = true;
    }
    if (requestId) body.request_id = requestId;
    const r = (await this.req('POST', `/api/sessions/${enc(sessionId)}/secrets`, body)) as Partial<SecretGiven> | null;
    if (typeof r?.note !== 'string') throw new ApiError(500, 'secret: expected a note');
    return r as SecretGiven;
  }

  /** The ssh keys the worker keeps for a workspace (SPEC 0.147). */
  async workspaceKeys(id: string): Promise<WorkspaceKey[]> {
    const data = (await this.req('GET', `/api/workspaces/${enc(id)}/keys`)) as { keys?: unknown };
    if (!Array.isArray(data?.keys)) throw new ApiError(500, 'keys: expected a list');
    return data.keys.filter(
      (k): k is WorkspaceKey => typeof k?.name === 'string' && typeof k?.fingerprint === 'string',
    );
  }

  async deleteWorkspaceKey(id: string, name: string): Promise<void> {
    await this.req('DELETE', `/api/workspaces/${enc(id)}/keys/${enc(name)}`);
  }

  /** How the worker's own Claude Code is signed in (SPEC 0.185). */
  async claudeAuth(workerId: string): Promise<ClaudeAuthStatus> {
    return (await this.req('GET', `/api/workers/${enc(workerId)}/claude`)) as ClaudeAuthStatus;
  }

  /** Starts a sign-in of the worker's Claude Code: the page to sign in on. */
  async claudeLogin(workerId: string): Promise<ClaudeLogin> {
    return (await this.req('POST', `/api/workers/${enc(workerId)}/claude/login`)) as ClaudeLogin;
  }

  /** The code the sign-in page gave; used once, kept nowhere. */
  async claudeLoginCode(workerId: string, loginId: string, code: string): Promise<ClaudeLoginDone> {
    return (await this.req('POST', `/api/workers/${enc(workerId)}/claude/login/${enc(loginId)}`, { code })) as ClaudeLoginDone;
  }

  async claudeLoginCancel(workerId: string, loginId: string): Promise<void> {
    await this.req('DELETE', `/api/workers/${enc(workerId)}/claude/login/${enc(loginId)}`);
  }

  async workers(opts: { includeRevoked?: boolean } = {}): Promise<WorkerInfo[]> {
    const q = opts.includeRevoked ? '?include_revoked=1' : '';
    const data = (await this.req('GET', `/api/workers${q}`)) as { workers?: unknown };
    if (!Array.isArray(data?.workers)) throw new ApiError(500, 'workers: expected a list');
    return data.workers as WorkerInfo[];
  }

  /** Revokes the worker's token, drops its connection and archives its service session
   *  (SPEC §5.1, §10.2). */
  async revokeWorker(id: string): Promise<void> {
    await this.req('DELETE', `/api/workers/${enc(id)}`);
  }

  /** Deletes a revoked worker with its service session and workspaces (SPEC §6); `409
   *  worker_has_sessions` (body `sessions_count`) while sessions work there. */
  async purgeWorker(id: string): Promise<void> {
    await this.req('DELETE', `/api/workers/${enc(id)}?purge=1`);
  }

  /** What the worker that printed `code` said about itself and which workers it may
   *  replace; 404 for an unknown code. */
  async enrollInfo(code: string): Promise<EnrollInfo> {
    return parseEnrollInfo(await this.req('GET', `/api/workers/enroll/${enc(code)}`));
  }

  /** `replaceWorkerId`: "the same machine" — the enrolling worker takes over that worker's
   *  id, sessions and workspaces (SPEC §5.1). */
  async enrollConfirm(code: string, replaceWorkerId?: string): Promise<EnrollConfirmed> {
    const body: Record<string, string> = { code };
    if (replaceWorkerId) body.replace_worker_id = replaceWorkerId;
    return parseEnrollConfirmed(await this.req('POST', '/api/workers/enroll/confirm', body));
  }

  /** `null`: the hub has no VAPID keys, push is off. */
  async vapidKey(): Promise<string | null> {
    return parseVapidKey(await this.req('GET', '/api/push/vapid'));
  }

  async pushSubscribe(sub: PushSubscriptionJSON): Promise<void> {
    await this.req('POST', '/api/push/subscribe', sub);
  }
}

const enc = encodeURIComponent;
