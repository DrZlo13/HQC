import { useEffect, useLayoutEffect, useRef, useState } from 'preact/hooks';
import type { AppState } from '../app';
import { replaceHash } from '../nav';
import { applyLook, applyTheme } from '../theme';
import { Chat, SessionsButton, type SessionsToggle } from './Chat';
import { Enroll } from './Enroll';
import { useIsNarrow, useIsPhone, useSubscription } from './hooks';
import { NewSessionChat } from './NewSession';
import { PhoneHome } from './PhoneHome';
import { ProjectRail, onMac } from './ProjectRail';
import { ProjectSettings } from './ProjectSettings';
import { RequestsScreen } from './requests';
import { RulesScreen } from './Rules';
import { SearchPage } from './Search';
import { SessionList, WidthGrip } from './SessionList';
import { rememberSession, reopenedSession } from './sessions';
import { Settings } from './Settings';

export type Route =
  | { name: 'list' }
  | { name: 'project'; id: string }
  | { name: 'newSession'; projectId: string }
  | { name: 'projectSettings'; projectId: string }
  | { name: 'rules'; projectId: string; workspaceId: string; sessionId?: string }
  | { name: 'session'; id: string; seq?: number }
  | { name: 'search' }
  | { name: 'requests' }
  | { name: 'settings' }
  | { name: 'enroll'; code: string };

/** Hash routes, so the hub can serve the client as static files: `#/p/<project>`,
 *  `#/p/<project>/new` (a new session, not on the hub until its first message),
 *  `#/p/<project>/settings` (the project's settings, T49), `#/p/<project>/rules/<workspace>`
 *  (`?session=<id>`: opened from that session; the folder's rules, T48), `#/s/<session>` (`?seq=<seq>`:
 *  scrolled to that event, a search hit), `#/search` (the search page; `?q=…` and its filters,
 *  read by the page),
 *  `#/requests` (every open request, T37), `#/settings`, `#/enroll?code=ABCD-1234`. */
