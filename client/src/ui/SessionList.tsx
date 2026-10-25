import { useEffect, useState } from 'preact/hooks';
import type { WorkerInfo } from '../api';
import type { AppState } from '../app';
import type { SessionInfo, Workspace } from '../proto';
import { useSubscription } from './hooks';
import { IconPlus, IconSearch } from './icons';
import { formatWhen, homePath } from './labels';
import { RequestsLink } from './requests';
import { SearchHits } from './Search';
import {
  LIST_MODES,
  activeLabel,
  bucketOf,
  groupSessions,
  headCounts,
  loadMode,
  matchesQuery,
  saveMode,
  subLabel,
  type ListMode,
} from './sessions';

/** "+ project": asks for the name, then opens the new project's workspaces — a new project
 *  has none yet, and that is the first thing to add. */
export async function newProject(app: AppState, onError: (m: string) => void): Promise<void> {
  const name = prompt('Project name', '');
  if (!name?.trim()) return;
  try {
    const p = await app.createProject(name.trim());
    location.hash = `#/p/${p.id}/workspaces`;
  } catch (e) {
    onError((e as Error).message);
  }
}

export function projectName(p: { name: string; is_personal: boolean }): string {
  return p.is_personal ? 'Personal' : p.name;
}

/** The sessions of the current project (SPEC §1: projects, in projects sessions), T26: a
 *  column next to the project rail on a desktop, the whole first screen on a phone, with
 *  the projects as chips above. */
export function SessionList({
  app,
  current,
  phone,
  requests,
}: {
  app: AppState;
  current?: string;
  phone?: boolean;
  /** The "Requests" screen is open (T37). */
  requests?: boolean;
}) {
  useSubscription((fn) => app.onChange(fn), [app]);
  const [creating, setCreating] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [mode, setMode] = useState<ListMode>(loadMode);
  // The phone searches on its own screen (`#/search`).
  const [query, setQuery] = useState('');
  const [showArchived, setShowArchived] = useState(false);
  const project = app.currentProject();
  const archived = app.archivedSessions();

  useEffect(() => {
    if (project && !app.workspaces.has(project.id)) app.loadWorkspaces(project.id).catch(() => {});
  }, [app, project?.id]);

  const pathOf = (s: SessionInfo) => homePath(app.sessionPath(s) ?? '');
  const all = showArchived ? archived : app.sortedSessions();
  const list = all.filter((s) => matchesQuery(s, pathOf(s), query));
  const now = Date.now();
  const groups = showArchived
    ? list.length
      ? [{ id: 'archive', label: 'Archive', items: list }]
      : []
    : groupSessions(list, mode, now);
  const pick = (m: ListMode) => {
    setMode(m);
    saveMode(m);
  };
  const conn = app.conn !== 'open' ? ' · offline' : '';
  const counts = headCounts(app.sortedSessions());

  const search = (
    <label class="search">
      <IconSearch size={14} />
      <input
        type="search"
        aria-label="Search sessions"
        placeholder="Search sessions and messages"
        value={query}
        onInput={(e) => setQuery((e.currentTarget as HTMLInputElement).value)}
      />
    </label>
  );
  const modes = (
    <div class="seg-group" role="group" aria-label="Session order">
      {LIST_MODES.map((m) => (
        <button
          type="button"
          key={m.id}
          class={`seg ${mode === m.id && !showArchived ? 'is-on' : ''}`}
          aria-pressed={mode === m.id && !showArchived}
          onClick={() => {
            setShowArchived(false);
            pick(m.id);
          }}
        >
          {m.label}
        </button>
      ))}
    </div>
  );

  return (
    <section class={`sessions ${phone ? 'sessions-phone' : ''}`}>
      {phone ? (
        <header class="m-head">
          <div class="m-head-row">
            <h1>Sessions</h1>
            <RequestsLink app={app} phone />
            <a class="icon-btn icon-btn-round" href="#/search" aria-label="Search sessions and messages">
              <IconSearch size={20} />
            </a>
            <a class="icon-btn icon-btn-round" href="#/settings" aria-label="Account and settings">
              <span class="avatar">{(app.me?.email ?? '?').slice(0, 1).toUpperCase()}</span>
            </a>
          </div>
          {(app.error || conn) && <div class="m-head-state">{app.error ? 'Hub unavailable' : 'offline'}</div>}
          <ProjectChips app={app} onError={setError} />
          {modes}
        </header>
      ) : (
        <>
          <header class="side-head">
            <div class="side-title">
              <div class="project-title">{project ? projectName(project) : 'hqc'}</div>
              <div class="side-counts">
                {counts}
                {conn}
              </div>
            </div>
            <button
              type="button"
              class="btn btn-primary btn-small btn-new-session"
              aria-label="New session"
              disabled={creating || !app.me}
              onClick={() => setCreating(true)}
            >
              <IconPlus size={14} />
              New
            </button>
          </header>
          <div class="side-tools">
            {search}
            {modes}
          </div>
        </>
      )}
      {app.error && <div class="notice">Hub unavailable: {app.error}</div>}
      {error && <div class="notice">{error}</div>}
      {creating && project && <NewSession key={project.id} app={app} projectId={project.id} onDone={() => setCreating(false)} />}
      <div class="session-scroll">
        {groups.map((g) => (
          <div key={g.id} class={`session-group group-${g.id}`}>
            <div class="group-head">
              <span class="group-label">{g.label}</span>
              <span class="group-count">{g.items.length}</span>
            </div>
            <ul class="session-list">
              {g.items.map((s) => (
                <SessionRow
                  key={s.id}
                  s={s}
                  current={s.id === current}
                  when={mode === 'recent' || showArchived ? formatWhen(s.updated_at, now) : undefined}
                  where={[app.workerName(s.worker_id), pathOf(s)].filter(Boolean).join(' · ')}
                />
              ))}
            </ul>
          </div>
        ))}
        {app.me && groups.length === 0 && (
          <div class="empty">{query ? 'No sessions match' : showArchived ? 'The archive is empty' : 'No sessions yet'}</div>
        )}
        {!phone && <SearchHits app={app} query={query} />}
        {project && (
          <div class="list-foot">
            <a href={`#/p/${project.id}/workspaces`}>Project workspaces</a>
            {(archived.length > 0 || showArchived) && (
              <button
                type="button"
                class={`btn-link btn-archive-toggle ${showArchived ? 'is-on' : ''}`}
                aria-pressed={showArchived}
                title="Show archived sessions"
                onClick={() => setShowArchived(!showArchived)}
              >
                {showArchived ? 'Hide archive' : `Archive ${archived.length}`}
              </button>
            )}
          </div>
        )}
        {phone && <WorkerList app={app} current={current} />}
      </div>
      {!phone && <RequestsLink app={app} current={requests} />}
      {!phone && <WorkerList app={app} current={current} />}
      {phone && (
        <button
          type="button"
          class="fab btn-new-session"
          aria-label="New session"
          disabled={creating || !app.me}
          onClick={() => setCreating(true)}
        >
          <IconPlus size={20} />
          New
        </button>
      )}
    </section>
  );
}

