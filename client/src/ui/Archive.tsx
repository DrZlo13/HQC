import { useState } from 'preact/hooks';
import type { AppState } from '../app';
import { goUp } from '../nav';
import { ApiError } from '../api';
import type { Project, SessionInfo } from '../proto';
import { projectName } from './Appearance';
import { IconArchive, IconBack, IconRestore, IconSearch, IconTrash } from './icons';
import { count, shortPath } from './labels';
import { archivedLabel, matchesQuery } from './sessions';

/** Archive mode of the session list (T50, `Archive` / `MArchive2`): it must look different at
 *  once — the sidebar without the project tint, a hatched header with a back arrow, and on
 *  every row "Restore" and "Delete" (confirmed in the row). Read-only: the rows open the
 *  archived session, whose view has no input. */
export function ArchiveList({ app, project, current, phone }: { app: AppState; project: Project; current?: string; phone?: boolean }) {
  const [query, setQuery] = useState('');
  const [confirm, setConfirm] = useState<string | null>(null);
  const [toast, setToast] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const all = app.archivedSessions();
  const pathOf = (s: SessionInfo) => shortPath(app.sessionPath(s) ?? '');
  const list = all.filter((s) => matchesQuery(s, pathOf(s), query));
  const now = Date.now();
  const titleOf = (s: SessionInfo) => s.title || s.slug;

  const restore = (s: SessionInfo) => {
    setNotice(null);
    app.restoreSession(s.id).then(
      () => setToast(`Restored «${titleOf(s)}» to sessions`),
      (e: Error) =>
        setNotice(
          e instanceof ApiError && e.code === 'no_workspace'
            ? `«${titleOf(s)}» cannot be restored: its workspace was removed.`
            : `Not restored: ${e.message}`,
        ),
    );
  };
  const remove = (s: SessionInfo) => {
    setConfirm(null);
    setNotice(null);
    app.deleteSession(s.id).then(
      () => {
        setToast(`Deleted «${titleOf(s)}»`);
        if (s.id === current) goUp(`#/p/${s.project_id}`);
      },
      (e: Error) => setNotice(`Not deleted: ${e.message}`),
    );
  };

  return (
    <section class={`sessions sessions-archive ${phone ? 'sessions-phone' : ''}`} aria-label={`${projectName(project)} archive`}>
      <header class="archive-head stripes">
        <div class="archive-head-row">
          <button
            type="button"
            class={`icon-btn ${phone ? 'icon-btn-round' : ''} btn-archive-back`}
            aria-label="Back to sessions"
            title="Back to sessions"
            onClick={() => app.setArchiveMode(false)}
          >
            <IconBack size={18} />
          </button>
          <span class="archive-tile" aria-hidden="true">
            <IconArchive />
          </span>
          <div class="side-title">
            <div class="project-title">Archive</div>
            <div class="side-counts">
              {projectName(project)} · {count(all.length, 'session')}
            </div>
          </div>
        </div>
        <p class="archive-note">Read-only. Restore a session to keep working, or delete it for good.</p>
      </header>
      {!phone && (
        <div class="side-tools archive-tools">
          <label class="search">
            <IconSearch size={14} />
            <input
              type="search"
              aria-label="Search archive"
              placeholder="Search archive"
              value={query}
              onInput={(e) => setQuery((e.currentTarget as HTMLInputElement).value)}
            />
          </label>
        </div>
      )}
      {notice && (
        <div class="notice" role="alert" onClick={() => setNotice(null)}>
          {notice}
        </div>
      )}
      <div class="session-scroll">
        <ul class="session-list archive-list">
          {list.map((s) => {
            const confirming = confirm === s.id;
            const where = [app.workerName(s.worker_id), pathOf(s)].filter(Boolean).join(' · ');
            return (
              <li key={s.id} class={`archive-row ${s.id === current ? 'is-current' : ''} ${confirming ? 'is-confirming' : ''}`}>
                <div class="archive-row-main">
                  <span class="archive-icon" aria-hidden="true">
                    <IconArchive />
                  </span>
                  <a class="archive-row-text" href={`#/s/${s.id}`} aria-current={s.id === current ? 'page' : undefined}>
                    <span class="session-title">{titleOf(s)}</span>
                    <span class="session-sub">{archivedLabel(s, now)}</span>
                    {where && (
                      <span class="session-where mono" title={where}>
                        {where}
                      </span>
                    )}
                  </a>
                </div>
                {confirming ? (
                  <div class="row-confirm" role="group" aria-label="Confirm deletion">
                    <span class="row-confirm-text">{phone ? 'Delete forever? This can’t be undone.' : 'Delete forever?'}</span>
                    <button type="button" class="btn btn-small btn-danger-fill btn-confirm-delete" onClick={() => remove(s)}>
                      Delete
                    </button>
                    <button type="button" class="btn btn-small btn-ghost" onClick={() => setConfirm(null)}>
                      Cancel
                    </button>
                  </div>
                ) : (
                  <div class="archive-acts">
                    <button type="button" class="btn btn-small btn-ghost btn-restore" onClick={() => restore(s)}>
                      <IconRestore />
                      Restore
                    </button>
                    <button type="button" class="btn btn-small btn-ghost btn-danger btn-delete" onClick={() => setConfirm(s.id)}>
                      <IconTrash />
                      Delete
                    </button>
                  </div>
                )}
              </li>
            );
          })}
        </ul>
        {list.length === 0 && <div class="empty">{query ? 'No sessions match' : 'Archive is empty'}</div>}
        {toast && (
          <div class="toast" role="status" onClick={() => setToast(null)}>
            <span class="toast-text">{toast}</span>
          </div>
        )}
      </div>
    </section>
  );
}
