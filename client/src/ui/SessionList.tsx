import { useEffect, useLayoutEffect, useRef, useState } from 'preact/hooks';
import type { AppState } from '../app';
import type { ListMode, Project, SessionInfo } from '../proto';
import { colorHex } from '../theme';
import { useSubscription } from './hooks';
import { ProjectGlyph, projectName, tileBackground } from './Appearance';
import { ArchiveList } from './Archive';
import { ContextMenu, fitInView, type MenuEntry } from './ContextMenu';
import {
  IconArchive,
  IconBack,
  IconChevronDown,
  IconChevronUp,
  IconCode,
  IconGrip,
  IconMore,
  IconMove,
  IconOpenNew,
  IconPin,
  IconPlus,
  IconRename,
  IconReorder,
  IconRestore,
  IconSearch,
  IconSettings,
  IconShield,
  IconStar,
  IconStarOff,
  IconTrash,
} from './icons';
import { agentsLabel, formatAge, formatWhen, homePath } from './labels';
import { newSessionWhere } from './NewSession';
import { MoveSession, confirmDeleteProject } from './ProjectSettings';
import { RequestsLink } from './requests';
import { LIST_MODES, bucketOf, groupSessions, lastLine, moveTo, runningLabel, subLabel, type SessionGroup } from './sessions';

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

/** A press this long on a phone row opens its menu (MList2). */
const LONG_PRESS_MS = 450;

/** The mouse resting this long on a row shows its card; passing over it shows nothing. */
const CARD_MS = 500;

/** A desktop row's archive button: its size, and how far past the column's right edge it
 *  stands (past the width grip, which reaches 9 px over the edge), px. */
const ROW_ARCHIVE_SIZE = 30;
const ROW_ARCHIVE_GAP = 12;
/** The button stays this long after the mouse leaves the row or the button: the way from the
 *  row to the button crosses the column's edge. */
const ROW_ARCHIVE_LEAVE_MS = 300;

/** The session column's width when dragged by its right edge, px; ←/→ on the edge step it. */
export const LIST_WIDTH_MIN = 240;
export const LIST_WIDTH_MAX = 560;
export const LIST_WIDTH_STEP = 16;

export function clampListWidth(w: number): number {
  return Math.round(Math.min(LIST_WIDTH_MAX, Math.max(LIST_WIDTH_MIN, w)));
}

/** The pointer resting this long on a link starts loading what it opens (SPEC §6). */
const PREFETCH_MS = 60;
let prefetchTimer: ReturnType<typeof setTimeout> | undefined;

/** Handlers for a link to a chat: `load` runs once the mouse has rested on it for
 *  `PREFETCH_MS`, or at once on a press. One mouse rests on one link: one timer. A touch
 *  loads nothing: on a phone it may start a scroll. */
export function prefetchOn(load: () => void) {
  return {
    onPointerEnter: (e: PointerEvent) => {
      if (e.pointerType !== 'mouse') return;
      clearTimeout(prefetchTimer);
      prefetchTimer = setTimeout(load, PREFETCH_MS);
    },
    onPointerLeave: (e: PointerEvent) => {
      if (e.pointerType === 'mouse') clearTimeout(prefetchTimer);
    },
    onPointerDown: (e: PointerEvent) => {
      if (e.pointerType !== 'mouse') return;
      clearTimeout(prefetchTimer);
      load();
    },
  };
}

/** The rules screen of the session's folder (T48), if it has one. */
function rulesHref(s: SessionInfo): string | undefined {
  return s.workspace_id && `#/p/${s.project_id}/rules/${s.workspace_id}?session=${encodeURIComponent(s.id)}`;
}

/** Renames a session by a prompt; a refusal goes to the list's notice. */
function rename(app: AppState, s: SessionInfo): void {
  const title = prompt('Rename the session', s.title)?.trim();
  if (!title || title === s.title) return;
  app.renameSession(s.id, title).catch((e: Error) => app.setListNotice(`Not renamed: ${e.message}`));
}

/** Adds the session to the user's favorites or takes it out; a refusal goes to the list's
 *  notice. */
function toggleFavorite(app: AppState, s: SessionInfo): void {
  app.setSessionFavorite(s.id, !s.favorite_at).catch((e: Error) => app.setListNotice(`Favorites not saved: ${e.message}`));
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
 *  column next to the project rail on a desktop, a phone's project screen (`#/p/<id>`; its
 *  first screen is `PhoneHome`). T50: the delayed archive with Undo, State / Recent / Custom
 *  and Reorder, archive mode. A row's actions are its menu (a right click; a long press on a
 *  phone); the header is one row (on a phone after "‹" back to the projects) and the favorites
 *  are pinned over the list; on a desktop the row under the mouse has a round archive button
 *  past the column's edge and its card shows where it runs, the workers are one line at the
 *  bottom and the column's right edge sets its width. */
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
    if (project && !app.orderLoaded(project.id)) void app.loadOrder(project.id);
  }, [app, project?.id]);
  if (project && app.archiveMode) return <ArchiveList app={app} project={project} current={current} phone={phone} />;
  return <LiveList app={app} current={current} phone={phone} requests={requests} />;
}