function SessionRow({ s, current, when, where }: { s: SessionInfo; current: boolean; when?: string; where: string }) {
  const bucket = bucketOf(s);
  return (
    <li>
      <a class={`session-row ${current ? 'is-current' : ''}`} href={`#/s/${s.id}`} aria-current={current ? 'page' : undefined}>
        <span class={`dot dot-${bucket} state-${s.state}`} aria-hidden="true" />
        <span class="session-main">
          <span class="session-top">
            <span class={`session-title ${bucket === 'idle' ? 'is-idle' : ''}`}>{s.title || s.slug || s.id.slice(-8)}</span>
            {when && <span class="session-when">{when}</span>}
            {s.unread > 0 && (
              <span class="badge" aria-label={`unread: ${s.unread}`}>
                {s.unread > 99 ? '99+' : s.unread}
              </span>
            )}
          </span>
          <span class={`session-sub state-${s.state}`}>{subLabel(s)}</span>
          {where && (
            <span class="session-where mono" title={where}>
              {where}
            </span>
          )}
        </span>
      </a>
    </li>
  );
}

/** Projects as chips on a phone (the rail's job on a desktop), with "needs you" counts. */
function ProjectChips({ app, onError }: { app: AppState; onError: (m: string) => void }) {
  const current = app.currentProjectId;
  return (
    <nav class="chips" aria-label="Projects">
      {app.sortedProjects().map((p) => {
        const waiting = app.projectWaiting(p.id);
        return (
          <a
            key={p.id}
            class={`chip project-row ${p.id === current ? 'is-current' : ''}`}
            href={`#/p/${p.id}`}
            aria-label={projectName(p)}
            aria-current={p.id === current ? 'true' : undefined}
          >
            {projectName(p)}
            {waiting > 0 && (
              <span class="chip-badge" aria-label={`needs you: ${waiting}`}>
                {waiting}
              </span>
            )}
          </a>
        );
      })}
      <button type="button" class="chip chip-new" aria-label="New project" disabled={!app.me} onClick={() => void newProject(app, onError)}>
        <IconPlus />
      </button>
    </nav>
  );
}

