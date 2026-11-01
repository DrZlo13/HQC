import { useEffect, useState } from 'preact/hooks';
import { ApiError, type WorkerInfo } from '../api';
import type { AppState } from '../app';
import { parseWorkspaceConflict, type Candidate, type DirEntry, type Workspace } from '../proto';
import { THEMES, colorLabel } from '../theme';
import { AppearanceDialog, ProjectGlyph, projectName } from './Appearance';
import { useSubscription } from './hooks';
import { IconBack, IconPlus, IconShield } from './icons';
import { homePath } from './labels';

/** The workspace's label or its folder's name. */
export function workspaceName(w: Workspace): string {
  return w.label || w.path.split('/').filter(Boolean).pop() || w.path;
}

/** The name, then where it is: the choice of the new-session form. */
export function workspaceTitle(w: Workspace, workers?: WorkerInfo[]): string {
  const worker = workers?.find((x) => x.id === w.worker_id)?.name;
  return `${workspaceName(w)} — ${worker ? `${worker}:` : ''}${w.path}`;
}

/** Why a workspace could not be added, as the user reads it. */
function addError(e: unknown, app: AppState): { text: string; link?: { href: string; label: string } } {
  if (e instanceof ApiError) {
    const c = parseWorkspaceConflict(e.body);
    if (c) {
      const p = app.projects.get(c.existing_project_id);
      const where = p ? `project “${projectName(p)}”` : 'another project';
      return {
        text: `This folder is already in ${where}: ${c.existing_path}. A folder belongs to one project, and so do its subfolders.`,
        link: { href: `#/p/${c.existing_project_id}/settings`, label: 'Open the project' },
      };
    }
    if (e.code === 'worker_offline') {
      return { text: 'Worker offline: the worker checks the path itself, so the folder can be added once it is connected.' };
    }
  }
  return { text: (e as Error).message };
}

/** Project settings (T49, `ProjectSettings.dc.html`, `MSettings2.dc.html`), opened with the
 *  gear of the sidebar header: Appearance (icon, color, the app theme), Workspaces (SPEC §3,
 *  §13: list, add, import, make default, "VS Code sessions", remove the record) and Project
 *  (rename, delete; neither for the personal project, SPEC §3). */
