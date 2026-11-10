import { useEffect, useLayoutEffect, useRef, useState } from 'preact/hooks';
import type { AppState } from '../app';
import { colorHex } from '../theme';
import { ProjectGlyph, projectName, tileBackground } from './Appearance';
import { useSubscription } from './hooks';
import { IconCheck, IconCross, IconPlus, IconReorder, IconSettings } from './icons';
import { newProject } from './SessionList';
import { moveTo } from './sessions';

/** A tile and the gap under it: how far a drag moves a project by one place. */
const TILE_STEP = 54;
/** The same for a row with the project's name (`Look.railNames`). */
const ROW_STEP = 40;

/** The project bar of a desktop (T49, README "Project bar"): a tile per project with its
 *  icon on its color at ~24 %; the current one more saturated (`--mark-bg`) with a pill of
 *  the project color at the left edge; its state, as a session row's dot: an amber "needs
 *  you" badge, else a pulsing dot of its color while a session works, else a dot when a
 *  session has unread events, else an empty ring; "+ project", Reorder
 *  (drag the tiles or move a focused one with ↑/↓, then Done or Cancel), settings and who is
 *  signed in. A tile's context menu adds the project to the favorites, which go first.
 *  With "Settings" → "Project bar" → names on, a row per project: its icon and full name. */