export function parseRoute(hash: string): Route {
  const [path, query = ''] = hash.replace(/^#/, '').split('?');
  const parts = path.split('/').filter(Boolean);
  if (parts[0] === 's' && parts[1]) {
    const seq = Number(new URLSearchParams(query).get('seq') ?? NaN);
    const id = decodeURIComponent(parts[1]);
    return Number.isInteger(seq) && seq > 0 ? { name: 'session', id, seq } : { name: 'session', id };
  }
  if (parts[0] === 'search') return { name: 'search' };
  if (parts[0] === 'requests') return { name: 'requests' };
  if (parts[0] === 'p' && parts[1]) {
    const id = decodeURIComponent(parts[1]);
    if (parts[2] === 'rules' && parts[3]) {
      const sessionId = new URLSearchParams(query).get('session') ?? undefined;
      return { name: 'rules', projectId: id, workspaceId: decodeURIComponent(parts[3]), sessionId };
    }
    if (parts[2] === 'new') return { name: 'newSession', projectId: id };
    return parts[2] === 'settings' ? { name: 'projectSettings', projectId: id } : { name: 'project', id };
  }
  if (parts[0] === 'settings') return { name: 'settings' };
  if (parts[0] === 'enroll') return { name: 'enroll', code: new URLSearchParams(query).get('code') ?? '' };
  return { name: 'list' };
}

export function App({ app }: { app: AppState }) {
  const [route, setRoute] = useState(() => parseRoute(location.hash));
  const phone = useIsPhone();
  const narrow = useIsNarrow();
  useSubscription((fn) => app.onChange(fn), [app]);
  // Where Settings goes back to: the address it was opened from, as it was then (the search
  // page keeps its query in the address without a hash change), not its worker-code page.
  const beforeSettings = useRef<string | null>(null);
  useEffect(() => {
    const on = (e: HashChangeEvent) => {
      const next = parseRoute(location.hash);
      const from = e.oldURL ? new URL(e.oldURL).hash || '#/' : null;
      const prev = from === null ? null : parseRoute(from).name;
      if (next.name === 'settings' && prev && prev !== 'settings' && prev !== 'enroll') beforeSettings.current = from;
      setRoute(next);
    };
    addEventListener('hashchange', on);
    return () => removeEventListener('hashchange', on);
  }, []);
  useEffect(() => {
    // Phones suspend sockets in the background; reconnect as soon as we are back.
    const nudge = () => document.visibilityState === 'visible' && app.socket.nudge();
    document.addEventListener('visibilitychange', nudge);
    addEventListener('online', nudge);
    return () => {
      document.removeEventListener('visibilitychange', nudge);
      removeEventListener('online', nudge);
    };
  }, [app]);

  // The left column follows the project of what is open.
  const sessionProject = route.name === 'session' ? app.sessions.get(route.id)?.project_id : undefined;
  const routeProject =
    route.name === 'project'
      ? route.id
      : route.name === 'projectSettings' || route.name === 'rules' || route.name === 'newSession'
        ? route.projectId
        : sessionProject;
  // The first render already shows the route's project: effects run after the first paint,
  // and a remembered list would show the personal project for that frame.
  useState(() => {
    if (routeProject) app.selectProject(routeProject);
  });
  useEffect(() => {
    if (routeProject) app.selectProject(routeProject);
  }, [app, routeProject]);

  // Where the search page goes back to: the last address before it, else the project; not
  // Settings, whose arrow may lead back to the search.
  const beforeSearch = useRef<string | null>(null);
  useEffect(() => {
    if (route.name !== 'search' && route.name !== 'settings' && route.name !== 'enroll') beforeSearch.current = location.hash || '#/';
  }, [route]);

  // A project opens its last session (on a desktop; a phone's project screen is its list).
  useEffect(() => {
    if (route.name === 'session' && sessionProject) rememberSession(sessionProject, route.id);
  }, [route, sessionProject]);
  const opened = route.name === 'project' && !phone ? route.id : undefined;
  const reopen = opened ? reopenedSession(app, opened) : undefined;
  useEffect(() => {
    if (reopen) replaceHash(`#/s/${reopen}`);
  }, [reopen]);

  // The theme (T49): the user's base and the current project's color, on the document root.
  const color = app.currentProject()?.color ?? 'teal';
  const theme = app.theme();
  useLayoutEffect(() => applyTheme(document.documentElement, color, theme), [color, theme]);
  // The look (SPEC §3 User): the user's, Compact or Spacious, with this device's column width.
  const look = app.lookName();
  useLayoutEffect(() => applyLook(document.documentElement, look, app.device), [look, app.device]);

  // A window 900–1079 px wide: the session column is out of the layout; "Sessions" in the
  // header, or ⌘\ (Ctrl+\ off a Mac), slides it out over the chat. Any route change (a session
  // picked, a new one, a project, a page), Esc, a click beside it or leaving that width puts it
  // away.
  const [slid, setSlid] = useState(false);
  useLayoutEffect(() => setSlid(false), [route, narrow]);
  useEffect(() => {
    if (!narrow) return;
    // On the window, after the document's handlers: an Esc a menu of the list has taken
    // (prevented, or stopped on its way) closes that menu only.
    const key = (e: KeyboardEvent) => {
      const mod = onMac() ? e.metaKey && !e.ctrlKey : e.ctrlKey && !e.metaKey;
      if (mod && !e.altKey && !e.shiftKey && (e.key === '\\' || e.code === 'Backslash')) {
        e.preventDefault();
        setSlid((s) => !s);
      } else if (e.key === 'Escape' && !e.defaultPrevented && document.querySelector('.sidebar-slide')) {
        // Taken: the browser would also clear a search field the focus is in.
        e.preventDefault();
        setSlid(false);
      }
    };
    addEventListener('keydown', key);
    return () => removeEventListener('keydown', key);
  }, [narrow]);
  const sessions: SessionsToggle | undefined = narrow
    ? {
        open: slid,
        waiting: app.currentProjectId ? app.projectWaiting(app.currentProjectId) : 0,
        toggle: () => setSlid(!slid),
      }
    : undefined;

  let main;
  switch (route.name) {
    case 'session':
      main = <Chat key={route.id} app={app} sessionId={route.id} phone={phone} focusSeq={route.seq} sessions={sessions} />;
      break;
    case 'newSession':
      main = <NewSessionChat key={route.projectId} app={app} projectId={route.projectId} phone={phone} sessions={sessions} />;
      break;
    case 'search': {
      const pid = app.currentProjectId;
      main = <SearchPage app={app} phone={phone} back={beforeSearch.current ?? (pid ? `#/p/${pid}` : '#/')} />;
      break;
    }
    case 'requests':
      main = <RequestsScreen app={app} phone={phone} />;
      break;
    case 'projectSettings':
      main = <ProjectSettings key={route.projectId} app={app} projectId={route.projectId} phone={phone} />;
      break;
    case 'rules':
      main = (
        <RulesScreen
          key={route.workspaceId}
          app={app}
          projectId={route.projectId}
          workspaceId={route.workspaceId}
          sessionId={route.sessionId}
          phone={phone}
        />
      );
      break;
    case 'settings':
      {
        // On a computer "#/" is an empty placeholder, and its arrow is hidden: the project then.
        const from = beforeSettings.current;
        const back = from && (phone || from !== '#/') ? from : app.currentProjectId ? `#/p/${app.currentProjectId}` : '#/';
        main = <Settings app={app} back={back} />;
      }
      break;
    case 'enroll':
      main = <Enroll app={app} initialCode={route.code} />;
      break;
    default:
      main = (
        <>
          {/* No list on the left in a narrow window: the column slides out from here. */}
          {sessions && (
            <header class="chat-head">
              <SessionsButton sessions={sessions} />
            </header>
          )}
          <div class="placeholder">
            {sessions ? 'Open Sessions to pick a session or start a new one' : 'Pick a session on the left or start a new one'}
          </div>
        </>
      );
  }
  const current = route.name === 'session' ? route.id : undefined;
  // Phone: one screen at a time (T26, Mobile-List). The projects (or every project's sessions)
  // first, a project's sessions, then whatever opens from there.
  if (phone) {
    return (
      <div class={`layout layout-phone route-${route.name}`}>
        {route.name === 'list' ? (
          <PhoneHome app={app} />
        ) : route.name === 'project' ? (
          <SessionList app={app} current={current} phone />
        ) : (
          <main class="main">{main}</main>
        )}
      </div>
    );
  }
  // The search page takes the session column's place and the chat's; a narrow window has no
  // column in the grid, only the one slid out over the chat.
  const column = !(route.name === 'search' || narrow);
  return (
    <div class={`layout${narrow ? ' layout-narrow' : ''} route-${route.name}`}>
      <ProjectRail app={app} />
      {column && (
        <aside class="sidebar" aria-label="Project sessions">
          <SessionList app={app} current={current} requests={route.name === 'requests'} />
        </aside>
      )}
      {/* Out of the grid's flow, on the border between the session column and the chat. */}
      {column && <WidthGrip app={app} />}
      <main class="main">{main}</main>
      {narrow && slid && (
        <>
          <div class="sessions-scrim" onClick={() => setSlid(false)} />
          {/* A session picked puts it away, the open one too (its address does not change). */}
          <aside
            class="sidebar sidebar-slide"
            aria-label="Project sessions"
            onClick={(e) => {
              // A link of the list in this tab (a row, the draft, a worker) or "+", even to the
              // address already open. Not a click whose navigation another handler prevented, a
              // click inside a menu ("Open in new tab") or ⌘/Ctrl/Shift+click.
              const t = e.target as Element;
              if (e.defaultPrevented || e.metaKey || e.ctrlKey || e.shiftKey || t.closest?.('[role="menu"]')) return;
              if (t.closest?.('a[href^="#/"], .btn-new-session')) setSlid(false);
            }}
          >
            <SessionList app={app} current={current} requests={route.name === 'requests'} />
          </aside>
        </>
      )}
    </div>
  );
}