export function ProjectSettings({ app, projectId, phone }: { app: AppState; projectId: string; phone?: boolean }) {
  useSubscription((fn) => app.onChange(fn), [app]);
  const [workers, setWorkers] = useState<WorkerInfo[] | null>(null);
  const [adding, setAdding] = useState(false);
  const [importing, setImporting] = useState(false);
  const [appearance, setAppearance] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const project = app.projects.get(projectId);
  const list = app.workspaces.get(projectId);

  useEffect(() => {
    app.loadWorkspaces(projectId).catch((e: Error) => setError(e.message));
    app.api.workers().then(setWorkers, () => setWorkers([]));
  }, [app, projectId]);

  const remove = async (w: Workspace) => {
    if (!confirm(`Remove workspace ${w.path} from the project? The folder on the worker stays.`)) return;
    setError(null);
    try {
      await app.api.deleteWorkspace(w.id);
      await app.workspacesChanged(projectId);
    } catch (e) {
      const busy = e instanceof ApiError && e.code === 'workspace_in_use';
      setError(busy ? `${e.message}. Archive these sessions first.` : (e as Error).message);
    }
  };

  // "VS Code sessions" (T21, SPEC §9.2): the worker mirrors the folder's Claude Code sessions.
  const toggleMirror = async (w: Workspace, on: boolean) => {
    setError(null);
    try {
      await app.api.patchWorkspace(w.id, { mirror_enabled: on });
      await app.workspacesChanged(projectId);
    } catch (e) {
      setError((e as Error).message);
    }
  };

  const makeDefault = (w: Workspace) =>
    app.setDefaultWorkspace(projectId, w.id).catch((e: Error) => setError(e.message));

  const rename = async () => {
    if (!project) return;
    const name = prompt('New project name', project.name);
    if (!name?.trim() || name.trim() === project.name) return;
    setError(null);
    try {
      await app.renameProject(projectId, name.trim());
    } catch (e) {
      setError((e as Error).message);
    }
  };

  const deleteProject = async () => {
    if (!project) return;
    const n = [...app.sessions.values()].filter((s) => s.project_id === projectId).length;
    if (!confirm(`Delete project “${project.name}” and its sessions (${n})? The folders on the workers stay.`)) return;
    try {
      await app.deleteProject(projectId);
      location.hash = '#/';
    } catch (e) {
      setError((e as Error).message);
    }
  };

  const name = project ? projectName(project) : '';
  const theme = THEMES.find((t) => t.id === app.theme())?.label ?? 'Warm';
  const look = project && (
    <>
      <span class="ps-tile">
        <ProjectGlyph p={project} />
      </span>
      <span class="ps-look-text">
        <span class="ps-name">{name}</span>
        <span class="ps-color">
          <span class="ps-dot" />
          {colorLabel(project.color)} · {phone ? theme : `${theme} theme`}
        </span>
      </span>
    </>
  );
  return (
    <section class={`page project-settings ${phone ? 'is-phone' : ''}`}>
      <header class="bar">
        <a class="back icon-btn" href={`#/p/${projectId}`} aria-label="Back to sessions" title="Back">
          <IconBack />
        </a>
        <div class="bar-title">
          <h1 class="title">Project settings</h1>
          <div class="state">{name}</div>
        </div>
      </header>
      <div class="page-scroll">
        <div class="page-body">
          <section class="ps-section" aria-labelledby="ps-look">
            <h2 id="ps-look">Appearance</h2>
            {project &&
              (phone ? (
                <button type="button" class="ps-look ps-card btn-appearance" onClick={() => setAppearance(true)}>
                  {look}
                  <span class="ps-change">Change</span>
                </button>
              ) : (
                <div class="ps-look ps-card">
                  {look}
                  <button type="button" class="btn btn-appearance" onClick={() => setAppearance(true)}>
                    Change icon and color
                  </button>
                </div>
              ))}
          </section>

          <section class="ps-section workspaces" aria-labelledby="ps-ws">
            <h2 id="ps-ws">Workspaces</h2>
            <p class="ps-text">
              Folders on workers that this project’s sessions work in. A folder belongs to one project. Removing one
              deletes only the record: the folder on disk stays.
            </p>
            {list && list.length === 0 && <p class="empty-inline">No workspaces yet.</p>}
            {list?.map((w) => {
              const isDefault = w.id === project?.default_workspace_id;
              const worker = workers?.find((x) => x.id === w.worker_id)?.name;
              return (
                <div key={w.id} class="workspace-row ps-card">
                  <div class="workspace-main">
                    <span class="workspace-name">{workspaceName(w)}</span>
                    <span class="workspace-path mono">
                      {worker ? `${worker}:` : ''}
                      {homePath(w.path)}
                    </span>
                    {isDefault && <span class="tag">default</span>}
                    {w.environment !== 'default' && (
                      <span class="tag workspace-env" title="Environment from the worker’s worker.toml">
                        env {w.environment}
                      </span>
                    )}
                  </div>
                  <div class="workspace-actions">
                    <label
                      class="mirror-toggle"
                      title="Show this folder's Claude Code sessions (VS Code, terminal) in the project and take them over"
                    >
                      <input
                        type="checkbox"
                        name="mirror"
                        checked={w.mirror_enabled}
                        onChange={(e) => toggleMirror(w, (e.currentTarget as HTMLInputElement).checked)}
                      />
                      <span>VS Code sessions</span>
                    </label>
                    <a class="btn btn-small btn-rules" href={`#/p/${projectId}/rules/${w.id}`} title="The folder's permission rules">
                      <IconShield />
                      Rules · {w.hqc_rules} HQC
                    </a>
                    {!isDefault && (
                      <button type="button" class="btn btn-small btn-make-default" onClick={() => makeDefault(w)}>
                        Make default
                      </button>
                    )}
                    <span class="spacer" />
                    <button type="button" class="btn btn-small btn-ghost btn-remove-workspace" onClick={() => remove(w)}>
                      Remove
                    </button>
                  </div>
                </div>
              );
            })}
            {error && <div class="error-text">{error}</div>}
            {adding ? (
              <AddWorkspace app={app} projectId={projectId} workers={workers ?? []} onDone={() => setAdding(false)} />
            ) : importing ? (
              <ImportFromClaude app={app} projectId={projectId} workers={workers ?? []} onDone={() => setImporting(false)} />
            ) : (
              <div class="row ps-buttons">
                <button type="button" class="btn btn-primary btn-add-workspace" onClick={() => setAdding(true)}>
                  <IconPlus size={15} />
                  Add a workspace
                </button>
                <button type="button" class="btn btn-import-claude" onClick={() => setImporting(true)}>
                  Import from Claude Code
                </button>
              </div>
            )}
          </section>

          {/* The personal project keeps its name and is not deleted (SPEC §3 Project). */}
          {project && !project.is_personal && (
            <section class="ps-section" aria-labelledby="ps-project">
              <h2 id="ps-project">Project</h2>
              <div class="row ps-buttons">
                <button type="button" class="btn btn-rename-project" onClick={rename}>
                  Rename
                </button>
                <button type="button" class="btn btn-delete-project" onClick={deleteProject}>
                  Delete project
                </button>
              </div>
            </section>
          )}
        </div>
      </div>
      {appearance && project && (
        <AppearanceDialog app={app} project={project} phone={phone} onClose={() => setAppearance(false)} />
      )}
    </section>
  );
}