/** "Workers": each worker with its online dot and how many sessions are active on it; a
 *  click opens its service session (SPEC §10.2). Listed by their service sessions:
 *  revoking a worker archives it, so revoked workers are not here. */
function WorkerList({ app, current }: { app: AppState; current?: string }) {
  const list = app.workerSessions();
  if (list.length === 0) return null;
  return (
    <nav class="worker-list" aria-label="Workers">
      <div class="group-title">Workers</div>
      {list.map((s) => {
        const offline = s.state === 'worker_offline';
        const asks = s.state === 'waiting';
        return (
          <a key={s.id} class={`worker-row ${s.id === current ? 'is-current' : ''}`} href={`#/s/${s.id}`} title="The worker's service session">
            <span class={`dot ${offline ? 'dot-offline' : 'dot-online'}`} aria-hidden="true" />
            <span class="worker-name">{s.title}</span>
            {asks ? (
              <span class="worker-asks">waiting for reply</span>
            ) : (
              <span class="worker-state">{offline ? 'offline' : activeLabel(app.workerActive(s.worker_id))}</span>
            )}
            {s.unread > 0 && (
              <span class="badge" aria-label={`unread: ${s.unread}`}>
                {s.unread > 99 ? '99+' : s.unread}
              </span>
            )}
          </a>
        );
      })}
    </nav>
  );
}

/** `label` or the folder's name, then where it is. */
export function workspaceTitle(w: Workspace, workers?: WorkerInfo[]): string {
  const name = w.label || w.path.split('/').filter(Boolean).pop() || w.path;
  const worker = workers?.find((x) => x.id === w.worker_id)?.name;
  return `${name} — ${worker ? `${worker}:` : ''}${w.path}`;
}

/** New session: title (empty — "Session N", SPEC §3), workspace (the project's default
 *  preselected, SPEC §13.4) and an optional `cwd` inside it. */
function NewSession({ app, projectId, onDone }: { app: AppState; projectId: string; onDone: () => void }) {
  const [list, setList] = useState<Workspace[] | null>(app.workspaces.get(projectId) ?? null);
  const [workers, setWorkers] = useState<WorkerInfo[] | undefined>();
  const [title, setTitle] = useState('');
  const [workspace, setWorkspace] = useState('');
  const [cwd, setCwd] = useState('');
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const project = app.projects.get(projectId);

  useEffect(() => {
    app.loadWorkspaces(projectId).then(setList, (e: Error) => setError(e.message));
    app.api.workers().then(setWorkers, () => {});
  }, [app, projectId]);
  const chosen = workspace || project?.default_workspace_id || list?.[0]?.id || '';

  const create = async (e: Event) => {
    e.preventDefault();
    setBusy(true);
    setError(null);
    try {
      const s = await app.createSession(title.trim() || undefined, { workspace_id: chosen, cwd: cwd.trim() }, projectId);
      onDone();
      location.hash = `#/s/${s.id}`;
    } catch (err) {
      setError((err as Error).message);
    } finally {
      setBusy(false);
    }
  };

  if (list && list.length === 0) {
    return (
      <div class="new-session">
        <p class="muted">
          The project has no workspaces: a session needs a folder on a worker.{' '}
          <a href={`#/p/${projectId}/workspaces`}>Add a workspace</a>
        </p>
        <button type="button" class="btn btn-small" onClick={onDone}>
          Cancel
        </button>
      </div>
    );
  }
  return (
    <form class="new-session" onSubmit={create}>
      <label class="field">
        <span class="field-label">Title (optional)</span>
        <input class="input" value={title} onInput={(e) => setTitle((e.target as HTMLInputElement).value)} />
      </label>
      <label class="field">
        <span class="field-label">Workspace</span>
        <select
          class="input"
          name="workspace"
          value={chosen}
          disabled={!list}
          onChange={(e) => setWorkspace((e.target as HTMLSelectElement).value)}
        >
          {(list ?? []).map((w) => (
            <option key={w.id} value={w.id}>
              {workspaceTitle(w, workers)}
              {w.id === project?.default_workspace_id ? ' (default)' : ''}
            </option>
          ))}
        </select>
      </label>
      <label class="field">
        <span class="field-label">Folder inside the workspace (optional)</span>
        <input
          class="input"
          name="cwd"
          placeholder="."
          value={cwd}
          onInput={(e) => setCwd((e.target as HTMLInputElement).value)}
        />
      </label>
      {error && <div class="error-text">{error}</div>}
      <div class="row">
        <button type="submit" class="btn btn-small btn-primary" disabled={busy || !chosen}>
          Create
        </button>
        <button type="button" class="btn btn-small" onClick={onDone}>
          Cancel
        </button>
      </div>
    </form>
  );
}
