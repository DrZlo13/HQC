import { useEffect, useState } from 'preact/hooks';
import { ApiError, type WorkerInfo } from '../api';
import type { AppState } from '../app';
import {
  parseWorkspaceConflict,
  type Candidate,
  type DirEntry,
  type Workspace,
  type WorkspaceInspection,
} from '../proto';
import { THEMES, colorLabel } from '../theme';
import { AppearanceDialog, ProjectGlyph, projectName } from './Appearance';
import { useSubscription } from './hooks';
import { IconBack, IconCross, IconPlus, IconShield } from './icons';
import { homePath } from './labels';

/** How often the list is read again while a clone runs (T45). */
const CLONE_POLL_MS = 1000;

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

/** The folder name a clone of `url` gets by default, as `git clone` names it. */
export function cloneName(url: string): string {
  const last = url.trim().replace(/\/+$/, '').split(/[/:]/).pop() ?? '';
  return last.replace(/\.git$/, '');
}

/** Where the hub puts a worktree without a path (T46): next to its origin,
 *  `<origin-name>-<branch>` with the branch's slashes as `-`. */
export function worktreePath(origin: string, branch: string): string {
  return branch ? `${origin.replace(/\/+$/, '')}-${branch.replace(/\//g, '-')}` : '';
}

/** The list with each worktree right under its origin (T46), in the hub's order otherwise. */
export function underOrigins(list: Workspace[]): Workspace[] {
  const ids = new Set(list.map((w) => w.id));
  const originOf = (w: Workspace) => (w.kind === 'worktree' && w.origin_workspace_id && ids.has(w.origin_workspace_id) ? w.origin_workspace_id : null);
  return list.filter((w) => !originOf(w)).flatMap((w) => [w, ...list.filter((x) => originOf(x) === w.id)]);
}

/** What the row says while the worker makes the folder, and when it could not (T45, T46). */
function making(w: Workspace): { tag: string; failed: string; note: string } {
  return w.kind === 'worktree'
    ? {
        tag: 'Adding worktree…',
        failed: 'Worktree failed',
        note: 'The worker is adding the worktree; sessions can start once it is done.',
      }
    : {
        tag: 'Cloning…',
        failed: 'Clone failed',
        note: 'The worker is cloning the repository; sessions can start once it is done.',
      };
}

/** Project settings (T49, `ProjectSettings.dc.html`, `MSettings2.dc.html`), opened with the
 *  gear of the sidebar header: Appearance (icon, color, the app theme), Workspaces (SPEC §3,
 *  §13: list, add, import, make default, "VS Code sessions", remove the record) and Project
 *  (rename, delete; neither for the personal project, SPEC §3). */