/** The import from Claude Code (SPEC §9.1): folders the worker found Claude Code sessions
 *  in (by the `cwd` of their transcripts); each picked one becomes a workspace of this
 *  project with the mirror on, and its sessions with their history show up in the project. */
function ImportFromClaude({
  app,
  projectId,
  workers,
  onDone,
}: {
  app: AppState;
  projectId: string;
  workers: WorkerInfo[];
  onDone: () => void;
}) {
  const online = workers.filter((w) => w.status === 'online');
  const [worker, setWorker] = useState(online[0]?.id ?? workers[0]?.id ?? '');
  const [list, setList] = useState<Candidate[] | null>(null);
  const [picked, setPicked] = useState<Set<string>>(new Set());
  const [loadError, setLoadError] = useState<string | null>(null);
  const [errors, setErrors] = useState<{ path: string; error: ReturnType<typeof addError> }[]>([]);
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    if (!worker && workers[0]) setWorker(online[0]?.id ?? workers[0].id);
  }, [workers]);

  useEffect(() => {
    if (!worker) return;
    setList(null);
    setLoadError(null);
    setPicked(new Set());
    app.api.candidates(worker).then(setList, (e) => {
      setList([]);
      setLoadError(addError(e, app).text);
    });
  }, [app, worker]);

  const toggle = (path: string) => {
    const next = new Set(picked);
    if (next.has(path)) next.delete(path);
    else next.add(path);
    setPicked(next);
  };

  const run = async () => {
    setBusy(true);
    setErrors([]);
    const failed: typeof errors = [];
    for (const path of picked) {
      try {
        await app.api.createWorkspace({ project_id: projectId, worker_id: worker, path, mirror_enabled: true });
      } catch (e) {
        failed.push({ path, error: addError(e, app) });
      }
    }
    await app.workspacesChanged(projectId).catch(() => {});
    // The worker sends the history in the background; sessions appear as it arrives.
    await app.refreshSessions().catch(() => {});
    setBusy(false);
    if (failed.length) setErrors(failed);
    else onDone();
  };

  if (workers.length === 0) {
    return (
      <div class="import-claude">
        <p class="muted">
          No workers yet. <a href="#/settings">Enroll a machine</a>.
        </p>
        <button type="button" class="btn btn-small" onClick={onDone}>
          Cancel
        </button>
      </div>
    );
  }
  return (
    <div class="import-claude">
      <p class="muted small">
        Folders on the worker that have Claude Code sessions (VS Code or terminal). The picked ones become workspaces of
        this project: their sessions show up here with their history, and new records keep coming in.
      </p>
      <label class="field">
        <span class="muted small">Worker</span>
        <select class="input" name="worker" value={worker} onChange={(e) => setWorker((e.target as HTMLSelectElement).value)}>
          {workers.map((w) => (
            <option key={w.id} value={w.id}>
              {w.name}
              {w.status === 'online' ? '' : ' (offline)'}
            </option>
          ))}
        </select>
      </label>
      {loadError && <div class="error-text">{loadError}</div>}
      {list === null && !loadError && <div class="muted small">…</div>}
      {list && list.length === 0 && !loadError && <div class="muted small">No Claude Code sessions found</div>}
      {list && list.length > 0 && (
        <ul class="candidate-list">
          {list.map((c) => (
            <li key={c.path}>
              <label class="candidate">
                <input type="checkbox" checked={picked.has(c.path)} onChange={() => toggle(c.path)} />
                <span class="candidate-path">{c.path}</span>
                <span class="muted small">
                  {' '}
                  · sessions: {c.sessions_count} · {new Date(c.last_active).toLocaleString('en-US', { hourCycle: 'h23' })}
                </span>
              </label>
            </li>
          ))}
        </ul>
      )}
      {errors.map((e) => (
        <div key={e.path} class="error-text">
          {e.path}: {e.error.text} {e.error.link && <a href={e.error.link.href}>{e.error.link.label}</a>}
        </div>
      ))}
      <div class="row">
        <button type="button" class="btn btn-small btn-primary btn-run-import" disabled={busy || picked.size === 0} onClick={run}>
          Import
        </button>
        <button type="button" class="btn btn-small" onClick={onDone}>
          Cancel
        </button>
      </div>
    </div>
  );
}

/** Pick a worker, then a folder: from the tree the worker lists (`fs.list_dirs`), or typed
 *  by hand. The worker checks it again when it is added (`fs.stat`). */