export function ProjectRail({ app }: { app: AppState }) {
  useSubscription((fn) => app.onChange(fn), [app]);
  const [error, setError] = useState<string | null>(null);
  const names = app.look.railNames;
  const step = names ? ROW_STEP : TILE_STEP;
  const current = app.currentProjectId;
  const email = app.me?.email ?? '';
  // Reorder mode: the order being arranged; Done stores it, Cancel drops it.
  const [order, setOrder] = useState<string[] | null>(null);
  const drag = useRef<{ id: string; y: number; base: string[] } | null>(null);
  const [dragId, setDragId] = useState<string | null>(null);
  const focusTile = useRef<string | null>(null);
  const nav = useRef<HTMLElement>(null);
  // A tile's context menu, where it was opened; fixed, the bar clips what overflows it.
  const [menu, setMenu] = useState<{ id: string; x: number; y: number } | null>(null);
  const menuBox = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (!menu) return;
    const doc = menuBox.current?.ownerDocument;
    if (!doc) return;
    menuBox.current?.querySelector<HTMLElement>('.menu-item')?.focus();
    const away = (e: Event) => {
      if (!menuBox.current?.contains(e.target as Node)) setMenu(null);
    };
    const esc = (e: KeyboardEvent) => e.key === 'Escape' && setMenu(null);
    doc.addEventListener('pointerdown', away);
    doc.addEventListener('keydown', esc);
    return () => {
      doc.removeEventListener('pointerdown', away);
      doc.removeEventListener('keydown', esc);
    };
  }, [menu]);
  const menuProject = menu && app.projects.get(menu.id);
  useLayoutEffect(() => {
    const id = focusTile.current;
    focusTile.current = null;
    if (id) nav.current?.querySelector<HTMLElement>(`[data-tile="${id}"]`)?.focus();
  }, [order]);
  const endDrag = () => {
    drag.current = null;
    setDragId(null);
  };
  const tile = (id: string) => ({
    'data-tile': id,
    tabIndex: 0,
    onPointerDown: (e: PointerEvent) => {
      if (!order) return;
      (e.currentTarget as HTMLElement).setPointerCapture?.(e.pointerId);
      drag.current = { id, y: e.clientY, base: order };
      e.preventDefault();
      setDragId(id);
    },
    onPointerMove: (e: PointerEvent) => {
      const d = drag.current;
      if (!d) return;
      setOrder(moveTo(d.base, d.id, d.base.indexOf(d.id) + Math.round((e.clientY - d.y) / step)));
    },
    onPointerUp: endDrag,
    onPointerCancel: endDrag,
    onLostPointerCapture: endDrag,
    onKeyDown: (e: KeyboardEvent) => {
      const dir = e.key === 'ArrowUp' ? -1 : e.key === 'ArrowDown' ? 1 : 0;
      if (!dir || !order) return;
      e.preventDefault();
      focusTile.current = id;
      setOrder(moveTo(order, id, order.indexOf(id) + dir));
    },
  });
  const done = () => {
    if (order) app.saveProjectOrder(order).catch((e: Error) => setError(e.message));
    setOrder(null);
  };
  const projects = order
    ? order.map((id) => app.projects.get(id)).filter((p) => !!p)
    : app.sortedProjects();
  // "+ project" and Reorder, or Done and Cancel: under the tiles; with the project names on, in
  // the bottom row with the settings and the avatar.
  const actions = order ? (
    <>
      <button type="button" class="rail-btn rail-done" aria-label="Done" title="Save the order" onClick={done}>
        <IconCheck size={16} />
      </button>
      <button type="button" class="rail-btn" aria-label="Cancel" title="Keep the old order" onClick={() => setOrder(null)}>
        <IconCross size={14} />
      </button>
    </>
  ) : (
    <>
      <button
        type="button"
        class="rail-btn rail-new"
        aria-label="New project"
        title={error ?? 'New project'}
        disabled={!app.me}
        onClick={() => void newProject(app, setError)}
      >
        <IconPlus size={15} />
      </button>
      {app.projects.size > 1 && (
        <button
          type="button"
          class="rail-btn rail-reorder"
          aria-label="Reorder projects"
          title="Reorder projects"
          onClick={() => setOrder(app.sortedProjects().map((p) => p.id))}
        >
          <IconReorder size={16} />
        </button>
      )}
    </>
  );
  return (
    <nav ref={nav} class={`rail ${order ? 'is-reordering' : ''}`} aria-label="Projects">
      <a class="rail-logo" href="#/" title="HQC">
        <img src="/hqc-mark-small.svg" alt={names ? '' : 'HQC'} width="38" height="38" />
        {names && <span class="rail-title">HQC</span>}
      </a>
      <div class="rail-sep" />
      {projects.map((p) => {
        const name = projectName(p);
        const waiting = app.projectWaiting(p.id);
        const unread = waiting === 0 && app.projectUnread(p.id);
        const on = p.id === current;
        // A row keeps the icon on its color and puts the name after it; a tile is the icon.
        const face = names ? (
          <>
            <span class="rail-glyph" style={{ background: tileBackground(p) }}>
              <ProjectGlyph p={p} />
            </span>
            <span class="rail-name">{name}</span>
          </>
        ) : (
          <ProjectGlyph p={p} />
        );
        const background = on || names ? undefined : { background: tileBackground(p) };
        return (
          <div key={p.id} class={`rail-item ${dragId === p.id ? 'is-dragging' : ''}`}>
            {on && <span class="rail-pill" aria-hidden="true" />}
            {order ? (
              <span
                class={`project-row rail-project rail-drag ${on ? 'is-current' : ''}`}
                role="button"
                aria-label={`Drag to reorder «${name}», or use arrow keys`}
                title={name}
                style={background}
                {...tile(p.id)}
              >
                {face}
              </span>
            ) : (
            <a
              class={`project-row rail-project ${on ? 'is-current' : ''}`}
              href={`#/p/${p.id}`}
              aria-label={name}
              aria-current={on ? 'true' : undefined}
              title={name}
              style={background}
              onContextMenu={(e) => {
                e.preventDefault();
                // From the keyboard (the menu key) there is no pointer: next to the tile.
                const r = (e.currentTarget as HTMLElement).getBoundingClientRect();
                setMenu({ id: p.id, x: e.clientX || r.right, y: e.clientY || r.top });
              }}
            >
              {face}
            </a>
            )}
            {waiting > 0 ? (
              <span class="rail-badge" aria-label={`needs you: ${waiting}`}>
                {waiting > 99 ? '99+' : waiting}
              </span>
            ) : app.projectRunning(p.id) ? (
              <span class="rail-run pulse" style={{ background: colorHex(p.color) }} aria-label="working" />
            ) : unread ? (
              <span class="rail-unread" aria-label="unread" />
            ) : (
              <span class="rail-idle" aria-label="idle" />
            )}
          </div>
        );
      })}
      {!names && <div class="rail-actions">{actions}</div>}
      {menu && menuProject && (
        <div class="menu rail-menu" role="menu" ref={menuBox} style={{ left: `${menu.x}px`, top: `${menu.y}px` }}>
          <button
            type="button"
            role="menuitem"
            class="menu-item btn-favorite"
            onClick={() => {
              setMenu(null);
              app
                .setProjectFavorite(menuProject.id, !menuProject.favorite_at)
                .catch((e: Error) => app.setListNotice(`Favorites not saved: ${e.message}`));
            }}
          >
            {menuProject.favorite_at ? 'Remove from favorites' : 'Add to favorites'}
          </button>
        </div>
      )}
      <div class="spacer" />
      <div class="rail-foot">
      {names && actions}
      <a class="rail-btn" href="#/settings" aria-label="Settings" title="Settings">
        <IconSettings size={18} />
      </a>
      {email && (
        <div class="avatar" title={email} aria-label={email} role="img">
          {email.slice(0, 1).toUpperCase()}
        </div>
      )}
      </div>
    </nav>
  );
}
