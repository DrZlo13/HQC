import { useEffect, useState } from 'preact/hooks';
import type { WorkerInfo } from '../api';
import type { AppState } from '../app';
import type { Workspace } from '../proto';
import { useSubscription } from './hooks';
import { STATE_LABELS } from './labels';

/** The left column: projects, and the sessions of the current one (SPEC §1: projects, in
 *  projects sessions). */
export function SessionList({ app, current }: { app: AppState; current?: string }) {
  useSubscription((fn) => app.onChange(fn), [app]);
  const [creating, setCreating] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const project = app.currentProject();
  const [showArchived, setShowArchived] = useState(false);
  const archived = app.archivedSessions();
  const list = showArchived ? archived : app.sortedSessions();

  const newProject = async () => {
    const name = prompt('Название проекта', '');
    if (!name?.trim()) return;
    setError(null);
    try {
      const p = await app.createProject(name.trim());
      // A new project has no workspace yet: that is the first thing to add.
      location.hash = `#/p/${p.id}/workspaces`;
    } catch (e) {
      setError((e as Error).message);
    }
  };

  return (
    <section class="sessions">
      <header class="bar">
        <div class="bar-title">
          <div class="title">hqc</div>
          <div class="state">
            {app.me?.email ?? ''}
            {app.conn !== 'open' && ' · нет связи'}
          </div>
        </div>
        <a class="btn btn-small btn-ghost btn-icon" href="#/settings" aria-label="Настройки" title="Настройки">
          <svg viewBox="0 0 24 24" width="22" height="22" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
            <circle cx="12" cy="12" r="3" />
            <path d="M19.4 15a1.7 1.7 0 0 0 .3 1.8l.1.1a2 2 0 1 1-2.8 2.8l-.1-.1a1.7 1.7 0 0 0-1.8-.3 1.7 1.7 0 0 0-1 1.5V21a2 2 0 1 1-4 0v-.1a1.7 1.7 0 0 0-1.1-1.5 1.7 1.7 0 0 0-1.8.3l-.1.1a2 2 0 1 1-2.8-2.8l.1-.1a1.7 1.7 0 0 0 .3-1.8 1.7 1.7 0 0 0-1.5-1H3a2 2 0 1 1 0-4h.1a1.7 1.7 0 0 0 1.5-1.1 1.7 1.7 0 0 0-.3-1.8l-.1-.1a2 2 0 1 1 2.8-2.8l.1.1a1.7 1.7 0 0 0 1.8.3h.1a1.7 1.7 0 0 0 1-1.5V3a2 2 0 1 1 4 0v.1a1.7 1.7 0 0 0 1 1.5 1.7 1.7 0 0 0 1.8-.3l.1-.1a2 2 0 1 1 2.8 2.8l-.1.1a1.7 1.7 0 0 0-.3 1.8v.1a1.7 1.7 0 0 0 1.5 1H21a2 2 0 1 1 0 4h-.1a1.7 1.7 0 0 0-1.5 1.1z" />
          </svg>
        </a>
      </header>
      {app.error && <div class="notice">Хаб недоступен: {app.error}</div>}
      {error && <div class="notice">{error}</div>}
      <nav class="project-list" aria-label="Проекты">
        {app.sortedProjects().map((p) => {
          const unread = app.projectUnread(p.id);
          return (
            <a key={p.id} class={`project-row ${p.id === project?.id ? 'is-current' : ''}`} href={`#/p/${p.id}`}>
              <span class="project-name">{p.is_personal ? 'Личный' : p.name}</span>
              {unread > 0 && p.id !== project?.id && (
                <span class="badge" aria-label={`непрочитанных: ${unread}`}>
                  {unread > 99 ? '99+' : unread}
                </span>
              )}
            </a>
          );
        })}
        <button type="button" class="btn btn-small btn-ghost btn-new-project" disabled={!app.me} onClick={newProject}>
          + Проект
        </button>
      </nav>
      <WorkerList app={app} current={current} />
      {project && (
        <div class="project-head">
          <div class="project-title">{project.is_personal ? 'Личный' : project.name}</div>
          <a class="btn btn-small" href={`#/p/${project.id}/workspaces`}>
            Workspaces
          </a>
          {(archived.length > 0 || showArchived) && (
            <button
              type="button"
              class={`btn btn-small btn-ghost btn-archive-toggle ${showArchived ? 'is-on' : ''}`}
              aria-pressed={showArchived}
              title="Показать архивные сессии"
              onClick={() => setShowArchived(!showArchived)}
            >
              Архив {archived.length}
            </button>
          )}
          <button
            type="button"
            class="btn btn-small btn-primary btn-new-session"
            disabled={creating || !app.me}
            onClick={() => setCreating(true)}
          >
            Новая
          </button>
        </div>
      )}
      {creating && project && <NewSession key={project.id} app={app} projectId={project.id} onDone={() => setCreating(false)} />}
      <ul class="session-list">
        {list.map((s) => (
          <li key={s.id}>
            <a class={`session-row ${s.id === current ? 'is-current' : ''}`} href={`#/s/${s.id}`}>
              <span class={`dot state-${s.state}`} aria-hidden="true" />
              <span class="session-main">
                <span class="session-title">{s.title || s.slug || s.id.slice(-8)}</span>
                <span class={`session-state state-${s.state}`}>
                  {STATE_LABELS[s.state]}
                  {s.queue_len > 0 && ` · в очереди ${s.queue_len}`}
                </span>
              </span>
              {s.unread > 0 && (
                <span class="badge" aria-label={`непрочитанных: ${s.unread}`}>
                  {s.unread > 99 ? '99+' : s.unread}
                </span>
              )}
            </a>
          </li>
        ))}
        {app.me && list.length === 0 && <li class="empty">{showArchived ? 'Архив пуст' : 'Сессий пока нет'}</li>}
      </ul>
    </section>
  );
}