function LiveList({ app, current, phone, requests }: { app: AppState; current?: string; phone?: boolean; requests?: boolean }) {
  // The session whose "Move to project…" form is open.
  const [moving, setMoving] = useState<string | null>(null);
  // Reorder mode: the order being arranged; Done stores it, Cancel drops it.
  const [reorder, setReorder] = useState<string[] | null>(null);
  // A row's menu (a right click; a long press on a phone): the session, where it was opened,
  // and whether it shows the delete step.
  const [menu, setMenu] = useState<{ id: string; x: number; y: number; confirm: boolean } | null>(null);
  // The card of the row the mouse rests on (desktop), right of the column.
  const [card, setCard] = useState<{ id: string; x: number; y: number } | null>(null);
  const cardTimer = useRef<ReturnType<typeof setTimeout> | undefined>(undefined);
  // The archive button of the row the mouse is on (desktop), past the column's edge: the row's
  // session and element.
  const [rowArchive, setRowArchive] = useState<{ id: string; row: HTMLElement } | null>(null);
  const leaveTimer = useRef<ReturnType<typeof setTimeout> | undefined>(undefined);
  const rowArchiveBtn = useRef<HTMLButtonElement>(null);
  const project = app.currentProject();
  const pid = project?.id;
  const archivedCount = app.archivedSessions().length;
  const mode = app.listMode();
  // The project's new session open in the main column: on top of the list, in place of the
  // session its first message created until that session's chat replaces it.
  const draft = app.newSession?.projectId === pid ? app.newSession : null;
  const newSession = () => {
    if (pid) location.hash = `#/p/${pid}/new`;
  };

  useEffect(() => setReorder(null), [pid]);
  useEffect(
    () => () => {
      clearTimeout(cardTimer.current);
      clearTimeout(leaveTimer.current);
    },
    [],
  );
  const now = Date.now();
  // The project's favorites above its groups.
  const favorites = reorder ? [] : app.favoriteSessions(pid);
  let groups: SessionGroup[];
  if (reorder) {
    const items = reorder.map((id) => app.sessions.get(id)).filter((s): s is SessionInfo => !!s && !s.archived);
    groups = items.length ? [{ id: 'reorder', label: 'Drag to reorder', plain: true, items }] : [];
  } else {
    // A favorite is listed once, in the Favorites block; Reorder still holds every session.
    const list = app.sortedSessions().filter((s) => !s.favorite_at && s.id !== draft?.started);
    groups = groupSessions(list, mode, now, (pid && app.orders.get(pid)?.session_ids) || []);
  }
  // The phone's head says "offline" (a desktop's workers line does); not before the first list
  // and the socket's first connect.
  const conn = app.offline ? 'offline' : '';

  const startReorder = () => {
    // The order sent must hold every live session: the waiting archive goes first.
    app.commitArchive();
    setRowArchive(null);
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
  const hideCard = () => {
    clearTimeout(cardTimer.current);
    setCard(null);
  };
  // The row's archive button goes a moment after the mouse leaves the row or the button, unless
  // it comes onto the other one meanwhile.
  const keepRowArchive = () => clearTimeout(leaveTimer.current);
  const leaveRowArchive = () => {
    clearTimeout(leaveTimer.current);
    leaveTimer.current = setTimeout(() => setRowArchive(null), ROW_ARCHIVE_LEAVE_MS);
  };
  const hideRowArchive = () => {
    clearTimeout(leaveTimer.current);
    setRowArchive(null);
  };
  // Where a desktop row's archive button and card go: right of the column, the card past the
  // button.
  const colRight = (row: HTMLElement) => (row.closest('.sessions') ?? row).getBoundingClientRect().right;
  // A desktop row under the mouse: its archive button at once, its card once the mouse has
  // rested on the row for CARD_MS.
  const cardOn = (id: string) => ({
    onPointerEnter: (e: PointerEvent) => {
      if (e.pointerType !== 'mouse') return;
      const row = e.currentTarget as HTMLElement;
      clearTimeout(leaveTimer.current);
      setRowArchive({ id, row });
      clearTimeout(cardTimer.current);
      // Another row's card goes at once, not when this one's comes.
      setCard((c) => (c && c.id !== id ? null : c));
      cardTimer.current = setTimeout(() => {
        // The row moved to another group meanwhile (Running → Idle): its element is gone.
        if (!row.isConnected) return;
        const top = row.getBoundingClientRect().top;
        setCard({ id, x: colRight(row) + ROW_ARCHIVE_GAP + ROW_ARCHIVE_SIZE + 8, y: top - 6 });
      }, CARD_MS);
    },
    onPointerLeave: (e: PointerEvent) => {
      hideCard();
      if (e.pointerType === 'mouse') leaveRowArchive();
    },
    onPointerDown: hideCard,
  });
  // The archive button stands where its row is now: a row that moved under a still mouse
  // (Running → Idle) takes it along, and a row that left the list (another group, archived)
  // takes it away.
  useLayoutEffect(() => {
    const b = rowArchiveBtn.current;
    if (!rowArchive || !b) return;
    const { row } = rowArchive;
    if (!row.isConnected) {
      hideRowArchive();
      return;
    }
    const r = row.getBoundingClientRect();
    b.style.left = `${colRight(row) + ROW_ARCHIVE_GAP}px`;
    b.style.top = `${r.top + r.height / 2 - ROW_ARCHIVE_SIZE / 2}px`;
  });
  const openMenu = (id: string, x: number, y: number) => {
    hideCard();
    hideRowArchive();
    setMenu({ id, x, y, confirm: false });
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

  const order = reorder && (
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
  const row = (s: SessionInfo) => (
    <SessionRow
      key={s.id}
      s={s}
      current={s.id === current}
      draft={app.draft(s.id) !== ''}
      phone={phone}
      // A desktop row has no time: its group says how old it is, its card says exactly.
      when={phone && mode === 'recent' && !reorder ? formatWhen(s.updated_at, now) : undefined}
      compact={app.lookName() === 'compact'}
      grip={reorder ? grip(s.id) : undefined}
      dragging={dragId === s.id}
      menuOpen={menu?.id === s.id}
      card={phone || reorder ? undefined : cardOn(s.id)}
      onPrefetch={() => app.prefetchChat(s.id)}
      onMenu={(x, y) => openMenu(s.id, x, y)}
    />
  );
  // Pinned over the scrolling list.
  const favoriteGroup = favorites.length > 0 && (
    <div class="session-group group-favorites fav-pinned">
      <div class="group-head">
        <span class="group-label">Favorites</span>
        <span class="group-count">{favorites.length}</span>
        <span class="group-pin" aria-hidden="true">
          <IconPin />
        </span>
      </div>
      <ul class="session-list">{favorites.map(row)}</ul>
    </div>
  );
  const menuFor = menu && app.sessions.get(menu.id);
  // No card while a menu is open; a menu whose session was deleted elsewhere is not shown and
  // does not keep the cards away.
  const cardFor = card && !menuFor && app.sessions.get(card.id);
  // Not for a row that went meanwhile (archived here or elsewhere).
  const hovered = rowArchive && app.sessions.get(rowArchive.id);
  const archiveFor = hovered && !hovered.archived && hovered.id !== app.pendingArchive?.id && !menuFor && !reorder ? hovered : undefined;
  const moveFor = moving && app.sessions.get(moving);

  return (
    <section class={`sessions ${phone ? 'sessions-phone' : ''} ${reorder ? 'is-reordering' : ''}`}>
      {/* One row: on a phone "‹" back to the projects, then the project's name, the search (its
          own page), the order, the project's menu and a new session. */}
      <header class="side-head">
        {phone && (
          <a class="icon-btn btn-back" href="#/" aria-label="Back to projects">
            <IconBack size={20} />
          </a>
        )}
        <h2 class="project-title" title={project ? projectName(project) : undefined}>
          {project ? projectName(project) : 'HQC'}
        </h2>
        <a class="icon-btn btn-search" href="#/search" aria-label="Search sessions and messages" title="Search">
          <IconSearch />
        </a>
        <ModeMenu app={app} pid={pid} mode={mode} disabled={!!reorder} phone={phone} onReorder={startReorder} />
        {project && <ProjectMenu app={app} project={project} phone={phone} />}
        <button
          type="button"
          class="btn btn-primary btn-new-session"
          aria-label="New session"
          title="New session"
          disabled={!pid || !app.me}
          onClick={newSession}
        >
          <IconPlus size={14} />
        </button>
      </header>
      {phone && (app.error || conn) && <div class="m-head-state">{app.error ? 'Hub unavailable' : 'offline'}</div>}
      {reorder && <div class="side-tools">{order}</div>}
      {app.error && <div class="notice">Hub unavailable: {app.error}</div>}
      {app.listNotice && (
        <div class="notice" role="alert" onClick={() => app.setListNotice(null)}>
          {app.listNotice}
        </div>
      )}
      {moveFor && (
        <div class="move-panel" role="region" aria-label="Move to project">
          <MoveSession key={moveFor.id} app={app} session={moveFor} onDone={() => setMoving(null)} />
        </div>
      )}
      {favoriteGroup}
      <div
        class="session-scroll"
        ref={scroll}
        onScroll={
          card || rowArchive
            ? () => {
                hideCard();
                hideRowArchive();
              }
            : undefined
        }
      >
        {draft && !reorder && <DraftRow app={app} />}
        {groups.map((g) => (
          <div key={g.id} class={`session-group group-${g.id}`}>
            <div class="group-head">
              <span class="group-label">{g.label}</span>
              {!g.plain && <span class="group-count">{g.items.length}</span>}
            </div>
            <ul class="session-list">{g.items.map(row)}</ul>
          </div>
        ))}
        {app.me && groups.length === 0 && !draft && !favorites.some((s) => s.project_id === pid) && (
          <div class="empty">No sessions yet</div>
        )}
        {phone && (groups.length > 0 || favorites.length > 0) && (
          <p class="list-hint">
            {reorder
              ? 'Drag the handle on the right. Done saves the order and switches to Custom.'
              : 'Press and hold a session to archive, rename or delete it.'}
          </p>
        )}
        {!phone && toast}
        {project && archivedCount > 0 && !reorder && (
          <div class="list-foot">
            <button type="button" class="btn-link btn-archive-open" onClick={openArchive}>
              <IconArchive />
              Archive
              <span class="archive-count">{archivedCount}</span>
            </button>
          </div>
        )}
      </div>
      {!phone && <RequestsLink app={app} current={requests} />}
      {!phone && <WorkersLine app={app} current={current} />}
      {phone && toast}
      {menu && menuFor && (
        <ContextMenu
          x={menu.x}
          y={menu.y}
          label={`Session «${titleOf(menuFor)}»`}
          items={sessionMenu(app, menuFor, {
            onMove: () => setMoving(menuFor.id),
            onAskDelete: () => setMenu({ ...menu, confirm: true }),
          })}
          backdrop={phone}
          onClose={() => setMenu(null)}
        >
          {menu.confirm && (
            <>
              <p class="menu-note">Delete this session forever? Messages and history go too. Files on disk stay.</p>
              <button
                type="button"
                role="menuitem"
                class="menu-item menu-danger btn-confirm-delete"
                onClick={() => {
                  setMenu(null);
                  remove(app, menuFor, current);
                }}
              >
                <IconTrash />
                <span class="menu-label">Delete forever</span>
              </button>
              {/* The focus is on Cancel: Enter right after "Delete…" deletes nothing. */}
              <button type="button" role="menuitem" class="menu-item btn-cancel-delete" data-focus onClick={() => setMenu(null)}>
                <span class="menu-label">Cancel</span>
              </button>
            </>
          )}
        </ContextMenu>
      )}
      {archiveFor && (
        <button
          ref={rowArchiveBtn}
          type="button"
          class="row-archive btn-row-archive"
          aria-label={`Archive «${titleOf(archiveFor)}»`}
          title="Archive"
          onPointerEnter={keepRowArchive}
          onPointerLeave={leaveRowArchive}
          onClick={() => {
            hideRowArchive();
            hideCard();
            app.archiveLater(archiveFor);
          }}
        >
          <IconArchive />
        </button>
      )}
      {cardFor && <SessionCard key={cardFor.id} app={app} s={cardFor} x={card!.x} y={card!.y} />}
    </section>
  );
}

function titleOf(s: SessionInfo): string {
  return s.title || s.slug || s.id.slice(-8);
}

/** A session's menu, the same on a desktop row and a phone row: Open in new tab, favorites,
 *  then Rename, Move to project…, Folder rules, then Archive and Delete… (a step of its own). */
function sessionMenu(app: AppState, s: SessionInfo, on: { onMove: () => void; onAskDelete: () => void }): MenuEntry[] {
  const rules = rulesHref(s);
  return [
    { label: 'Open in new tab', icon: <IconOpenNew />, cls: 'btn-open-tab', href: `#/s/${s.id}`, newTab: true },
    {
      label: s.favorite_at ? 'Remove from favorites' : 'Add to favorites',
      icon: s.favorite_at ? <IconStarOff /> : <IconStar />,
      cls: 'btn-favorite',
      onSelect: () => toggleFavorite(app, s),
    },
    'sep',
    { label: 'Rename', icon: <IconRename />, cls: 'btn-rename', onSelect: () => rename(app, s) },
    { label: 'Move to project…', icon: <IconMove />, cls: 'btn-move', onSelect: on.onMove },
    ...(rules ? [{ label: 'Folder rules', icon: <IconShield />, cls: 'btn-rules', href: rules }] : []),
    'sep',
    { label: 'Archive', icon: <IconArchive />, cls: 'btn-archive', onSelect: () => app.archiveLater(s) },
    { label: 'Delete…', icon: <IconTrash />, cls: 'btn-delete', danger: true, keepOpen: true, onSelect: on.onAskDelete },
  ];
}

/** Where a header menu opens: under its button. */
function below(el: HTMLElement | null): { x: number; y: number } {
  const r = el?.getBoundingClientRect();
  return { x: r?.left ?? 0, y: (r?.bottom ?? 0) + 4 };
}

/** "State ▾" of the column's header: the list's order — State, Recent, Custom — and Reorder. */
function ModeMenu({
  app,
  pid,
  mode,
  disabled,
  phone,
  onReorder,
}: {
  app: AppState;
  pid?: string;
  mode: ListMode;
  disabled: boolean;
  /** A tap beside the menu only closes it. */
  phone?: boolean;
  onReorder: () => void;
}) {
  const [at, setAt] = useState<{ x: number; y: number } | null>(null);
  const btn = useRef<HTMLButtonElement>(null);
  const label = LIST_MODES.find((m) => m.id === mode)?.label ?? 'State';
  const items: MenuEntry[] = [
    ...LIST_MODES.map((m) => ({
      label: m.label,
      cls: `btn-mode-${m.id}`,
      checked: mode === m.id,
      onSelect: () => {
        if (pid && mode !== m.id) void app.setListMode(pid, m.id);
      },
    })),
    'sep',
    {
      label: 'Reorder…',
      icon: <IconReorder />,
      cls: 'btn-reorder',
      disabled: !pid || app.sortedSessions().length < 2,
      onSelect: onReorder,
    },
  ];
  return (
    <>
      <button
        ref={btn}
        type="button"
        class="btn-mode"
        aria-label={`Session order: ${label}`}
        aria-haspopup="menu"
        aria-expanded={!!at}
        disabled={!pid || disabled}
        onClick={() => setAt(at ? null : below(btn.current))}
      >
        {label}
        <IconChevronDown />
      </button>
      {at && (
        <ContextMenu x={at.x} y={at.y} label="Session order" items={items} anchor={btn.current} backdrop={phone} onClose={() => setAt(null)} />
      )}
    </>
  );
}

/** A project's menu, the same on the project bar (a right click) and in the column's header
 *  ("⋯"): Open in new tab, favorites; Project settings…; Archive project (Restore for an
 *  archived one) and Delete project…, neither for the personal project (SPEC §3). Archiving the
 *  open project opens the personal one; on a phone, the projects screen with its Undo. */
export function projectMenu(app: AppState, project: Project, phone?: boolean): MenuEntry[] {
  const name = projectName(project);
  const items: MenuEntry[] = [
    { label: 'Open in new tab', icon: <IconOpenNew />, cls: 'btn-open-tab', href: `#/p/${project.id}`, newTab: true },
    {
      label: project.favorite_at ? 'Remove from favorites' : 'Add to favorites',
      icon: project.favorite_at ? <IconStarOff /> : <IconStar />,
      cls: 'btn-favorite',
      onSelect: () =>
        void app
          .setProjectFavorite(project.id, !project.favorite_at)
          .catch((e: Error) => app.setListNotice(`Favorites not saved: ${e.message}`)),
    },
    'sep',
    { label: 'Project settings…', icon: <IconSettings />, cls: 'btn-project-settings', href: `#/p/${project.id}/settings` },
  ];
  if (project.archived_at) {
    items.push('sep', {
      label: 'Restore',
      icon: <IconRestore />,
      cls: 'btn-restore-project',
      onSelect: () => void app.restoreProject(project.id).catch((e: Error) => app.setListNotice(`Not restored «${name}»: ${e.message}`)),
    });
  } else if (!project.is_personal) {
    items.push('sep', {
      label: 'Archive project',
      icon: <IconArchive />,
      cls: 'btn-archive-project',
      onSelect: () => {
        const open = app.currentProjectId === project.id;
        app.archiveProject(project.id);
        if (phone) location.hash = '#/';
        else if (open && app.me) location.hash = `#/p/${app.me.personal_project_id}`;
      },
    });
  }
  if (!project.is_personal) {
    items.push({
      label: 'Delete project…',
      icon: <IconTrash />,
      cls: 'btn-delete-project',
      danger: true,
      onSelect: () => void confirmDeleteProject(app, project).catch((e: Error) => app.setListNotice(`Not deleted: ${e.message}`)),
    });
  }
  return items;
}

/** "⋯" of the column's header: the project's menu; on a phone a tap beside it only closes it. */
function ProjectMenu({ app, project, phone }: { app: AppState; project: Project; phone?: boolean }) {
  const [at, setAt] = useState<{ x: number; y: number } | null>(null);
  const btn = useRef<HTMLButtonElement>(null);
  const items = projectMenu(app, project, phone);
  return (
    <>
      <button
        ref={btn}
        type="button"
        class="icon-btn btn-project-menu"
        aria-label="Project: settings, favorites, archive"
        title="Project"
        aria-haspopup="menu"
        aria-expanded={!!at}
        onClick={() => setAt(at ? null : below(btn.current))}
      >
        <IconMore />
      </button>
      {at && (
        <ContextMenu
          x={at.x}
          y={at.y}
          label={`Project «${projectName(project)}»`}
          items={items}
          anchor={btn.current}
          backdrop={phone}
          onClose={() => setAt(null)}
        />
      )}
    </>
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

type CardHandlers = {
  onPointerEnter: (e: PointerEvent) => void;
  onPointerLeave: (e: PointerEvent) => void;
  onPointerDown: () => void;
};

/** A session's state mark: a dot of its group (amber waits, the accent works, a ring is
 *  idle), `</>` while it is open in VS Code and not working there. `color`: the project's own,
 *  for a working dot in a list of several projects, where `--accent` is the last opened one's. */
function StateMark({ s, color }: { s: SessionInfo; color?: string }) {
  const bucket = bucketOf(s);
  if (s.state === 'locked_vscode' && bucket !== 'run') {
    return (
      <span class="dot-vs" aria-hidden="true">
        <IconCode size={12} />
      </span>
    );
  }
  const style = bucket === 'run' && color ? { background: color } : undefined;
  return <span class={`dot dot-${bucket} state-${s.state}`} style={style} aria-hidden="true" />;
}

/** A project's tile on a phone: its icon on its color at ~24 %; `named` where nothing else on
 *  the row names the project for a screen reader. */
export function PhoneTile({ p, named }: { p: Project; named?: boolean }) {
  return (
    <span
      class="phone-tile"
      style={{ background: tileBackground(p) }}
      role={named ? 'img' : undefined}
      aria-label={named ? projectName(p) : undefined}
      aria-hidden={named ? undefined : true}
    >
      <ProjectGlyph p={p} />
    </span>
  );
}

/** A row of the live list, without buttons of its own. Desktop: a right click opens its menu,
 *  the mouse resting on it its card. Phone: a long press or a right click opens the same menu.
 *  In Reorder: a handle (left on a desktop, right on a phone), no menu, no link. Without
 *  `onMenu` (a phone's "Sessions" view) it has no menu; with `project` its project's name
 *  starts the state line and its tile ends the row. */
export function SessionRow({
  s,
  current,
  draft,
  phone,
  when,
  compact,
  project,
  grip,
  dragging,
  menuOpen,
  card,
  onPrefetch,
  onMenu,
}: {
  s: SessionInfo;
  current: boolean;
  /** A message is typed into it and not sent (on this device). */
  draft?: boolean;
  phone?: boolean;
  when?: string;
  /** The Compact look: one line, the state in the dot, the subagents after the title. */
  compact?: boolean;
  project?: Project;
  grip?: GripHandlers;
  dragging: boolean;
  /** Its menu is open: the row stays lit under it. */
  menuOpen?: boolean;
  card?: CardHandlers;
  /** Loads the newest page of the chat before it is opened. */
  onPrefetch: () => void;
  onMenu?: (x: number, y: number) => void;
}) {
  const press = useRef<{ timer: ReturnType<typeof setTimeout>; x: number; y: number } | null>(null);
  const fired = useRef(false);
  const title = titleOf(s);
  const agents = agentsLabel(s.subagents);
  const handle = grip && (
    <button type="button" class="grip" data-grip={s.id} aria-label={`Drag to reorder «${title}», or use arrow keys`} {...grip}>
      <IconGrip />
    </button>
  );
  const body = (
    <>
      <StateMark s={s} color={project && colorHex(project.color)} />
      <span class="session-main">
        <span class="session-top">
          <span class="session-title">{title}</span>
          {draft && !current && <span class="session-draft">Draft</span>}
          {compact && agents && <span class="session-ag">{agents}</span>}
          {when && <span class="session-when">{when}</span>}
          {s.unread > 0 && (
            <span class="badge" aria-label={`unread: ${s.unread}`}>
              {s.unread > 99 ? '99+' : s.unread}
            </span>
          )}
        </span>
        <span class={`session-sub state-${s.state}`}>
          {project && `${projectName(project)} · `}
          {subLabel(s)}
          {agents && <span class="session-agents"> · {agents}</span>}
        </span>
      </span>
      {/* In the Compact look the state line with the project's name is hidden. */}
      {project && <PhoneTile p={project} named />}
    </>
  );
  const cancelPress = () => {
    if (press.current) clearTimeout(press.current.timer);
    press.current = null;
  };
  const handlers = grip || !onMenu
    ? {}
    : phone
      ? {
          onPointerDown: (e: PointerEvent) => {
            fired.current = false;
            cancelPress();
            const x = e.clientX;
            const y = e.clientY;
            press.current = {
              x,
              y,
              timer: setTimeout(() => {
                fired.current = true;
                press.current = null;
                onMenu(x, y);
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
            onMenu(e.clientX, e.clientY);
          },
        }
      : {
          ...card,
          onContextMenu: (e: MouseEvent) => {
            e.preventDefault();
            // From the keyboard (the menu key) there is no pointer: next to the row.
            const r = (e.currentTarget as HTMLElement).getBoundingClientRect();
            onMenu(e.clientX || r.right, e.clientY || r.top);
          },
        };
  useEffect(() => cancelPress, []);
  const cls = `session-row ${current ? 'is-current' : ''} ${menuOpen ? 'is-menu' : ''} ${s.unread > 0 ? 'has-unread' : ''}`;
  return (
    <li class={`session-item ${dragging ? 'is-dragging' : ''}`} {...handlers}>
      {!phone && handle}
      {grip ? (
        <div class={cls}>{body}</div>
      ) : (
        <a
          class={cls}
          href={`#/s/${s.id}`}
          aria-current={current ? 'page' : undefined}
          {...prefetchOn(onPrefetch)}
          onClick={(e) => {
            // The press opened the menu: no navigation on release.
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
    </li>
  );
}

/** The new session open in the main column (`AppState.newSession`), as the group "Draft" on
 *  top of the list: "New session", the current row, and under it "Draft" with where it will
 *  run. No menu, card or archive: it is not on the hub yet. */
function DraftRow({ app }: { app: AppState }) {
  const d = app.newSession!;
  const { worker, path } = newSessionWhere(app, d);
  return (
    <div class="session-group group-draft">
      <div class="group-head">
        <span class="group-label">Draft</span>
      </div>
      <ul class="session-list">
        <li class="session-item">
          <a class="session-row is-current is-draft" href={`#/p/${d.projectId}/new`} aria-current="page">
            <span class="dot dot-idle" aria-hidden="true" />
            <span class="session-main">
              <span class="session-top">
                <span class="session-title">New session</span>
              </span>
              <span class="session-sub">{['Draft', worker, path && homePath(path)].filter(Boolean).join(' · ')}</span>
            </span>
          </a>
        </li>
      </ul>
    </div>
  );
}

/** A desktop row's card in the place of a tooltip, right of the column over the feed: the full
 *  title, the state and how long ago, the worker and the folder, the last line of the turn
 *  once its chat is loaded (resting on the row loads it, `prefetchOn`). */
function SessionCard({ app, s, x, y }: { app: AppState; s: SessionInfo; x: number; y: number }) {
  const [chat] = useState(() => app.chat(s.id));
  useSubscription((fn) => chat.timeline.onChange(fn), [chat]);
  const box = useRef<HTMLDivElement>(null);
  useLayoutEffect(() => {
    if (box.current) fitInView(box.current, x, y);
  }, [x, y]);
  const path = app.sessionPath(s);
  const where = [app.workerName(s.worker_id), path && homePath(path)].filter(Boolean).join(' · ');
  const state = [subLabel(s), agentsLabel(s.subagents), formatAge(s.updated_at, Date.now())].filter(Boolean).join(' · ');
  const last = lastLine(chat.timeline.events());
  return (
    <div class="session-card" role="tooltip" ref={box} style={{ left: `${x}px`, top: `${y}px` }}>
      <div class="session-card-title">{titleOf(s)}</div>
      <div class="session-card-state">
        <StateMark s={s} />
        {state}
      </div>
      {where && <div class="session-card-where mono">{where}</div>}
      {last && <div class="session-card-last">{last}</div>}
    </div>
  );
}

/** The bottom line of a desktop column and of a phone's first screen: how many workers are
 *  online and, for the machine of the open session, how many of its sessions run (the list's
 *  Running rule); "offline" while the socket is down. A click opens the workers' list above
 *  it. */
export function WorkersLine({ app, current }: { app: AppState; current?: string }) {
  const [open, setOpen] = useState(false);
  const box = useRef<HTMLDivElement>(null);
  useEffect(() => {
    const el = box.current;
    const doc = el?.ownerDocument;
    if (!open || !el || !doc) return;
    const away = (e: Event) => {
      if (!el.contains(e.target as Node)) setOpen(false);
    };
    const esc = (e: KeyboardEvent) => {
      if (e.key !== 'Escape') return;
      // Marked handled: a permission card in the open chat skips a prevented Esc.
      e.preventDefault();
      setOpen(false);
    };
    doc.addEventListener('pointerdown', away);
    doc.addEventListener('keydown', esc);
    return () => {
      doc.removeEventListener('pointerdown', away);
      doc.removeEventListener('keydown', esc);
    };
  }, [open]);
  const list = app.workerSessions();
  if (list.length === 0 && !app.offline) return null;
  const online = list.filter((s) => s.state !== 'worker_offline').length;
  const worker = app.sessions.get(current ?? '')?.worker_id;
  const here = app.workerName(worker);
  const live = online > 0 && !app.offline;
  return (
    <div
      class="wline-box"
      ref={box}
      onKeyDown={(e) => {
        // Esc on the line or in the list closes the list and only that (a permission card
        // answers Esc on the document).
        if (!open || e.key !== 'Escape') return;
        e.stopPropagation();
        setOpen(false);
      }}
    >
      {open && list.length > 0 && (
        <div class="wline-pop">
          <WorkerList app={app} current={current} onPick={() => setOpen(false)} />
        </div>
      )}
      <button type="button" class={`wline ${open ? 'is-open' : ''}`} aria-expanded={open} onClick={() => setOpen(!open)}>
        <span class={`dot ${live ? 'dot-online' : 'dot-offline'}`} aria-hidden="true" />
        <span class="wline-count">{app.offline ? 'offline' : `${online} ${online === 1 ? 'worker' : 'workers'} online`}</span>
        {here && !app.offline && (
          <span class="wline-here">
            {here} · {runningLabel(app.workerRunning(worker))}
          </span>
        )}
        <IconChevronUp />
      </button>
    </div>
  );
}

/** The session column's right edge (a desktop's layout puts it between the column and the
 *  chat, off the list's scrollbar): dragged (or ←/→ while focused) to make the column wider or
 *  narrower; the width stays on this device with the look (`DeviceLayout.listWidth`). */
export function WidthGrip({ app }: { app: AppState }) {
  const self = useRef<HTMLDivElement>(null);
  const drag = useRef<{ x: number; w: number; to: number; moved: boolean } | null>(null);
  // The column as it stands: its whole box, the border with it, as the layout's track.
  const widthOf = (el: HTMLElement) => el.parentElement?.querySelector('.sidebar')?.getBoundingClientRect().width ?? 0;
  const announce = (el: HTMLElement) => el.setAttribute('aria-valuenow', String(Math.round(widthOf(el))));
  // The width read out is the one on screen: the default, the dragged one, or less in a
  // narrow window. Measured when the look that sets it changes, after the app has put the look
  // on the root (its layout effect runs after this child's), and again when the grip gets the
  // focus (the window may have been resized since).
  useEffect(() => {
    if (self.current) announce(self.current);
  }, [app.device.listWidth, app.lookName()]);
  const end = () => {
    const d = drag.current;
    drag.current = null;
    if (d?.moved) app.setDevice({ listWidth: d.to });
  };
  return (
    <div
      ref={self}
      class="col-grip"
      role="separator"
      aria-orientation="vertical"
      aria-label="Session list width"
      aria-valuemin={LIST_WIDTH_MIN}
      aria-valuemax={LIST_WIDTH_MAX}
      tabIndex={0}
      onFocus={(e) => announce(e.currentTarget as HTMLElement)}
      onPointerDown={(e) => {
        const el = e.currentTarget as HTMLElement;
        el.setPointerCapture?.(e.pointerId);
        const w = widthOf(el);
        drag.current = { x: e.clientX, w, to: clampListWidth(w), moved: false };
        e.preventDefault();
      }}
      onPointerMove={(e) => {
        const d = drag.current;
        if (!d) return;
        d.to = clampListWidth(d.w + e.clientX - d.x);
        d.moved = true;
        // Live while dragging; stored once on release.
        const el = e.currentTarget as HTMLElement;
        el.ownerDocument.documentElement.style.setProperty('--list-w', `${d.to}px`);
        announce(el);
      }}
      onPointerUp={end}
      onPointerCancel={end}
      onLostPointerCapture={end}
      onKeyDown={(e) => {
        const dir = e.key === 'ArrowLeft' ? -1 : e.key === 'ArrowRight' ? 1 : 0;
        if (!dir) return;
        e.preventDefault();
        app.setDevice({ listWidth: clampListWidth(widthOf(e.currentTarget as HTMLElement) + dir * LIST_WIDTH_STEP) });
      }}
    />
  );
}

/** "Workers": each worker with its online dot and how many of its sessions run (the list's
 *  Running rule); a click opens its service session (SPEC §10.2). Listed by their service
 *  sessions: revoking a worker archives it, so revoked workers are not here. It opens from the
 *  workers line; `onPick` closes it there. */
function WorkerList({ app, current, onPick }: { app: AppState; current?: string; onPick?: () => void }) {
  const list = app.workerSessions();
  if (list.length === 0) return null;
  return (
    <nav class="worker-list" aria-label="Workers">
      <div class="group-title">Workers</div>
      {list.map((s) => {
        const offline = s.state === 'worker_offline';
        const asks = s.state === 'waiting';
        return (
          <a
            key={s.id}
            class={`worker-row ${s.id === current ? 'is-current' : ''}`}
            href={`#/s/${s.id}`}
            title="The worker's service session"
            onClick={onPick}
          >
            <span class={`dot ${offline ? 'dot-offline' : 'dot-online'}`} aria-hidden="true" />
            <span class="worker-name">{s.title}</span>
            {asks ? (
              <span class="worker-asks">waiting for reply</span>
            ) : (
              <span class="worker-state">{offline ? 'offline' : runningLabel(app.workerRunning(s.worker_id))}</span>
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