export function ProjectSettings({ app, projectId, phone }: { app: AppState; projectId: string; phone?: boolean }) {
  useSubscription((fn) => app.onChange(fn), [app]);
  const [workers, setWorkers] = useState<WorkerInfo[] | null>(null);
  const [adding, setAdding] = useState(false);
  const [cloning, setCloning] = useState(false);
  const [importing, setImporting] = useState(false);
  const [deleting, setDeleting] = useState<Workspace | null>(null);
  // The workspace whose "New worktree" form is open (T46).
  const [branching, setBranching] = useState<string | null>(null);
  const [appearance, setAppearance] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const project = app.projects.get(projectId);
  const list = app.workspaces.get(projectId);

  useEffect(() => {
    app.loadWorkspaces(projectId).catch((e: Error) => setError(e.message));
    app.api.workers().then(setWorkers, () => setWorkers([]));
  }, [app, projectId]);

  // A clone or a worktree is made on the worker (T45, T46): its row follows until it is ready
  // or failed.
  const creating = !!list?.some((w) => w.state === 'creating');
  useEffect(() => {
    if (!creating) return;
    const t = setInterval(() => app.loadWorkspaces(projectId).catch(() => {}), CLONE_POLL_MS);
    return () => clearInterval(t);
  }, [app, projectId, creating]);

  const remove = async (w: Workspace) => {
    // A folder hqc made: the dialog says what deleting it would lose (T45).
    if (w.created_by_hqc && w.state === 'ready') {
      setDeleting(w);
      return;
    }
    const what =
      w.state === 'failed'
        ? `Remove the failed ${w.kind === 'worktree' ? 'worktree' : 'clone'} ${w.path} from the project?`
        : `Remove workspace ${w.path} from the project? The folder on the worker stays.`;
    if (!confirm(what)) return;
    setError(null);
    try {
      await app.api.deleteWorkspace(w.id);
      await app.workspacesChanged(projectId);
    } catch (e) {
      // `workspace_in_use` says what holds it: sessions to archive or worktrees to delete.
      setError((e as Error).message);
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
              deletes only the record: the folder on disk stays, unless hqc made it (a clone or a worktree) and you delete
              it too.
            </p>
            {list && list.length === 0 && <p class="empty-inline">No workspaces yet.</p>}
            {list &&
              underOrigins(list).map((w) => {
              const isDefault = w.id === project?.default_workspace_id;
              const worker = workers?.find((x) => x.id === w.worker_id)?.name;
              const ready = w.state === 'ready';
              const worktree = w.kind === 'worktree';
              const origin = worktree ? list.find((x) => x.id === w.origin_workspace_id) : undefined;
              const text = making(w);
              return (
                <div key={w.id} class={`workspace-row ps-card ${ready ? '' : `is-${w.state}`} ${worktree ? 'is-worktree' : ''}`}>
                  <div class="workspace-main">
                    <span class="workspace-name">{workspaceName(w)}</span>
                    <span class="workspace-path mono">
                      {worker ? `${worker}:` : ''}
                      {homePath(w.path)}
                    </span>
                    {worktree && w.branch && (
                      <span class="tag workspace-branch mono" title={origin ? `Worktree of ${workspaceName(origin)}` : 'Worktree'}>
                        {w.branch}
                      </span>
                    )}
                    {w.ephemeral && (
                      <span class="tag workspace-ephemeral" title="hqc_spawn made this worktree for the session it started">
                        spawned
                      </span>
                    )}
                    {w.marked_for_deletion && <span class="tag tag-failed workspace-marked">for deletion</span>}
                    {isDefault && <span class="tag">default</span>}
                    {w.environment !== 'default' && (
                      <span class="tag workspace-env" title="Environment from the worker’s worker.toml">
                        env {w.environment}
                      </span>
                    )}
                    {w.state === 'creating' && <span class="tag tag-progress workspace-progress">{text.tag}</span>}
                    {w.state === 'failed' && <span class="tag tag-failed">{text.failed}</span>}
                  </div>
                  {w.state === 'failed' && <div class="error-text workspace-error">{w.error ?? 'The worker could not make it.'}</div>}
                  {w.marked_for_deletion && (
                    <div class="muted small workspace-note">
                      Its sessions are archived. hqc keeps the worktree and its branch until you delete it.
                    </div>
                  )}
                  {w.state === 'creating' ? (
                    <div class="muted small workspace-note">{text.note}</div>
                  ) : (
                    <div class="workspace-actions">
                      {ready && (
                        <>
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
                          {!worktree && (
                            <button
                              type="button"
                              class="btn btn-small btn-new-worktree"
                              title="A git worktree of this folder on another branch, as a workspace of its own"
                              onClick={() => setBranching(branching === w.id ? null : w.id)}
                            >
                              <IconPlus size={13} />
                              New worktree
                            </button>
                          )}
                        </>
                      )}
                      <span class="spacer" />
                      <button
                        type="button"
                        class={`btn btn-small btn-remove-workspace ${w.marked_for_deletion ? 'btn-danger-fill' : 'btn-ghost'}`}
                        onClick={() => remove(w)}
                      >
                        {w.marked_for_deletion ? 'Delete' : 'Remove'}
                      </button>
                    </div>
                  )}
                  {branching === w.id && ready && (
                    <NewWorktree app={app} projectId={projectId} origin={w} onDone={() => setBranching(null)} />
                  )}
                </div>
              );
            })}
            {error && <div class="error-text">{error}</div>}
            {adding ? (
              <AddWorkspace app={app} projectId={projectId} workers={workers ?? []} onDone={() => setAdding(false)} />
            ) : cloning ? (
              <CloneRepository app={app} projectId={projectId} workers={workers ?? []} onDone={() => setCloning(false)} />
            ) : importing ? (
              <ImportFromClaude app={app} projectId={projectId} workers={workers ?? []} onDone={() => setImporting(false)} />
            ) : (
              <div class="row ps-buttons">
                <button type="button" class="btn btn-primary btn-add-workspace" onClick={() => setAdding(true)}>
                  <IconPlus size={15} />
                  Add a workspace
                </button>
                <button type="button" class="btn btn-clone" onClick={() => setCloning(true)}>
                  Clone a repository
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
      {deleting && (
        <DeleteFolderDialog
          key={deleting.id}
          app={app}
          workspace={deleting}
          origin={list?.find((x) => x.id === deleting.origin_workspace_id)}
          worker={workers?.find((x) => x.id === deleting.worker_id)?.name}
          phone={phone}
          onClose={() => setDeleting(null)}
          onDone={() => {
            setDeleting(null);
            app.workspacesChanged(projectId).catch((e: Error) => setError(e.message));
          }}
        />
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

/** The worker's folders (`fs.list_dirs`), from its allowed_roots down; "Pick this folder"
 *  (or a double click on one) hands a path to the form. Mount it with `key={worker}`. */
function FolderTree({ app, worker, onPick }: { app: AppState; worker: string; onPick: (path: string) => void }) {
  // `null`: the worker's allowed_roots.
  const [dir, setDir] = useState<string | null>(null);
  const [entries, setEntries] = useState<DirEntry[] | null>(null);
  const [roots, setRoots] = useState<string[]>([]);
  const [browseError, setBrowseError] = useState<string | null>(null);

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
  return (
    <div class="dir-browser">
      <div class="dir-path">
        <span class="muted small">{dir ?? "The worker's allowed roots"}</span>
        {dir !== null && (
          <button type="button" class="btn btn-small btn-ghost btn-dir-up" onClick={() => setDir(roots.includes(dir) ? null : parent)}>
            ↑ Up
          </button>
        )}
        {dir !== null && (
          <button type="button" class="btn btn-small btn-pick-dir" onClick={() => onPick(dir)}>
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
              <button type="button" class="dir-entry" onClick={() => setDir(d.path)} onDblClick={() => onPick(d.path)}>
                📁 {d.name}
              </button>
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}

/** The worker select of the add and clone forms. */
function WorkerSelect({ workers, value, onChange }: { workers: WorkerInfo[]; value: string; onChange: (id: string) => void }) {
  return (
    <label class="field">
      <span class="muted small">Worker</span>
      <select class="input" name="worker" value={value} onChange={(e) => onChange((e.target as HTMLSelectElement).value)}>
        {workers.map((w) => (
          <option key={w.id} value={w.id}>
            {w.name}
            {w.status === 'online' ? '' : ' (offline)'}
          </option>
        ))}
      </select>
    </label>
  );
}

/** No workers to pick a folder on: a link to enroll one. */
function NoWorkers({ cls, onDone }: { cls: string; onDone: () => void }) {
  return (
    <div class={cls}>
      <p class="muted">
        No workers yet. <a href="#/settings">Enroll a machine</a> to pick a folder on it.
      </p>
      <button type="button" class="btn btn-small" onClick={onDone}>
        Cancel
      </button>
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
  const [path, setPath] = useState('');
  const [label, setLabel] = useState('');
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<ReturnType<typeof addError> | null>(null);

  useEffect(() => {
    if (!worker && workers[0]) setWorker(online[0]?.id ?? workers[0].id);
  }, [workers]);

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

  if (workers.length === 0) return <NoWorkers cls="add-workspace" onDone={onDone} />;
  return (
    <form class="add-workspace" onSubmit={add}>
      <WorkerSelect workers={workers} value={worker} onChange={setWorker} />
      <FolderTree key={worker} app={app} worker={worker} onPick={setPath} />
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

/** "Clone a repository" (T45, SPEC §13.1): the URL, the parent folder from the tree (or
 *  typed) and the new folder's name, by default the repository's. The worker clones into
 *  the new folder in the environment of its path; the row shows the clone until it is
 *  ready or failed. */
function CloneRepository({
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
  const [url, setUrl] = useState('');
  const [parent, setParent] = useState('');
  // `null`: the repository's name, following the URL.
  const [name, setName] = useState<string | null>(null);
  const [branch, setBranch] = useState('');
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<ReturnType<typeof addError> | null>(null);

  useEffect(() => {
    if (!worker && workers[0]) setWorker(online[0]?.id ?? workers[0].id);
  }, [workers]);

  const folder = (name ?? cloneName(url)).trim();
  const path = parent.trim() && folder ? `${parent.trim().replace(/\/+$/, '')}/${folder}` : '';
  const clone = async (e: Event) => {
    e.preventDefault();
    setBusy(true);
    setError(null);
    try {
      await app.api.createWorkspace({
        kind: 'clone',
        project_id: projectId,
        worker_id: worker,
        path,
        url: url.trim(),
        branch: branch.trim() || undefined,
      });
      await app.workspacesChanged(projectId);
      onDone();
    } catch (err) {
      setError(addError(err, app));
    } finally {
      setBusy(false);
    }
  };

  if (workers.length === 0) return <NoWorkers cls="add-workspace clone-repository" onDone={onDone} />;
  return (
    <form class="add-workspace clone-repository" onSubmit={clone}>
      <WorkerSelect workers={workers} value={worker} onChange={setWorker} />
      <label class="field">
        <span class="muted small">Repository URL</span>
        <input
          class="input mono"
          name="url"
          placeholder="git@github.com:me/project.git"
          autocomplete="off"
          value={url}
          onInput={(e) => setUrl((e.target as HTMLInputElement).value)}
        />
      </label>
      <FolderTree key={worker} app={app} worker={worker} onPick={setParent} />
      <label class="field">
        <span class="muted small">Parent folder on the worker</span>
        <input
          class="input"
          name="parent"
          placeholder="/Users/me/work"
          value={parent}
          onInput={(e) => setParent((e.target as HTMLInputElement).value)}
        />
      </label>
      <div class="row clone-fields">
        <label class="field">
          <span class="muted small">Folder name</span>
          <input class="input" name="name" value={folder} onInput={(e) => setName((e.target as HTMLInputElement).value)} />
        </label>
        <label class="field">
          <span class="muted small">Branch (optional)</span>
          <input
            class="input"
            name="branch"
            placeholder="the default branch"
            value={branch}
            onInput={(e) => setBranch((e.target as HTMLInputElement).value)}
          />
        </label>
      </div>
      {path && <div class="muted small mono clone-target">Clones into {path}</div>}
      {error && (
        <div class="error-text">
          {error.text} {error.link && <a href={error.link.href}>{error.link.label}</a>}
        </div>
      )}
      <div class="row">
        <button type="submit" class="btn btn-small btn-primary" disabled={busy || !worker || !url.trim() || !path}>
          Clone
        </button>
        <button type="button" class="btn btn-small" onClick={onDone}>
          Cancel
        </button>
      </div>
    </form>
  );
}

/** "New worktree" of a workspace (T46, SPEC §13.1), under its row: a branch, new (made from
 *  the origin's current commit) or existing, and the folder, by default next to the origin
 *  (`<origin-name>-<branch>`, following the branch until it is edited). The worker checks the
 *  origin is a git repository and the branch is not checked out elsewhere; the new row shows
 *  the worktree until it is added, or why it was not. */
function NewWorktree({
  app,
  projectId,
  origin,
  onDone,
}: {
  app: AppState;
  projectId: string;
  origin: Workspace;
  onDone: () => void;
}) {
  const [branch, setBranch] = useState('');
  const [isNew, setIsNew] = useState(true);
  // `null`: the hub's default, shown as it will be.
  const [path, setPath] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<ReturnType<typeof addError> | null>(null);

  const shown = path ?? worktreePath(origin.path, branch.trim());
  const create = async (e: Event) => {
    e.preventDefault();
    setBusy(true);
    setError(null);
    try {
      await app.api.createWorkspace({
        kind: 'worktree',
        project_id: projectId,
        worker_id: origin.worker_id,
        origin_workspace_id: origin.id,
        branch: branch.trim(),
        new_branch: isNew,
        path: path?.trim() || undefined,
      });
      await app.workspacesChanged(projectId);
      onDone();
    } catch (err) {
      setError(addError(err, app));
    } finally {
      setBusy(false);
    }
  };

  return (
    <form class="new-worktree" onSubmit={create}>
      <div class="row clone-fields">
        <label class="field">
          <span class="muted small">Branch</span>
          <input
            class="input mono"
            name="branch"
            placeholder="hqc/fix"
            autocomplete="off"
            value={branch}
            onInput={(e) => setBranch((e.target as HTMLInputElement).value)}
          />
        </label>
        <div class="field worktree-branch-kind" role="radiogroup" aria-label="New or existing branch">
          <label>
            <input type="radio" name="branch-kind" value="new" checked={isNew} onChange={() => setIsNew(true)} />
            New, from the current commit
          </label>
          <label>
            <input type="radio" name="branch-kind" value="existing" checked={!isNew} onChange={() => setIsNew(false)} />
            Existing branch
          </label>
        </div>
      </div>
      <label class="field">
        <span class="muted small">Folder on the worker</span>
        <input
          class="input mono"
          name="path"
          placeholder={worktreePath(origin.path, 'branch')}
          value={shown}
          onInput={(e) => setPath((e.target as HTMLInputElement).value)}
        />
      </label>
      {error && (
        <div class="error-text">
          {error.text} {error.link && <a href={error.link.href}>{error.link.label}</a>}
        </div>
      )}
      <div class="row">
        <button type="submit" class="btn btn-small btn-primary" disabled={busy || !branch.trim() || !shown.trim()}>
          Create worktree
        </button>
        <button type="button" class="btn btn-small" onClick={onDone}>
          Cancel
        </button>
      </div>
    </form>
  );
}

/** Deleting a workspace hqc made (T45, T46, SPEC §13.2): the worker says first what the folder
 *  would lose — uncommitted changes and commits that are on no remote branch — then the
 *  folder can be deleted with the record, or only the record removed. A worktree goes with
 *  `git worktree remove` and keeps its branch; uncommitted files the dialog showed are
 *  removed with it (`force`). */
function DeleteFolderDialog({
  app,
  workspace: w,
  origin,
  worker,
  phone,
  onClose,
  onDone,
}: {
  app: AppState;
  workspace: Workspace;
  /** A worktree's origin. */
  origin?: Workspace;
  worker?: string;
  phone?: boolean;
  onClose: () => void;
  onDone: () => void;
}) {
  const [info, setInfo] = useState<WorkspaceInspection | null>(null);
  const [checkError, setCheckError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    app.api.inspectWorkspace(w.id).then(setInfo, (e) =>
      setCheckError(
        e instanceof ApiError && e.code === 'worker_offline'
          ? 'Worker offline: the folder can be checked and deleted once it is connected.'
          : `The worker could not check the folder: ${(e as Error).message}`,
      ),
    );
  }, [app, w.id]);

  const run = async (removeFolder: boolean) => {
    setBusy(true);
    setError(null);
    try {
      // The uncommitted files are listed above the button: deleting is confirming them.
      await app.api.deleteWorkspace(w.id, removeFolder, removeFolder && !!info && info.dirty_count > 0);
      onDone();
    } catch (e) {
      setError((e as Error).message);
    } finally {
      setBusy(false);
    }
  };

  const worktree = w.kind === 'worktree';
  const more = info ? info.dirty_count - info.dirty.length : 0;
  const clean = info && info.git && info.dirty_count === 0 && info.unpushed.length === 0;
  return (
    <div class={`modal-scrim ${phone ? 'modal-phone' : ''}`} onClick={(e) => e.target === e.currentTarget && onClose()}>
      <section class="confirm-dialog delete-folder" role="dialog" aria-modal="true" aria-label="Delete workspace">
        <header class="confirm-head">
          <div class="confirm-title">
            <div class="title">Delete {workspaceName(w)}?</div>
            <div class="sub mono">
              {worker ? `${worker}:` : ''}
              {homePath(w.path)}
            </div>
          </div>
          <button type="button" class="icon-btn" aria-label="Close" onClick={onClose}>
            <IconCross size={13} />
          </button>
        </header>
        <div class="confirm-body">
          {worktree ? (
            <p class="delete-what">
              hqc made this worktree{origin ? ` of ${workspaceName(origin)}` : ''}
              {w.ephemeral ? ' for a session an agent started' : ''}. Deleting it removes the folder with{' '}
              <span class="mono">git worktree remove</span>; the branch <span class="mono">{w.branch}</span> and its
              commits stay in the repository. Removing it from the project keeps the folder.
            </p>
          ) : (
            <p class="delete-what">
              hqc cloned this folder. Deleting it removes the folder from the worker’s disk; removing it from the project
              keeps the folder.
            </p>
          )}
          {!info && !checkError && <p class="muted">Checking the folder on the worker…</p>}
          {checkError && <p class="error-text">{checkError}</p>}
          {info && !info.git && <p class="delete-warn">Not a git repository: everything in it is lost.</p>}
          {clean && <p class="delete-clean">No uncommitted changes, and every commit is on a remote branch.</p>}
          {info && info.dirty_count > 0 && (
            <div class="delete-warn delete-dirty">
              <b>Uncommitted changes ({info.dirty_count})</b>
              <ul class="mono">
                {info.dirty.map((f) => (
                  <li key={f}>{f}</li>
                ))}
              </ul>
              {more > 0 && <div class="muted small">and {more} more</div>}
            </div>
          )}
          {info && info.unpushed.length > 0 && (
            <div class="delete-warn delete-unpushed">
              <b>Commits on no remote branch</b>
              <ul>
                {info.unpushed.map((u) => (
                  <li key={u.branch}>
                    <span class="mono">{u.branch}</span> — {u.commits} {u.commits === 1 ? 'commit' : 'commits'}
                  </li>
                ))}
              </ul>
            </div>
          )}
          {error && <div class="error-text">{error}</div>}
        </div>
        <footer class="confirm-foot">
          <button type="button" class="btn" onClick={onClose}>
            Cancel
          </button>
          <button type="button" class="btn btn-remove-record" disabled={busy} onClick={() => run(false)}>
            Remove from project only
          </button>
          <button type="button" class="btn btn-danger-fill btn-delete-folder" disabled={busy || !info} onClick={() => run(true)}>
            Delete folder
          </button>
        </footer>
      </section>
    </div>
  );
}
