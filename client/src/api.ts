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
  type ClarifyAnswer,
  type DirEntry,
  type EnrollConfirmed,
  type EnrollInfo,
  type HqcEvent,
  type Me,
  type PermissionMode,
  type Project,
  type ProjectColor,
  type SearchPage,
  type Schedule,
  type ListMode,
  type SessionInfo,
  type SessionOrder,
  type Theme,
  type Workspace,
  type WorkspaceInspection,
  type WorkspacePermissions,
  type WorkspaceRule,
} from './proto';

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
}

/** `PATCH /api/sessions/{id}`: absent fields stay; `null` is the runtime's default. */
export interface SessionPatch {
  title?: string;
  permission_mode?: PermissionMode;
  model?: string | null;
}

/** A page of events; `invalid` are raw events that failed validation (shown, not dropped). */
export interface EventPage {
  events: HqcEvent[];
  invalid: { raw: unknown; error: string }[];
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
}

export class Api {
  constructor(
    private readonly base = '',
    private readonly fetchImpl: Fetch = (...a) => fetch(...a),
  ) {}

  /** `body`: JSON, or `FormData` sent as multipart (the browser sets its content type).
   *  `keepalive`: the request outlives the page (sent while it is being left). */
  private async req(method: string, path: string, body?: unknown, keepalive = false): Promise<unknown> {
    const form = typeof FormData !== 'undefined' && body instanceof FormData;
    const res = await this.fetchImpl(this.base + path, {
      method,
      credentials: 'include',
      ...(keepalive ? { keepalive } : {}),
      headers: body === undefined || form ? undefined : { 'content-type': 'application/json' },
      body: body === undefined ? undefined : form ? (body as FormData) : JSON.stringify(body),
    });
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
    return data;
  }

  async me(): Promise<Me> {
    return parseMe(await this.req('GET', '/api/me'));
  }

  /** The default zone of new schedules (SPEC §16). */
  async setTimezone(timezone: string): Promise<Me> {
    return parseMe(await this.req('PATCH', '/api/me', { timezone }));
  }

  /** The app theme (T49), the same on every device. */
  async setTheme(theme: Theme): Promise<Me> {
    return parseMe(await this.req('PATCH', '/api/me', { theme }));
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

  /** No `title`: the hub's default "Session N", which the runtime's title may replace. No
   *  `workspace_id`: the project's default workspace; `cwd` is relative to it (SPEC §13.4). */
  async createSession(projectId: string, title?: string, opts: NewSessionOptions = {}): Promise<SessionInfo> {
    const body: Record<string, string> = {};
    if (title) body.title = title;
    if (opts.workspace_id) body.workspace_id = opts.workspace_id;
    if (opts.cwd && opts.cwd !== '.') body.cwd = opts.cwd;
    if (opts.permission_mode) body.permission_mode = opts.permission_mode;
    if (opts.model) body.model = opts.model;
    const data = await this.req('POST', `/api/projects/${enc(projectId)}/sessions`, body);
    return parseSessionInfo(data);
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

  async projects(): Promise<Project[]> {
    return parseProjectList(await this.req('GET', '/api/projects'));
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

  /** `GET /api/search` (SPEC §15): the user's sessions, newest hits first. */
  async search(q: string, opts: { fuzzy?: boolean; project?: string; cursor?: string } = {}): Promise<SearchPage> {
    const p = new URLSearchParams({ q });
    if (opts.fuzzy) p.set('fuzzy', '1');
    if (opts.project) p.set('project', opts.project);
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
    const page: EventPage = { events: [], invalid: [], maxSeq: from, minSeq: undefined, count: 0 };
    for (const raw of parseEventPage(data).events) {
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

  /** A signed link, valid for 10 minutes (SPEC §12). */
  async artifactLink(id: string): Promise<ArtifactUrl> {
    return parseArtifactUrl(await this.req('GET', `/api/artifacts/${enc(id)}/url`));
  }

  async stop(id: string): Promise<void> {
    await this.req('POST', `/api/sessions/${enc(id)}/stop`);
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

  async clarify(id: string, requestId: string, answers: Record<string, ClarifyAnswer>) {
    await this.req('POST', `/api/sessions/${enc(id)}/requests/${enc(requestId)}`, { answers });
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
