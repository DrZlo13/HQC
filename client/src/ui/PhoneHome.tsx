// A phone's first screen, `#/` (the new screen, stage 6): the projects — those with a session
// that waits for the user, those with one at work, the favorites, then every project in the
// bar's order — or, picked from the title's menu, every project's live sessions by state, each
// with its project's tile. One header row: the title with that menu, Requests, the search, the
// settings and who is signed in; the workers are one line at the bottom. A project opens its
// own screen, `#/p/<id>` (`SessionList`).

import { useRef, useState } from 'preact/hooks';
import type { AppState, PhoneView } from '../app';
import type { Project } from '../proto';
import { projectName } from './Appearance';
import { ContextMenu, type MenuEntry } from './ContextMenu';
import { useSubscription } from './hooks';
import { IconChevronDown, IconPlus, IconSearch, IconSettings } from './icons';
import { ProjectUndo } from './ProjectRail';
import { RequestsLink } from './requests';
import { PhoneTile, SessionRow, WorkersLine, newProject } from './SessionList';
import { bucketOf, groupSessions } from './sessions';

const VIEWS: { id: PhoneView; label: string; detail?: string }[] = [
  { id: 'projects', label: 'Projects' },
  { id: 'sessions', label: 'Sessions', detail: 'every project, by state' },
];

/** A project's sessions that wait for the user, that work and that have unread events. */
interface Counts {
  wait: number;
  run: number;
  unread: number;
}
const NONE: Counts = { wait: 0, run: 0, unread: 0 };

/** The counts of each project, over the sessions the bar's marks count (not archived, not a
 *  service session), by the list's rule (`bucketOf`). */
function projectCounts(app: AppState): Map<string, Counts> {
  const out = new Map<string, Counts>();
  for (const s of app.sessions.values()) {
    if (s.archived || s.system) continue;
    const c = out.get(s.project_id) ?? { ...NONE };
    const b = bucketOf(s);
    if (b === 'wait') c.wait++;
    else if (b === 'run') c.run++;
    if (s.unread > 0) c.unread++;
    out.set(s.project_id, c);
  }
  return out;
}

export function PhoneHome({ app }: { app: AppState }) {
  useSubscription((fn) => app.onChange(fn), [app]);
  const [error, setError] = useState<string | null>(null);
  // The view menu, where it opens: under the title.
  const [at, setAt] = useState<{ x: number; y: number } | null>(null);
  const btn = useRef<HTMLButtonElement>(null);
  const view = app.phoneView;
  const email = app.me?.email ?? '';
  const items: MenuEntry[] = VIEWS.map((v) => ({
    label: v.label,
    detail: v.detail,
    cls: `btn-view-${v.id}`,
    checked: view === v.id,
    onSelect: () => app.setPhoneView(v.id),
  }));
  return (
    <section class={`sessions sessions-phone phone-home phone-home-${view}`}>
      <header class="side-head">
        <button
          ref={btn}
          type="button"
          class="phone-view"
          aria-haspopup="menu"
          aria-expanded={!!at}
          onClick={() => {
            const r = btn.current?.getBoundingClientRect();
            setAt(at ? null : { x: r?.left ?? 0, y: (r?.bottom ?? 0) + 4 });
          }}
        >
          {VIEWS.find((v) => v.id === view)!.label}
          <IconChevronDown size={14} />
        </button>
        <span class="spacer" />
        <RequestsLink app={app} phone />
        <a class="icon-btn btn-search" href="#/search" aria-label="Search sessions and messages">
          <IconSearch />
        </a>
        <a class="icon-btn btn-settings" href="#/settings" aria-label="Settings">
          <IconSettings />
        </a>
        {email && (
          <div class="avatar" title={email} aria-label={email} role="img">
            {email.slice(0, 1).toUpperCase()}
          </div>
        )}
      </header>
      {app.error && <div class="notice">Hub unavailable: {app.error}</div>}
      {error && <div class="notice">{error}</div>}
      {/* Where a phone lands after archiving a project: the hub's refusal, or a failed Undo. */}
      {app.listNotice && (
        <div class="notice" role="alert" onClick={() => app.setListNotice(null)}>
          {app.listNotice}
        </div>
      )}
      <div class="session-scroll">{view === 'projects' ? <Projects app={app} onError={setError} /> : <Sessions app={app} />}</div>
      <ProjectUndo app={app} />
      <WorkersLine app={app} />
      {at && <ContextMenu x={at.x} y={at.y} label="View" items={items} anchor={btn.current} backdrop onClose={() => setAt(null)} />}
    </section>
  );
}