function AddWorkspace({
  app,
  projectId,
  workers,
  onDone,
}: {
  app: AppState;
  projectId: string;
  workers: WorkerInfo[];
  onDone: () => void;
}) {
  const online = workers.filter((w) => w.status === 'online');
  const [worker, setWorker] = useState(online[0]?.id ?? workers[0]?.id ?? '');
  // `null`: the worker's allowed_roots.
  const [dir, setDir] = useState<string | null>(null);
  const [entries, setEntries] = useState<DirEntry[] | null>(null);
  const [roots, setRoots] = useState<string[]>([]);
  const [browseError, setBrowseError] = useState<string | null>(null);
  const [path, setPath] = useState('');
  const [label, setLabel] = useState('');
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<ReturnType<typeof addError> | null>(null);

  useEffect(() => {
    if (!worker && workers[0]) setWorker(online[0]?.id ?? workers[0].id);
  }, [workers]);

  useEffect(() => {
    if (!worker) return;
    setEntries(null);
    setBrowseError(null);
    app.api.workerDirs(worker, dir ?? undefined).then((list) => {
      setEntries(list);
      if (dir === null) setRoots(list.map((d) => d.path));
    }, (e) => {
      setEntries([]);
      setBrowseError(addError(e, app).text);
    });
  }, [app, worker, dir]);

  const parent = dir === null ? null : dir.replace(/\/[^/]+\/?$/, '') || '/';
  const add = async (e: Event) => {
    e.preventDefault();
    setBusy(true);
    setError(null);
    try {
      await app.api.createWorkspace({ project_id: projectId, worker_id: worker, path: path.trim(), label: label.trim() || undefined });
      await app.workspacesChanged(projectId);
      onDone();
    } catch (err) {
      setError(addError(err, app));
    } finally {
      setBusy(false);
    }
  };

  if (workers.length === 0) {
    return (
      <div class="add-workspace">
        <p class="muted">
          No workers yet. <a href="#/settings">Enroll a machine</a> to pick a folder on it.
        </p>
        <button type="button" class="btn btn-small" onClick={onDone}>
          Cancel
        </button>
      </div>
    );
  }
  return (
    <form class="add-workspace" onSubmit={add}>
      <label class="field">
        <span class="muted small">Worker</span>
        <select
          class="input"
          name="worker"
          value={worker}
          onChange={(e) => {
            setWorker((e.target as HTMLSelectElement).value);
            setDir(null);
          }}
        >
          {workers.map((w) => (
            <option key={w.id} value={w.id}>
              {w.name}
              {w.status === 'online' ? '' : ' (offline)'}
            </option>
          ))}
        </select>
      </label>
      <div class="dir-browser">
        <div class="dir-path">
          <span class="muted small">{dir ?? "The worker's allowed roots"}</span>
          {dir !== null && (
            <button type="button" class="btn btn-small btn-ghost btn-dir-up" onClick={() => setDir(roots.includes(dir) ? null : parent)}>
              ↑ Up
            </button>
          )}
          {dir !== null && (
            <button type="button" class="btn btn-small btn-pick-dir" onClick={() => setPath(dir)}>
              Pick this folder
            </button>
          )}
        </div>
        {browseError && <div class="error-text">{browseError}</div>}
        {entries === null && !browseError && <div class="muted small">…</div>}
        {entries && entries.length === 0 && !browseError && <div class="muted small">No subfolders</div>}
        {entries && entries.length > 0 && (
          <ul class="dir-list">
            {entries.map((d) => (
              <li key={d.path}>
                <button type="button" class="dir-entry" onClick={() => setDir(d.path)} onDblClick={() => setPath(d.path)}>
                  📁 {d.name}
                </button>
              </li>
            ))}
          </ul>
        )}
      </div>
      <label class="field">
        <span class="muted small">Path on the worker</span>
        <input
          class="input"
          name="path"
          placeholder="/Users/me/work/project"
          value={path}
          onInput={(e) => setPath((e.target as HTMLInputElement).value)}
        />
      </label>
      <label class="field">
        <span class="muted small">Label (optional)</span>
        <input class="input" name="label" value={label} onInput={(e) => setLabel((e.target as HTMLInputElement).value)} />
      </label>
      {error && (
        <div class="error-text">
          {error.text} {error.link && <a href={error.link.href}>{error.link.label}</a>}
        </div>
      )}
      <div class="row">
        <button type="submit" class="btn btn-small btn-primary" disabled={busy || !worker || !path.trim()}>
          Add
        </button>
        <button type="button" class="btn btn-small" onClick={onDone}>
          Cancel
        </button>
      </div>
    </form>
  );
}
