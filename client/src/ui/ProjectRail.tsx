import { useEffect, useLayoutEffect, useRef, useState } from 'preact/hooks';
import type { AppState } from '../app';
import type { Project, ProjectSort } from '../proto';
import { ProjectGlyph, projectName, tileBackground } from './Appearance';
import { ContextMenu, type MenuEntry } from './ContextMenu';
import { useSubscription } from './hooks';
import { IconArchive, IconCheck, IconCross, IconPlus, IconReorder, IconSearch, IconSettings, IconStar } from './icons';
import { newProject, prefetchOn, projectMenu } from './SessionList';
import { moveTo, reopenedSession } from './sessions';

/** A tile and the gap under it: how far a drag moves a project by one place. */
const TILE_STEP = 54;
/** The same for a row with the project's name (the Compact look). */
const ROW_STEP = 40;

/** The bar's orders (SPEC §3 User), as the order menu lists them. */
const PROJECT_SORTS: { id: ProjectSort; label: string }[] = [
  { id: 'custom', label: 'Custom' },
  { id: 'name', label: 'Name' },
  { id: 'recent', label: 'Recent' },
];

/** Where `query` first stands in `name`, ignoring case: the part before it, it, and the part
 *  after it; null when it is not there. */
export function matchName(name: string, query: string): [string, string, string] | null {
  const q = query.trim().toLocaleLowerCase();
  const i = name.toLocaleLowerCase().indexOf(q);
  return i < 0 ? null : [name.slice(0, i), name.slice(i, i + q.length), name.slice(i + q.length)];
}

/** How many times faster than its 18 s lap (the CSS) the stripe around a working project runs
 *  at `rate` output tokens per second: twice at 40, about 2.8 times at 100, at most 6 (a lap
 *  in 3 s). */
export function runSpeed(rate: number): number {
  return Math.min(6, 1 + Math.log2(1 + rate / 40));
}

/** The hover text of a working project's tile: its name, the agents at work in it (each
 *  session's own and its subagents) and their output tokens per second. */
export function runningTitle(name: string, agents: number, rate: number): string {
  return `${name} · ${agents} agent${agents === 1 ? '' : 's'} · ${rate} tokens/s`;
}

/** The stripe around a working project's tile, at [`runSpeed`] of the project's rate. The
 *  speed changes through the playback rate of its CSS animation, so the stripe goes on from
 *  where it is: a new `animation-duration` would make it jump. `quiet` where the row already
 *  says how many sessions run (a phone's project list): no label of its own. */
export function RailRun({ rate, quiet }: { rate: number; quiet?: boolean }) {
  const el = useRef<HTMLSpanElement>(null);
  const k = runSpeed(rate);
  useEffect(() => {
    for (const a of el.current?.getAnimations() ?? []) a.updatePlaybackRate(k);
  }, [k]);
  return <span ref={el} class="rail-run" aria-label={quiet ? undefined : 'working'} aria-hidden={quiet ? true : undefined} />;
}

/** A Mac (or an iPad with a keyboard): ⌘ is the shortcuts' key there, Ctrl elsewhere. */
export function onMac(): boolean {
  return /Mac|iPhone|iPad/.test(navigator.platform || navigator.userAgent);
}

/** The project bar of a desktop (T49, README "Project bar"): a tile per project with its
 *  icon on its color at ~24 %; the current one more saturated (`--mark-bg`) with a pill of
 *  the project color at the left edge. A mark only when something goes on there: an amber
 *  count of the sessions that wait for the user, else a stripe running around the tile while one
 *  works, else a blue dot (and, with the names on, a bold name) while one has unread events;
 *  an idle project has none. A favorite project also has a star on the tile's bottom right
 *  corner. "+ project", the order menu (Custom, Name, Recent, and Reorder…: drag the tiles or
 *  move a focused one with ↑/↓, then Done, which makes the order Custom, or Cancel), settings
 *  and who is signed in. A project's right click opens its menu (`projectMenu`); archiving one
 *  shows "Archived «…» · Undo" at the bottom. The search
 *  (its magnifier, or ⌘K / Ctrl+K) finds projects by name, archived ones too: with the names
 *  on (the Compact look) the field takes the place of "HQC" and the list shrinks to
 *  the matches; with tiles the bar opens 290 px wide over the column, with the names. */