/** The projects in four groups, each in the bar's order: Needs you, Running (not already
 *  there), Favorites, and All projects with "New project" last; a project may be in several. */
function Projects({ app, onError }: { app: AppState; onError: (m: string) => void }) {
  const counts = projectCounts(app);
  const of = (p: Project) => counts.get(p.id) ?? NONE;
  const all = app.sortedProjects();
  const groups = [
    { id: 'wait', label: 'Needs you', items: all.filter((p) => of(p).wait > 0) },
    { id: 'run', label: 'Running', items: all.filter((p) => of(p).wait === 0 && of(p).run > 0) },
    { id: 'favorites', label: 'Favorites', items: all.filter((p) => p.favorite_at) },
    { id: 'all', label: 'All projects', items: all },
  ].filter((g) => g.items.length > 0 || g.id === 'all');
  const spacious = app.lookName() === 'spacious';
  return (
    <>
      {groups.map((g) => (
        <div key={g.id} class={`session-group group-${g.id}`}>
          <div class="group-head">
            <span class="group-label">{g.label}</span>
            <span class="group-count">{g.items.length}</span>
          </div>
          <ul class="session-list">
            {g.items.map((p) => (
              <ProjectRow key={p.id} p={p} c={of(p)} spacious={spacious} />
            ))}
            {g.id === 'all' && (
              <li>
                <button type="button" class="phone-project btn-new-project" disabled={!app.me} onClick={() => void newProject(app, onError)}>
                  <span class="phone-tile phone-tile-new" aria-hidden="true">
                    <IconPlus />
                  </span>
                  <span class="phone-project-main">
                    <span class="phone-project-name">New project</span>
                  </span>
                </button>
              </li>
            )}
          </ul>
        </div>
      ))}
    </>
  );
}

/** A project: its tile, its name (bold with unread events; in the Spacious look "N waiting ·
 *  N running" under it), then the running mark with how many run, the amber count of those
 *  that wait and the count of those with unread events. */
function ProjectRow({ p, c, spacious }: { p: Project; c: Counts; spacious: boolean }) {
  const sub = [c.wait && `${c.wait} waiting`, c.run && `${c.run} running`].filter(Boolean).join(' · ');
  return (
    <li>
      <a class={`phone-project ${c.unread ? 'has-unread' : ''}`} href={`#/p/${p.id}`}>
        <PhoneTile p={p} />
        <span class="phone-project-main">
          <span class="phone-project-name">{projectName(p)}</span>
          {spacious && sub && <span class={`phone-project-sub ${c.wait ? 'is-wait' : ''}`}>{sub}</span>}
        </span>
        {c.run > 0 && (
          <span class="phone-run" aria-label={`running: ${c.run}`}>
            <span class="dot dot-run" aria-hidden="true" />
            {c.run}
          </span>
        )}
        {c.wait > 0 && (
          <span class="sessions-waiting" aria-label={`needs you: ${c.wait}`}>
            {c.wait}
          </span>
        )}
        {c.unread > 0 && (
          <span class="badge" aria-label={`unread: ${c.unread}`}>
            {c.unread}
          </span>
        )}
      </a>
    </li>
  );
}

/** Every live session of the projects on the bar in the groups of State (Needs you, Running,
 *  Idle by recency, none collapsed), each row with its project's tile; a tap opens it. */
function Sessions({ app }: { app: AppState }) {
  const groups = groupSessions(
    app.sortedProjects().flatMap((p) => app.sortedSessions(p.id)),
    'state',
    Date.now(),
  );
  const compact = app.lookName() === 'compact';
  return (
    <>
      {groups.map((g) => (
        <div key={g.id} class={`session-group group-${g.id}`}>
          <div class="group-head">
            <span class="group-label">{g.label}</span>
            <span class="group-count">{g.items.length}</span>
          </div>
          <ul class="session-list">
            {g.items.map((s) => (
              <SessionRow
                key={s.id}
                s={s}
                current={false}
                draft={app.draft(s.id) !== ''}
                phone
                compact={compact}
                project={app.project(s.project_id)}
                dragging={false}
                onPrefetch={() => app.prefetchChat(s.id)}
              />
            ))}
          </ul>
        </div>
      ))}
      {app.me && groups.length === 0 && <div class="empty">No sessions yet</div>}
    </>
  );
}
