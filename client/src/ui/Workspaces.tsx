import { useEffect, useState } from 'preact/hooks';
import { IconBack } from './icons';
import { ApiError, type WorkerInfo } from '../api';
import type { AppState } from '../app';
import { parseWorkspaceConflict, type Candidate, type DirEntry, type Workspace } from '../proto';
import { useSubscription } from './hooks';
import { workspaceTitle } from './SessionList';

/** Why a workspace could not be added, as the user reads it. */
function addError(e: unknown, app: AppState): { text: string; link?: { href: string; label: string } } {
  if (e instanceof ApiError) {
    const c = parseWorkspaceConflict(e.body);
    if (c) {
      const p = app.projects.get(c.existing_project_id);
      const name = p ? (p.is_personal ? 'Личный' : p.name) : 'другом';
      return {
        text: `Эта папка уже в проекте «${name}»: ${c.existing_path}. Папка принадлежит одному проекту, вложенные папки тоже.`,
        link: { href: `#/p/${c.existing_project_id}/workspaces`, label: 'Открыть проект' },
      };
    }
    if (e.code === 'worker_offline') {
      return { text: 'Воркер офлайн: путь проверяет сам воркер, добавить папку можно, когда он подключён.' };
    }
  }
  return { text: (e as Error).message };
}

