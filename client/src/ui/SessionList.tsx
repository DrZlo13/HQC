import { useEffect, useLayoutEffect, useRef, useState } from 'preact/hooks';
import type { WorkerInfo } from '../api';
import type { AppState } from '../app';
import type { PermissionMode, SessionInfo, Workspace } from '../proto';
import { useSubscription } from './hooks';
import { colorHex } from '../theme';
import { ProjectGlyph, projectName } from './Appearance';
import { ArchiveList } from './Archive';
import { IconArchive, IconGear, IconGrip, IconMore, IconPlus, IconRename, IconReorder, IconSearch, IconTrash } from './icons';
import { AUTO_CONFIRM, formatWhen, shortPath } from './labels';
import { workspaceTitle } from './ProjectSettings';
import { RequestsLink } from './requests';
import { SearchHits } from './Search';
import { LIST_MODES, activeLabel, bucketOf, groupSessions, headCounts, matchesQuery, moveTo, subLabel, type SessionGroup } from './sessions';

/** "+ project": asks for the name, then opens the new project's settings — a new project
 *  has no workspaces yet, and that is the first thing to add. */
export async function newProject(app: AppState, onError: (m: string) => void): Promise<void> {
  const name = prompt('Project name', '');
  if (!name?.trim()) return;
  try {
    const p = await app.createProject(name.trim());
    location.hash = `#/p/${p.id}/settings`;
  } catch (e) {
    onError((e as Error).message);
  }
}

/** A press this long on a phone row opens its actions (MList2). */
const LONG_PRESS_MS = 450;

/** Renames a session by a prompt; a refusal goes to the list's notice. */
function rename(app: AppState, s: SessionInfo): void {
  const title = prompt('Rename the session', s.title)?.trim();
  if (!title || title === s.title) return;
  app.renameSession(s.id, title).catch((e: Error) => app.setListNotice(`Not renamed: ${e.message}`));
}

/** Deletes a session; the one open goes back to its project. */
function remove(app: AppState, s: SessionInfo, current?: string): void {
  app.deleteSession(s.id).then(
    () => {
      if (s.id === current) location.hash = `#/p/${s.project_id}`;
    },
    (e: Error) => app.setListNotice(`Not deleted: ${e.message}`),
  );
}

/** The sessions of the current project (SPEC §1: projects, in projects sessions), T26: a
 *  column next to the project rail on a desktop, the whole first screen on a phone, with
 *  the projects as chips above. T50: row actions (on hover on a desktop, a long press on a
 *  phone), the delayed archive with Undo, State / Recent / Custom and Reorder, archive mode. */
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
  const project = app.currentProject();
  useEffect(() => {
    if (project && !app.workspaces.has(project.id)) app.loadWorkspaces(project.id).catch(() => {});
    if (project && !app.orders.has(project.id)) void app.loadOrder(project.id);
  }, [app, project?.id]);
  if (project && app.archiveMode) return <ArchiveList app={app} project={project} current={current} phone={phone} />;
  return <LiveList app={app} current={current} phone={phone} requests={requests} />;
}