/** «Воркеры»: the service session of each worker, by its name (SPEC §10.2). */
function WorkerList({ app, current }: { app: AppState; current?: string }) {
  const list = app.workerSessions();
  if (list.length === 0) return null;
  return (
    <nav class="worker-list" aria-label="Воркеры">
      <div class="group-title">Воркеры</div>
      {list.map((s) => (
        <a key={s.id} class={`project-row worker-row ${s.id === current ? 'is-current' : ''}`} href={`#/s/${s.id}`}>
          <span class={`dot state-${s.state}`} aria-hidden="true" />
          <span class="project-name">{s.title}</span>
          {s.state === 'waiting' && <span class="worker-asks">ждёт ответа</span>}
          {s.state === 'worker_offline' && <span class="worker-asks muted">офлайн</span>}
          {s.unread > 0 && (
            <span class="badge" aria-label={`непрочитанных: ${s.unread}`}>
              {s.unread > 99 ? '99+' : s.unread}
            </span>
          )}
        </a>
      ))}
    </nav>
  );
}

/** `label` or the folder's name, then where it is. */
export function workspaceTitle(w: Workspace, workers?: WorkerInfo[]): string {
  const name = w.label || w.path.split('/').filter(Boolean).pop() || w.path;
  const worker = workers?.find((x) => x.id === w.worker_id)?.name;
  return `${name} — ${worker ? `${worker}:` : ''}${w.path}`;
}

/** New session: title (empty — «Сессия N», SPEC §3), workspace (the project's default
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
          В проекте нет ни одного workspace: сессии нужна папка на воркере.{' '}
          <a href={`#/p/${projectId}/workspaces`}>Добавить workspace</a>
        </p>
        <button type="button" class="btn btn-small" onClick={onDone}>
          Отмена
        </button>
      </div>
    );
  }
  return (
    <form class="new-session" onSubmit={create}>
      <label class="field">
        <span class="muted small">Название (можно оставить пустым)</span>
        <input class="input" value={title} onInput={(e) => setTitle((e.target as HTMLInputElement).value)} />
      </label>
      <label class="field">
        <span class="muted small">Workspace</span>
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
              {w.id === project?.default_workspace_id ? ' (по умолчанию)' : ''}
            </option>
          ))}
        </select>
      </label>
      <label class="field">
        <span class="muted small">Папка внутри workspace (необязательно)</span>
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
          Создать
        </button>
        <button type="button" class="btn btn-small" onClick={onDone}>
          Отмена
        </button>
      </div>
    </form>
  );
}
