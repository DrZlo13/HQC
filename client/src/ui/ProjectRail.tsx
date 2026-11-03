import { useLayoutEffect, useRef, useState } from 'preact/hooks';
import type { AppState } from '../app';
import { ProjectGlyph, projectName, tileBackground } from './Appearance';
import { useSubscription } from './hooks';
import { IconCheck, IconCross, IconPlus, IconReorder, IconSettings } from './icons';
import { newProject } from './SessionList';
import { moveTo } from './sessions';

/** A tile and the gap under it: how far a drag moves a project by one place. */
const TILE_STEP = 54;

/** The project bar of a desktop (T49, README "Project bar"): a tile per project with its
 *  icon on its color at ~24 %; the current one more saturated (`--mark-bg`) with a pill of
 *  the project color at the left edge; an amber "needs you" badge; "+ project", Reorder
 *  (drag the tiles or move a focused one with ↑/↓, then Done or Cancel), settings and who is
 *  signed in. */
export function ProjectRail({ app }: { app: AppState }) {
  useSubscription((fn) => app.onChange(fn), [app]);
  const [error, setError] = useState<string | null>(null);
  const current = app.currentProjectId;
  const email = app.me?.email ?? '';
  // Reorder mode: the order being arranged; Done stores it, Cancel drops it.
  const [order, setOrder] = useState<string[] | null>(null);
  const drag = useRef<{ id: string; y: number; base: string[] } | null>(null);
  const [dragId, setDragId] = useState<string | null>(null);
  const focusTile = useRef<string | null>(null);
  const nav = useRef<HTMLElement>(null);
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
      setOrder(moveTo(d.base, d.id, d.base.indexOf(d.id) + Math.round((e.clientY - d.y) / TILE_STEP)));
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
  return (
    <nav ref={nav} class={`rail ${order ? 'is-reordering' : ''}`} aria-label="Projects">
      <a class="rail-logo" href="#/" title="HQC">
        <img src="/hqc-mark-small.svg" alt="HQC" width="38" height="38" />
      </a>
      <div class="rail-sep" />
      {projects.map((p) => {
        const name = projectName(p);
        const waiting = app.projectWaiting(p.id);
        const on = p.id === current;
        return (
          <div key={p.id} class={`rail-item ${dragId === p.id ? 'is-dragging' : ''}`}>
            {on && <span class="rail-pill" aria-hidden="true" />}
            {order ? (
              <span
                class={`project-row rail-project rail-drag ${on ? 'is-current' : ''}`}
                role="button"
                aria-label={`Drag to reorder «${name}», or use arrow keys`}
                title={name}
                style={on ? undefined : { background: tileBackground(p) }}
                {...tile(p.id)}
              >
                <ProjectGlyph p={p} />
              </span>
            ) : (
            <a
              class={`project-row rail-project ${on ? 'is-current' : ''}`}
              href={`#/p/${p.id}`}
              aria-label={name}
              aria-current={on ? 'true' : undefined}
              title={name}
              style={on ? undefined : { background: tileBackground(p) }}
            >
              <ProjectGlyph p={p} />
            </a>
            )}
            {waiting > 0 && (
              <span class="rail-badge" aria-label={`needs you: ${waiting}`}>
                {waiting > 99 ? '99+' : waiting}
              </span>
            )}
          </div>
        );
      })}
      {order ? (
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
      )}
      <div class="spacer" />
      <a class="rail-btn" href="#/settings" aria-label="Settings" title="Settings">
        <IconSettings size={18} />
      </a>
      {email && (
        <div class="avatar" title={email} aria-label={email} role="img">
          {email.slice(0, 1).toUpperCase()}
        </div>
      )}
    </nav>
  );
}