/** The project's workspaces (SPEC §3, §13): list, add, make default, remove the record. */
export function Workspaces({ app, projectId }: { app: AppState; projectId: string }) {
  useSubscription((fn) => app.onChange(fn), [app]);
  const [workers, setWorkers] = useState<WorkerInfo[] | null>(null);
  const [adding, setAdding] = useState(false);
  const [importing, setImporting] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const project = app.projects.get(projectId);
  const list = app.workspaces.get(projectId);

  useEffect(() => {
    app.loadWorkspaces(projectId).catch((e: Error) => setError(e.message));
    app.api.workers().then(setWorkers, () => setWorkers([]));
  }, [app, projectId]);

  const remove = async (w: Workspace) => {
    if (!confirm(`Убрать workspace ${w.path} из проекта? Папка на воркере останется.`)) return;
    setError(null);
    try {
      await app.api.deleteWorkspace(w.id);
      await app.workspacesChanged(projectId);
    } catch (e) {
      const busy = e instanceof ApiError && e.code === 'workspace_in_use';
      setError(busy ? `${e.message}. Сначала архивируйте эти сессии.` : (e as Error).message);
    }
  };

  // «Сессии VS Code» (T21, SPEC §9.2): the worker mirrors the folder's Claude Code sessions.
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

  // The personal project keeps its name (SPEC §3 Project): no button for it.
  const rename = async () => {
    if (!project) return;
    const name = prompt('Новое название проекта', project.name);
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
    if (!confirm(`Удалить проект «${project.name}» и его сессии (${n})? Папки на воркерах останутся.`)) return;
    try {
      await app.deleteProject(projectId);
      location.hash = '#/';
    } catch (e) {
      setError((e as Error).message);
    }
  };

  const name = project ? (project.is_personal ? 'Личный' : project.name) : '';
  return (
    <section class="page workspaces">
      <header class="bar">
        <a class="back icon-btn" href={`#/p/${projectId}`} aria-label="К проекту">
          <IconBack />
        </a>
        <div class="bar-title">
          <div class="title">Workspaces</div>
          <div class="state">{name}</div>
        </div>
      </header>
      <div class="page-body">
        <p class="muted">
          Папки на воркерах, в которых работают сессии проекта. Одна папка принадлежит одному проекту. Удаление убирает
          только запись: папка на диске остаётся.
        </p>
        {list && list.length === 0 && <p class="empty-inline">Пока нет ни одного workspace.</p>}
        {list && list.length > 0 && (
          <ul class="workspace-list">
            {list.map((w) => {
              const isDefault = w.id === project?.default_workspace_id;
              return (
                <li key={w.id} class="workspace-row">
                  <div class="workspace-main">
                    <b>{workspaceTitle(w, workers ?? undefined)}</b>
                    {isDefault && <span class="tag">по умолчанию</span>}
                  </div>
                  <label
                    class="choice mirror-toggle"
                    title="Показывать в проекте сессии Claude Code этой папки (VS Code, терминал) и подхватывать их"
                  >
                    <input
                      type="checkbox"
                      name="mirror"
                      checked={w.mirror_enabled}
                      onChange={(e) => toggleMirror(w, (e.currentTarget as HTMLInputElement).checked)}
                    />
                    <span>Сессии VS Code</span>
                  </label>
                  <div class="row">
                    {!isDefault && (
                      <button type="button" class="btn btn-small btn-make-default" onClick={() => makeDefault(w)}>
                        Сделать основным
                      </button>
                    )}
                    <button type="button" class="btn btn-small btn-deny btn-remove-workspace" onClick={() => remove(w)}>
                      Убрать
                    </button>
                  </div>
                </li>
              );
            })}
          </ul>
        )}
        {error && <div class="error-text">{error}</div>}
        {adding ? (
          <AddWorkspace
            app={app}
            projectId={projectId}
            workers={workers ?? []}
            onDone={() => setAdding(false)}
          />
        ) : importing ? (
          <ImportFromClaude
            app={app}
            projectId={projectId}
            workers={workers ?? []}
            onDone={() => setImporting(false)}
          />
        ) : (
          <div class="row">
            <button type="button" class="btn btn-primary btn-add-workspace" onClick={() => setAdding(true)}>
              Добавить workspace
            </button>
            <button type="button" class="btn btn-import-claude" onClick={() => setImporting(true)}>
              Импорт из Claude Code
            </button>
          </div>
        )}
        {project && !project.is_personal && (
          <>
            <h2>Проект</h2>
            <div class="row">
              <button type="button" class="btn btn-rename-project" onClick={rename}>
                Переименовать
              </button>
              <button type="button" class="btn btn-deny btn-delete-project" onClick={deleteProject}>
                Удалить проект
              </button>
            </div>
          </>
        )}
      </div>
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
          Нет ни одного воркера. <a href="#/settings">Зарегистрируйте машину</a>.
        </p>
        <button type="button" class="btn btn-small" onClick={onDone}>
          Отмена
        </button>
      </div>
    );
  }
  return (
    <div class="import-claude">
      <p class="muted small">
        Папки, в которых на воркере есть сессии Claude Code (VS Code или терминал). Выбранные станут workspace этого
        проекта: их сессии появятся здесь с историей, новые записи будут подтягиваться.
      </p>
      <label class="field">
        <span class="muted small">Воркер</span>
        <select class="input" name="worker" value={worker} onChange={(e) => setWorker((e.target as HTMLSelectElement).value)}>
          {workers.map((w) => (
            <option key={w.id} value={w.id}>
              {w.name}
              {w.status === 'online' ? '' : ' (офлайн)'}
            </option>
          ))}
        </select>
      </label>
      {loadError && <div class="error-text">{loadError}</div>}
      {list === null && !loadError && <div class="muted small">…</div>}
      {list && list.length === 0 && !loadError && <div class="muted small">Сессий Claude Code не найдено</div>}
      {list && list.length > 0 && (
        <ul class="candidate-list">
          {list.map((c) => (
            <li key={c.path}>
              <label class="candidate">
                <input type="checkbox" checked={picked.has(c.path)} onChange={() => toggle(c.path)} />
                <span class="candidate-path">{c.path}</span>
                <span class="muted small">
                  {' '}
                  · сессий: {c.sessions_count} · {new Date(c.last_active).toLocaleString('ru-RU')}
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
          Импортировать
        </button>
        <button type="button" class="btn btn-small" onClick={onDone}>
          Отмена
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
          Нет ни одного воркера. <a href="#/settings">Зарегистрируйте машину</a>, чтобы выбрать на ней папку.
        </p>
        <button type="button" class="btn btn-small" onClick={onDone}>
          Отмена
        </button>
      </div>
    );
  }
  return (
    <form class="add-workspace" onSubmit={add}>
      <label class="field">
        <span class="muted small">Воркер</span>
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
              {w.status === 'online' ? '' : ' (офлайн)'}
            </option>
          ))}
        </select>
      </label>
      <div class="dir-browser">
        <div class="dir-path">
          <span class="muted small">{dir ?? 'Разрешённые корни воркера'}</span>
          {dir !== null && (
            <button type="button" class="btn btn-small btn-ghost btn-dir-up" onClick={() => setDir(roots.includes(dir) ? null : parent)}>
              ↑ Выше
            </button>
          )}
          {dir !== null && (
            <button type="button" class="btn btn-small btn-pick-dir" onClick={() => setPath(dir)}>
              Выбрать эту папку
            </button>
          )}
        </div>
        {browseError && <div class="error-text">{browseError}</div>}
        {entries === null && !browseError && <div class="muted small">…</div>}
        {entries && entries.length === 0 && !browseError && <div class="muted small">Подпапок нет</div>}
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
        <span class="muted small">Путь на воркере</span>
        <input
          class="input"
          name="path"
          placeholder="/Users/me/work/project"
          value={path}
          onInput={(e) => setPath((e.target as HTMLInputElement).value)}
        />
      </label>
      <label class="field">
        <span class="muted small">Метка (необязательно)</span>
        <input class="input" name="label" value={label} onInput={(e) => setLabel((e.target as HTMLInputElement).value)} />
      </label>
      {error && (
        <div class="error-text">
          {error.text} {error.link && <a href={error.link.href}>{error.link.label}</a>}
        </div>
      )}
      <div class="row">
        <button type="submit" class="btn btn-small btn-primary" disabled={busy || !worker || !path.trim()}>
          Добавить
        </button>
        <button type="button" class="btn btn-small" onClick={onDone}>
          Отмена
        </button>
      </div>
    </form>
  );
}