function LiveList({ app, current, phone, requests }: { app: AppState; current?: string; phone?: boolean; requests?: boolean }) {
  const [creating, setCreating] = useState(false);
  const [error, setError] = useState<string | null>(null);
  // The phone searches on its own screen (`#/search`).
  const [query, setQuery] = useState('');
  // Reorder mode: the order being arranged; Done stores it, Cancel drops it.
  const [reorder, setReorder] = useState<string[] | null>(null);
  // The phone's bottom sheet of a row (MList2), and its delete step.
  const [sheet, setSheet] = useState<{ id: string; confirm: boolean } | null>(null);
  const project = app.currentProject();
  const pid = project?.id;
  const archivedCount = app.archivedSessions().length;
  const mode = app.listMode();

  useEffect(() => setReorder(null), [pid]);

  const pathOf = (s: SessionInfo) => shortPath(app.sessionPath(s) ?? '');
  const whereOf = (s: SessionInfo) => [app.workerName(s.worker_id), pathOf(s)].filter(Boolean).join(' · ');
  const now = Date.now();
  let groups: SessionGroup[];
  if (reorder) {
    const items = reorder.map((id) => app.sessions.get(id)).filter((s): s is SessionInfo => !!s && !s.archived);
    groups = items.length ? [{ id: 'reorder', label: 'Drag to reorder', plain: true, items }] : [];
  } else {
    const list = app.sortedSessions().filter((s) => matchesQuery(s, pathOf(s), query));
    groups = groupSessions(list, mode, now, (pid && app.orders.get(pid)?.session_ids) || []);
  }
  const conn = app.conn !== 'open' ? ' · offline' : '';
  const counts = headCounts(app.sortedSessions());

  const startReorder = () => {
    // The order sent must hold every live session: the waiting archive goes first.
    app.commitArchive();
    setQuery('');
    setReorder(app.orderedSessions().map((s) => s.id));
  };
  // The archive should list the session archived a moment ago: it goes to the hub now.
  const openArchive = () => {
    app.commitArchive();
    app.setArchiveMode(true);
  };
  const doneReorder = () => {
    if (pid && reorder) void app.saveOrder(pid, reorder);
    setReorder(null);
  };

  // Dragging by the handle (pointer events: mouse and finger alike), ↑/↓ on a focused one.
  const drag = useRef<{ id: string; y: number; h: number; base: string[] } | null>(null);
  const [dragId, setDragId] = useState<string | null>(null);
  const focusGrip = useRef<string | null>(null);
  const scroll = useRef<HTMLDivElement>(null);
  useLayoutEffect(() => {
    const id = focusGrip.current;
    if (!id) return;
    focusGrip.current = null;
    const el = scroll.current?.querySelector<HTMLElement>(`[data-grip="${id}"]`);
    if (el && el.ownerDocument.activeElement !== el) el.focus();
  }, [reorder]);
  const endDrag = () => {
    if (!drag.current) return;
    drag.current = null;
    setDragId(null);
  };
  const grip = (id: string) => ({
    onPointerDown: (e: PointerEvent) => {
      if (!reorder) return;
      const el = e.currentTarget as HTMLElement;
      el.setPointerCapture?.(e.pointerId);
      const h = el.closest('li')?.getBoundingClientRect().height || 0;
      drag.current = { id, y: e.clientY, h: h ? h + 2 : 64, base: reorder };
      e.preventDefault();
      setDragId(id);
    },
    onPointerMove: (e: PointerEvent) => {
      const d = drag.current;
      if (!d) return;
      e.preventDefault();
      setReorder(moveTo(d.base, d.id, d.base.indexOf(d.id) + Math.round((e.clientY - d.y) / d.h)));
    },
    onPointerUp: endDrag,
    onPointerCancel: endDrag,
    onLostPointerCapture: endDrag,
    onKeyDown: (e: KeyboardEvent) => {
      const dir = e.key === 'ArrowUp' ? -1 : e.key === 'ArrowDown' ? 1 : 0;
      if (!dir || !reorder) return;
      e.preventDefault();
      focusGrip.current = id;
      setReorder(moveTo(reorder, id, reorder.indexOf(id) + dir));
    },
  });

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
  const order = reorder ? (
    <div class="reorder-bar" role="group" aria-label="Reorder">
      <span class="reorder-icon">
        <IconReorder />
      </span>
      <span class="reorder-title">Reorder</span>
      <button type="button" class="btn btn-small btn-ghost btn-reorder-cancel" onClick={() => setReorder(null)}>
        Cancel
      </button>
      <button type="button" class="btn btn-small btn-primary btn-reorder-done" onClick={doneReorder}>
        Done
      </button>
    </div>
  ) : (
    <div class="order-row">
      <div class="seg-group seg-3" role="group" aria-label="Session order">
        {LIST_MODES.map((m) => (
          <button
            type="button"
            key={m.id}
            class={`seg ${mode === m.id ? 'is-on' : ''}`}
            aria-pressed={mode === m.id}
            disabled={!pid}
            onClick={() => pid && mode !== m.id && void app.setListMode(pid, m.id)}
          >
            {m.label}
          </button>
        ))}
      </div>
      <button
        type="button"
        class={`icon-btn ${phone ? 'icon-btn-round' : ''} btn-reorder`}
        aria-label="Reorder sessions"
        title="Reorder"
        disabled={!pid || app.sortedSessions().length < 2}
        onClick={startReorder}
      >
        <IconReorder />
      </button>
    </div>
  );
  const pending = app.pendingArchive;
  const toast = pending && (
    <div class="toast toast-archive" role="status">
      <IconArchive />
      <span class="toast-text">Archived «{pending.title}»</span>
      <button type="button" class="btn-link btn-undo" onClick={() => app.undoArchive()}>
        Undo
      </button>
    </div>
  );
  const sheetFor = sheet && app.sessions.get(sheet.id);

  return (
    <section class={`sessions ${phone ? 'sessions-phone' : ''} ${reorder ? 'is-reordering' : ''}`}>
      {phone ? (
        <header class="m-head">
          <div class="m-head-row">
            <h1>Sessions</h1>
            <RequestsLink app={app} phone />
            <a class="icon-btn icon-btn-round" href="#/search" aria-label="Search sessions and messages">
              <IconSearch size={20} />
            </a>
            {project && (
              <a class="icon-btn icon-btn-round btn-project-settings" href={`#/p/${project.id}/settings`} aria-label="Project settings">
                <IconGear size={20} />
              </a>
            )}
            <a class="icon-btn icon-btn-round" href="#/settings" aria-label="Account and settings">
              <span class="avatar">{(app.me?.email ?? '?').slice(0, 1).toUpperCase()}</span>
            </a>
          </div>
          {(app.error || conn) && <div class="m-head-state">{app.error ? 'Hub unavailable' : 'offline'}</div>}
          <ProjectChips app={app} onError={setError} />
          {order}
        </header>
      ) : (
        <>
          <header class="side-head">
            {project && (
              <span class="side-tile" aria-hidden="true">
                <ProjectGlyph p={project} />
              </span>
            )}
            <div class="side-title">
              <div class="project-title">{project ? projectName(project) : 'hqc'}</div>
              <div class="side-counts">
                {counts}
                {conn}
              </div>
            </div>
            {project && (
              <a
                class="icon-btn btn-project-settings"
                href={`#/p/${project.id}/settings`}
                aria-label="Project settings: appearance, workspaces"
                title="Project settings"
              >
                <IconGear />
              </a>
            )}
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
            {order}
          </div>
        </>
      )}
      {app.error && <div class="notice">Hub unavailable: {app.error}</div>}
      {error && <div class="notice">{error}</div>}
      {app.listNotice && (
        <div class="notice" role="alert" onClick={() => app.setListNotice(null)}>
          {app.listNotice}
        </div>
      )}
      {creating && project && <NewSession key={project.id} app={app} projectId={project.id} onDone={() => setCreating(false)} />}
      <div class="session-scroll" ref={scroll}>
        {groups.map((g) => (
          <div key={g.id} class={`session-group group-${g.id}`}>
            <div class="group-head">
              <span class="group-label">{g.label}</span>
              {!g.plain && <span class="group-count">{g.items.length}</span>}
            </div>
            <ul class="session-list">
              {g.items.map((s) => (
                <SessionRow
                  key={s.id}
                  s={s}
                  current={s.id === current}
                  phone={phone}
                  when={mode === 'recent' && !reorder ? formatWhen(s.updated_at, now) : undefined}
                  where={whereOf(s)}
                  grip={reorder ? grip(s.id) : undefined}
                  dragging={dragId === s.id}
                  onArchive={() => app.archiveLater(s)}
                  onRename={() => rename(app, s)}
                  onDelete={() => remove(app, s, current)}
                  onSheet={() => setSheet({ id: s.id, confirm: false })}
                />
              ))}
            </ul>
          </div>
        ))}
        {app.me && groups.length === 0 && <div class="empty">{query ? 'No sessions match' : 'No sessions yet'}</div>}
        {phone && groups.length > 0 && (
          <p class="list-hint">
            {reorder
              ? 'Drag the handle on the right. Done saves the order and switches to Custom.'
              : 'Press and hold a session to archive, rename or delete it.'}
          </p>
        )}
        {!phone && toast}
        {!phone && <SearchHits app={app} query={query} />}
        {project && archivedCount > 0 && !reorder && (
          <div class="list-foot">
            <button type="button" class="btn-link btn-archive-open" onClick={openArchive}>
              <IconArchive />
              Archive
              <span class="archive-count">{archivedCount}</span>
            </button>
          </div>
        )}
        {phone && <WorkerList app={app} current={current} />}
      </div>
      {!phone && <RequestsLink app={app} current={requests} />}
      {!phone && <WorkerList app={app} current={current} />}
      {phone && toast}
      {phone && !reorder && (
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
      {sheet && sheetFor && (
        <ActionSheet
          s={sheetFor}
          where={whereOf(sheetFor)}
          confirm={sheet.confirm}
          onClose={() => setSheet(null)}
          onArchive={() => {
            setSheet(null);
            app.archiveLater(sheetFor);
          }}
          onRename={() => {
            setSheet(null);
            rename(app, sheetFor);
          }}
          onAskDelete={() => setSheet({ id: sheetFor.id, confirm: true })}
          onDelete={() => {
            setSheet(null);
            remove(app, sheetFor, current);
          }}
        />
      )}
    </section>
  );
}

type GripHandlers = {
  onPointerDown: (e: PointerEvent) => void;
  onPointerMove: (e: PointerEvent) => void;
  onPointerUp: () => void;
  onPointerCancel: () => void;
  onLostPointerCapture: () => void;
  onKeyDown: (e: KeyboardEvent) => void;
};

/** A row of the live list. Desktop: the archive button and "⋯" on hover or focus, delete
 *  confirmed in the row. Phone: a long press or a right click opens the bottom sheet. In
 *  Reorder: a handle (left on a desktop, right on a phone), no actions, no link. */
function SessionRow({
  s,
  current,
  phone,
  when,
  where,
  grip,
  dragging,
  onArchive,
  onRename,
  onDelete,
  onSheet,
}: {
  s: SessionInfo;
  current: boolean;
  phone?: boolean;
  when?: string;
  where: string;
  grip?: GripHandlers;
  dragging: boolean;
  onArchive: () => void;
  onRename: () => void;
  onDelete: () => void;
  onSheet: () => void;
}) {
  const [confirm, setConfirm] = useState(false);
  const press = useRef<{ timer: ReturnType<typeof setTimeout>; x: number; y: number } | null>(null);
  const fired = useRef(false);
  const bucket = bucketOf(s);
  const title = s.title || s.slug || s.id.slice(-8);
  const handle = grip && (
    <button type="button" class="grip" data-grip={s.id} aria-label={`Drag to reorder «${title}», or use arrow keys`} {...grip}>
      <IconGrip />
    </button>
  );
  const body = (
    <>
      <span class={`dot dot-${bucket} state-${s.state}`} aria-hidden="true" />
      <span class="session-main">
        <span class="session-top">
          <span class="session-title">{title}</span>
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
    </>
  );
  const cancelPress = () => {
    if (press.current) clearTimeout(press.current.timer);
    press.current = null;
  };
  const phoneHandlers =
    phone && !grip
      ? {
          onPointerDown: (e: PointerEvent) => {
            fired.current = false;
            cancelPress();
            press.current = {
              x: e.clientX,
              y: e.clientY,
              timer: setTimeout(() => {
                fired.current = true;
                press.current = null;
                onSheet();
              }, LONG_PRESS_MS),
            };
          },
          onPointerMove: (e: PointerEvent) => {
            // A scroll is not a press.
            const p = press.current;
            if (p && Math.hypot(e.clientX - p.x, e.clientY - p.y) > 10) cancelPress();
          },
          onPointerUp: cancelPress,
          onPointerLeave: cancelPress,
          onPointerCancel: cancelPress,
          onContextMenu: (e: MouseEvent) => {
            e.preventDefault();
            cancelPress();
            onSheet();
          },
        }
      : {};
  useEffect(() => cancelPress, []);
  return (
    <li class={`session-item ${dragging ? 'is-dragging' : ''} ${confirm ? 'is-confirming' : ''}`} {...phoneHandlers}>
      {!phone && handle}
      {grip ? (
        <div class={`session-row ${current ? 'is-current' : ''}`}>{body}</div>
      ) : (
        <a
          class={`session-row ${current ? 'is-current' : ''}`}
          href={`#/s/${s.id}`}
          aria-current={current ? 'page' : undefined}
          onClick={(e) => {
            // The press opened the sheet: no navigation on release.
            if (fired.current) {
              e.preventDefault();
              fired.current = false;
            }
          }}
        >
          {body}
        </a>
      )}
      {phone && handle}
      {!phone && !grip && !confirm && (
        <div class="row-acts">
          <button type="button" class="icon-btn btn-row-archive" aria-label={`Archive «${title}»`} title="Archive" onClick={onArchive}>
            <IconArchive />
          </button>
          <RowMenu onRename={onRename} onDelete={() => setConfirm(true)} />
        </div>
      )}
      {confirm && (
        <div class="row-confirm" role="group" aria-label="Confirm deletion">
          <span class="row-confirm-text">Delete forever?</span>
          <button
            type="button"
            class="btn btn-small btn-danger-fill btn-confirm-delete"
            onClick={() => {
              setConfirm(false);
              onDelete();
            }}
          >
            Delete
          </button>
          <button type="button" class="btn btn-small btn-ghost" onClick={() => setConfirm(false)}>
            Cancel
          </button>
        </div>
      )}
    </li>
  );
}

/** "⋯" of a desktop row: Rename, Delete… (confirmed in the row). */
function RowMenu({ onRename, onDelete }: { onRename: () => void; onDelete: () => void }) {
  const [open, setOpen] = useState(false);
  const box = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (!open) return;
    const doc = box.current?.ownerDocument;
    if (!doc) return;
    const away = (e: Event) => {
      if (box.current && !box.current.contains(e.target as Node)) setOpen(false);
    };
    const esc = (e: KeyboardEvent) => e.key === 'Escape' && setOpen(false);
    doc.addEventListener('pointerdown', away);
    doc.addEventListener('keydown', esc);
    return () => {
      doc.removeEventListener('pointerdown', away);
      doc.removeEventListener('keydown', esc);
    };
  }, [open]);
  const item = (cls: string, label: string, fn: () => void) => (
    <button
      type="button"
      role="menuitem"
      class={`menu-item ${cls}`}
      onClick={() => {
        setOpen(false);
        fn();
      }}
    >
      {label}
    </button>
  );
  return (
    <div class={`menu-box ${open ? 'is-open' : ''}`} ref={box}>
      <button
        type="button"
        class="icon-btn btn-row-more"
        aria-label="More: rename, delete"
        title="More"
        aria-haspopup="menu"
        aria-expanded={open}
        onClick={() => setOpen(!open)}
      >
        <IconMore />
      </button>
      {open && (
        <div class="menu" role="menu">
          {item('btn-rename', 'Rename', onRename)}
          {item('btn-delete menu-danger', 'Delete…', onDelete)}
        </div>
      )}
    </div>
  );
}