export function ProjectRail({ app }: { app: AppState }) {
  useSubscription((fn) => app.onChange(fn), [app]);
  const [error, setError] = useState<string | null>(null);
  const names = app.lookName() === 'compact';
  const step = names ? ROW_STEP : TILE_STEP;
  const current = app.currentProjectId;
  const email = app.me?.email ?? '';
  // Reorder mode: the order being arranged; Done stores it, Cancel drops it.
  const [order, setOrder] = useState<string[] | null>(null);
  const drag = useRef<{ id: string; y: number; base: string[] } | null>(null);
  const [dragId, setDragId] = useState<string | null>(null);
  const focusTile = useRef<string | null>(null);
  const nav = useRef<HTMLElement>(null);
  // A project's context menu, where it was opened.
  const [menu, setMenu] = useState<{ id: string; x: number; y: number } | null>(null);
  const menuProject = menu && app.project(menu.id);
  // The order menu, where it opens: right of its button.
  const [sortAt, setSortAt] = useState<{ x: number; y: number } | null>(null);
  const sortBtn = useRef<HTMLButtonElement>(null);
  const sort = app.projectSort();
  // The project search: open or not, what is typed, the match ↑/↓ stand on; with the names on,
  // the bar's width when it opened, kept while the list shrinks.
  const [searching, setSearching] = useState(false);
  const [query, setQuery] = useState('');
  const [sel, setSel] = useState(0);
  const [findWidth, setFindWidth] = useState<number | undefined>();
  const field = useRef<HTMLInputElement>(null);
  const searchBtn = useRef<HTMLButtonElement>(null);
  // Esc closed the search: the focus goes back to the magnifier.
  const toSearchBtn = useRef(false);
  useLayoutEffect(() => {
    const id = focusTile.current;
    focusTile.current = null;
    if (id) nav.current?.querySelector<HTMLElement>(`[data-tile="${id}"]`)?.focus();
  }, [order]);
  const openSearch = () => {
    if (order) return;
    if (searching) {
      field.current?.focus();
      return;
    }
    setFindWidth(nav.current?.getBoundingClientRect().width || undefined);
    setQuery('');
    setSel(0);
    setSearching(true);
  };
  const closeSearch = (toButton = false) => {
    toSearchBtn.current = toButton;
    setSearching(false);
    setQuery('');
  };
  useEffect(() => {
    if (searching) field.current?.focus();
    else if (toSearchBtn.current) searchBtn.current?.focus();
    toSearchBtn.current = false;
  }, [searching]);
  // ⌘K on a Mac, Ctrl+K elsewhere, opens the search from anywhere (on a Mac Ctrl+K deletes to
  // the end of the line in a field); a press outside the bar closes it.
  useEffect(() => {
    const doc = nav.current?.ownerDocument;
    if (!doc) return;
    const key = (e: KeyboardEvent) => {
      const mod = onMac() ? e.metaKey && !e.ctrlKey : e.ctrlKey && !e.metaKey;
      if (!mod || e.altKey || e.shiftKey || e.key.toLowerCase() !== 'k') return;
      e.preventDefault();
      openSearch();
    };
    const away = (e: Event) => {
      if (searching && !nav.current?.contains(e.target as Node)) closeSearch();
    };
    doc.addEventListener('keydown', key);
    doc.addEventListener('pointerdown', away);
    return () => {
      doc.removeEventListener('keydown', key);
      doc.removeEventListener('pointerdown', away);
    };
  }, [searching, order]);
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
  // The order arranged is the user's own: the bar shows it from now on.
  const done = () => {
    if (order) {
      app.saveProjectOrder(order).catch((e: Error) => setError(e.message));
      app.setProjectSort('custom').catch((e: Error) => setError(e.message));
    }
    setOrder(null);
  };
  const sortItems: MenuEntry[] = [
    ...PROJECT_SORTS.map((m) => ({
      label: m.label,
      cls: `btn-sort-${m.id}`,
      checked: sort === m.id,
      onSelect: () => void app.setProjectSort(m.id).catch((e: Error) => setError(e.message)),
    })),
    'sep',
    {
      label: 'Reorder…',
      icon: <IconReorder />,
      cls: 'btn-reorder-projects',
      // The user's own order is what gets arranged, whichever the bar shows.
      onSelect: () => setOrder(app.sortedProjects('custom').map((p) => p.id)),
    },
  ];
  // A right click on a project (on the bar or among the matches); from the keyboard (the menu
  // key) there is no pointer: next to the project.
  const menuOn = (id: string) => (e: MouseEvent) => {
    e.preventDefault();
    const r = (e.currentTarget as HTMLElement).getBoundingClientRect();
    setMenu({ id, x: e.clientX || r.right, y: e.clientY || r.top });
  };
  const projects = order
    ? order.map((id) => app.projects.get(id)).filter((p) => !!p)
    : app.sortedProjects();

  // The matches: the projects on the bar, then the archived ones once something is typed.
  const live = app.sortedProjects();
  const hits = searching
    ? [...live, ...(query.trim() ? [...app.archivedProjects.values()] : [])]
        .map((p) => ({ p, m: matchName(projectName(p), query) }))
        .filter((h): h is { p: Project; m: [string, string, string] } => !!h.m)
    : [];
  const at = Math.min(sel, hits.length - 1);
  const openHit = (p: Project) => {
    closeSearch();
    location.hash = `#/p/${p.id}`;
  };
  const find = (
    <label class="rail-find">
      <IconSearch size={14} />
      <input
        ref={field}
        type="search"
        aria-label="Search projects"
        placeholder="Search projects"
        value={query}
        onInput={(e) => {
          setQuery((e.currentTarget as HTMLInputElement).value);
          setSel(0);
        }}
        onKeyDown={(e) => {
          if (e.isComposing) return;
          if (e.key === 'ArrowDown' || e.key === 'ArrowUp') {
            e.preventDefault();
            if (hits.length) setSel((at + (e.key === 'ArrowDown' ? 1 : hits.length - 1)) % hits.length);
            return;
          }
          if (e.key !== 'Escape' && e.key !== 'Enter') return;
          // The field's keys: a permission card in the open chat answers Enter and Esc on the
          // document.
          e.preventDefault();
          e.stopPropagation();
          if (e.key === 'Escape') closeSearch(true);
          else if (hits[at]) openHit(hits[at].p);
        }}
      />
      {query.trim() && (
        <span class="rail-find-count">
          {hits.length} of {live.length + app.archivedProjects.size}
        </span>
      )}
    </label>
  );
  const results = (
    <div class="rail-hits" role="listbox" aria-label="Projects found">
      {hits.map(({ p, m }, i) => (
        <a
          key={p.id}
          class={`rail-hit ${i === at ? 'is-sel' : ''} ${p.archived_at ? 'is-archived' : ''} ${menu?.id === p.id ? 'is-menu' : ''}`}
          href={`#/p/${p.id}`}
          role="option"
          aria-selected={i === at}
          onClick={() => closeSearch()}
          onContextMenu={menuOn(p.id)}
        >
          <span class="rail-glyph" style={{ background: tileBackground(p) }}>
            <ProjectGlyph p={p} />
          </span>
          <span class="rail-name">
            {m[0]}
            {m[1] && <mark>{m[1]}</mark>}
            {m[2]}
          </span>
          {p.archived_at && <span class="rail-tag">archived</span>}
        </a>
      ))}
      {hits.length === 0 && <div class="rail-hits-none">No projects match</div>}
    </div>
  );
  const hint = <div class="rail-hint">↑↓ select · ↵ open · Esc close</div>;
  const searchButton = !order && (
    <button
      ref={searchBtn}
      type="button"
      class="rail-btn rail-search"
      aria-label="Search projects"
      title={`Search projects (${onMac() ? '⌘K' : 'Ctrl+K'})`}
      onClick={openSearch}
    >
      <IconSearch size={names ? 14 : 16} />
    </button>
  );
  const logo = <img src="/hqc-mark-small.svg" alt={names ? '' : 'HQC'} width="38" height="38" />;

  // "+ project" and the order menu, or Done and Cancel: under the tiles; with the project names
  // on, in the bottom row with the settings and the avatar.
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
        onClick={() => {
          closeSearch();
          void newProject(app, setError);
        }}
      >
        <IconPlus size={15} />
      </button>
      {app.projects.size > 1 && (
        <button
          ref={sortBtn}
          type="button"
          class="rail-btn rail-order"
          disabled={!app.me}
          aria-label={`Project order: ${PROJECT_SORTS.find((m) => m.id === sort)?.label}`}
          aria-haspopup="menu"
          aria-expanded={!!sortAt}
          title="Project order"
          onClick={() => {
            // The search gives the bar back first: an order and Reorder are for the whole list.
            closeSearch();
            const r = sortBtn.current?.getBoundingClientRect();
            setSortAt(sortAt ? null : { x: (r?.right ?? 0) + 4, y: r?.top ?? 0 });
          }}
        >
          <IconReorder size={16} />
        </button>
      )}
    </>
  );
  const inPlace = names && searching;
  return (
    <nav
      ref={nav}
      class={`rail ${order ? 'is-reordering' : ''} ${inPlace ? 'is-searching' : ''}`}
      aria-label="Projects"
      style={inPlace && findWidth ? { width: `${findWidth}px` } : undefined}
      onKeyDown={(e) => {
        // On a match (focused by a click or by its closed menu): Esc closes the search too, and
        // Enter follows the link without reaching a permission card in the open chat (it answers
        // Enter on the document and would cancel the link). The field and an open menu handle
        // their own keys.
        if (!searching || e.defaultPrevented) return;
        if (e.key === 'Enter') e.stopPropagation();
        if (e.key !== 'Escape') return;
        e.preventDefault();
        e.stopPropagation();
        closeSearch(true);
      }}
    >
      <div class="rail-head">
        <a class="rail-logo" href="#/" title="HQC">
          {logo}
          {names && !searching && <span class="rail-title">HQC</span>}
        </a>
        {inPlace ? find : searchButton}
      </div>
      <div class="rail-sep" />
      {inPlace
        ? results
        : projects.map((p) => {
            const name = projectName(p);
            const waiting = app.projectWaiting(p.id);
            const works = app.projectRunning(p.id);
            const running = waiting === 0 && works;
            const unread = waiting === 0 && !running && app.projectUnread(p.id);
            const title = works ? runningTitle(name, app.projectAgents(p.id), app.projectRate(p.id)) : name;
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
            const cls = `project-row rail-project ${on ? 'is-current' : ''} ${unread ? 'has-unread' : ''}`;
            return (
              <div key={p.id} class={`rail-item ${dragId === p.id ? 'is-dragging' : ''}`}>
                {on && <span class="rail-pill" aria-hidden="true" />}
                {order ? (
                  <span
                    class={`${cls} rail-drag`}
                    role="button"
                    aria-label={`Drag to reorder «${name}», or use arrow keys`}
                    title={title}
                    style={background}
                    {...tile(p.id)}
                  >
                    {face}
                  </span>
                ) : (
                  <a
                    class={`${cls} ${menu?.id === p.id ? 'is-menu' : ''}`}
                    href={`#/p/${p.id}`}
                    aria-label={name}
                    aria-current={on ? 'true' : undefined}
                    title={title}
                    style={background}
                    {...prefetchOn(() => {
                      // The chat the project opens with.
                      const id = reopenedSession(app, p.id);
                      if (id) app.prefetchChat(id);
                    })}
                    onContextMenu={menuOn(p.id)}
                  >
                    {face}
                  </a>
                )}
                {waiting > 0 ? (
                  <span class="rail-badge" aria-label={`needs you: ${waiting}`}>
                    {waiting > 99 ? '99+' : waiting}
                  </span>
                ) : running ? (
                  <RailRun rate={app.projectRate(p.id)} />
                ) : (
                  unread && <span class="rail-unread" aria-label="unread" />
                )}
                {p.favorite_at && (
                  <span class="rail-fav" aria-label="favorite">
                    <IconStar size={10} />
                  </span>
                )}
              </div>
            );
          })}
      {!names && <div class="rail-actions">{actions}</div>}
      {menu && menuProject && (
        <ContextMenu
          x={menu.x}
          y={menu.y}
          label={`Project «${projectName(menuProject)}»`}
          // A tile has no name: the menu says whose it is (a match of the search shows its own).
          heading={names || searching ? undefined : projectName(menuProject)}
          items={projectMenu(app, menuProject)}
          onClose={() => setMenu(null)}
        />
      )}
      {sortAt && (
        <ContextMenu x={sortAt.x} y={sortAt.y} label="Project order" items={sortItems} anchor={sortBtn.current} onClose={() => setSortAt(null)} />
      )}
      <div class="spacer" />
      {inPlace && hint}
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
      {!names && searching && (
        // The tiles have no room for a field: the bar opens over the column, with the names.
        <div class="rail-pop">
          <div class="rail-head">
            <span class="rail-logo">{logo}</span>
            {find}
          </div>
          {results}
          <div class="spacer" />
          {hint}
        </div>
      )}
      <ProjectUndo app={app} hint=" · find it with the search on the bar" />
    </nav>
  );
}

/** "Archived «…» · Undo" for a project archived here a moment ago: at the bottom of the bar on
 *  a desktop, of the projects screen on a phone (which has no bar, so no `hint`). */
export function ProjectUndo({ app, hint = '' }: { app: AppState; hint?: string }) {
  const undone = app.projectUndo ? app.archivedProjects.get(app.projectUndo) : undefined;
  if (!undone) return null;
  return (
    <div class="toast toast-archive toast-project" role="status">
      <IconArchive />
      <span class="toast-text">
        Archived «{projectName(undone)}»{hint}
      </span>
      <button
        type="button"
        class="btn-link btn-undo"
        onClick={() =>
          void app.restoreProject(undone.id).catch((e: Error) => app.setListNotice(`Not restored «${projectName(undone)}»: ${e.message}`))
        }
      >
        Undo
      </button>
    </div>
  );
}