/** The phone's actions of a row (MList2): Archive, Rename, Delete… with a second step. */
function ActionSheet({
  s,
  where,
  confirm,
  onClose,
  onArchive,
  onRename,
  onAskDelete,
  onDelete,
}: {
  s: SessionInfo;
  where: string;
  confirm: boolean;
  onClose: () => void;
  onArchive: () => void;
  onRename: () => void;
  onAskDelete: () => void;
  onDelete: () => void;
}) {
  return (
    <>
      <div class="sheet-scrim" onClick={onClose} />
      <section class="action-sheet" role="dialog" aria-label="Session actions">
        <span class="action-sheet-grab" aria-hidden="true" />
        <div class="action-sheet-head">
          <span class="action-sheet-title">{s.title || s.slug}</span>
          {where && <span class="action-sheet-where mono">{where}</span>}
        </div>
        {confirm ? (
          <>
            <p class="action-sheet-note">Delete this session forever? Messages and history go too. Files on disk stay.</p>
            <button type="button" class="btn btn-large btn-danger-fill btn-sheet-delete-forever" onClick={onDelete}>
              Delete forever
            </button>
            <button type="button" class="btn btn-large btn-sheet-cancel" onClick={onClose}>
              Cancel
            </button>
          </>
        ) : (
          <>
            <button type="button" class="sheet-btn btn-sheet-archive" onClick={onArchive}>
              <IconArchive />
              Archive
            </button>
            <button type="button" class="sheet-btn btn-sheet-rename" onClick={onRename}>
              <IconRename />
              Rename
            </button>
            <button type="button" class="sheet-btn sheet-danger btn-sheet-delete" onClick={onAskDelete}>
              <IconTrash />
              Delete…
            </button>
            <button type="button" class="btn btn-large btn-sheet-cancel" onClick={onClose}>
              Cancel
            </button>
          </>
        )}
      </section>
    </>
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
            {/* Without an icon a dot of the project's color: the name is right after it. */}
            <span class="chip-icon" aria-hidden="true">
              {p.icon ? <ProjectGlyph p={p} /> : <span class="chip-dot" style={{ background: colorHex(p.color) }} />}
            </span>
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

/** New session: title (empty — "Session N", SPEC §3), workspace (the project's default
 *  preselected, SPEC §13.4) and an optional `cwd` inside it. */
function NewSession({ app, projectId, onDone }: { app: AppState; projectId: string; onDone: () => void }) {
  const [list, setList] = useState<Workspace[] | null>(app.workspaces.get(projectId) ?? null);
  const [workers, setWorkers] = useState<WorkerInfo[] | undefined>();
  const [title, setTitle] = useState('');
  const [workspace, setWorkspace] = useState('');
  const [cwd, setCwd] = useState('');
  // T38: '' is the worker's own default.
  const [mode, setMode] = useState<PermissionMode | ''>('');
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
      const s = await app.createSession(
        title.trim() || undefined,
        { workspace_id: chosen, cwd: cwd.trim(), permission_mode: mode || undefined },
        projectId,
      );
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
          <a href={`#/p/${projectId}/settings`}>Add a workspace</a>
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
      <label class="field">
        <span class="field-label">Permissions</span>
        <select
          class="input"
          name="permission_mode"
          value={mode}
          onChange={(e) => {
            const el = e.target as HTMLSelectElement;
            const next = el.value as PermissionMode | '';
            // Auto after a one-line confirmation; Ask and the default at once.
            if (next === 'auto' && !confirm(AUTO_CONFIRM)) {
              el.value = mode;
              return;
            }
            setMode(next);
          }}
        >
          <option value="">The worker's default</option>
          <option value="ask">Ask</option>
          <option value="auto">Auto</option>
        </select>
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
