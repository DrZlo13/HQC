import { useEffect, useLayoutEffect, useState } from 'preact/hooks';
import type { AppState } from '../app';
import { applyLook, applyTheme } from '../theme';
import { Chat } from './Chat';
import { Enroll } from './Enroll';
import { useIsPhone, useSubscription } from './hooks';
import { ProjectRail } from './ProjectRail';
import { ProjectSettings } from './ProjectSettings';
import { RequestsScreen } from './requests';
import { RulesScreen } from './Rules';
import { SearchScreen } from './Search';
import { SessionList } from './SessionList';
import { Settings } from './Settings';

export type Route =
  | { name: 'list' }
  | { name: 'project'; id: string }
  | { name: 'projectSettings'; projectId: string }
  | { name: 'rules'; projectId: string; workspaceId: string; sessionId?: string }
  | { name: 'session'; id: string; seq?: number }
  | { name: 'search' }
  | { name: 'requests' }
  | { name: 'settings' }
  | { name: 'enroll'; code: string };

/** Hash routes, so the hub can serve the client as static files: `#/p/<project>`,
 *  `#/p/<project>/settings` (the project's settings, T49), `#/p/<project>/rules/<workspace>`
 *  (`?session=<id>`: opened from that session; the folder's rules, T48), `#/s/<session>` (`?seq=<seq>`:
 *  scrolled to that event, a search hit), `#/search` (the phone's search screen),
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
    return parts[2] === 'settings' ? { name: 'projectSettings', projectId: id } : { name: 'project', id };
  }
  if (parts[0] === 'settings') return { name: 'settings' };
  if (parts[0] === 'enroll') return { name: 'enroll', code: new URLSearchParams(query).get('code') ?? '' };
  return { name: 'list' };
}

/** Routes whose screen is the session list (on a phone, one screen at a time). */
const SIDEBAR_ROUTES = new Set<Route['name']>(['list', 'project']);

/** The session last open in each project, on this device: a convenience, so storage that
 *  fails or is empty only means the project opens without one. */
const LAST_KEY = 'hqc.lastSession';
function lastSessions(): Record<string, string> {
  try {
    return JSON.parse(localStorage.getItem(LAST_KEY) ?? '{}') ?? {};
  } catch {
    return {};
  }
}
function rememberSession(projectId: string, sessionId: string) {
  try {
    localStorage.setItem(LAST_KEY, JSON.stringify({ ...lastSessions(), [projectId]: sessionId }));
  } catch {
    // Not kept: the project opens without it.
  }
}

export function App({ app }: { app: AppState }) {
  const [route, setRoute] = useState(() => parseRoute(location.hash));
  const phone = useIsPhone();
  useSubscription((fn) => app.onChange(fn), [app]);
  useEffect(() => {
    const on = () => setRoute(parseRoute(location.hash));
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
      : route.name === 'projectSettings' || route.name === 'rules'
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

  // A project opens its last session (on a desktop; a phone's project screen is its list).
  useEffect(() => {
    if (route.name === 'session' && sessionProject) rememberSession(sessionProject, route.id);
  }, [route, sessionProject]);
  const opened = route.name === 'project' && !phone ? route.id : undefined;
  const last = opened ? app.sessions.get(lastSessions()[opened] ?? '') : undefined;
  const reopen = last && last.project_id === opened && !last.archived ? last.id : undefined;
  useEffect(() => {
    if (reopen) location.replace(`#/s/${reopen}`);
  }, [reopen]);

  // The theme (T49): the user's base and the current project's color, on the document root.
  const color = app.currentProject()?.color ?? 'teal';
  const theme = app.theme();
  useLayoutEffect(() => applyTheme(document.documentElement, color, theme), [color, theme]);
  useLayoutEffect(() => applyLook(document.documentElement, app.look), [app.look]);

  let main;
  switch (route.name) {
    case 'session':
      main = <Chat key={route.id} app={app} sessionId={route.id} phone={phone} focusSeq={route.seq} />;
      break;
    case 'search':
      // A phone screen; a desktop has the search in the left column.
      main = phone ? <SearchScreen app={app} /> : <div class="placeholder">Search from the field on the left</div>;
      break;
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
      main = <Settings app={app} />;
      break;
    case 'enroll':
      main = <Enroll app={app} initialCode={route.code} />;
      break;
    default:
      main = <div class="placeholder">Pick a session on the left or start a new one</div>;
  }
  const current = route.name === 'session' ? route.id : undefined;
  // Phone: the list is the first screen, everything else replaces it (T26, Mobile-List).
  if (phone) {
    return (
      <div class={`layout layout-phone route-${route.name}`}>
        {SIDEBAR_ROUTES.has(route.name) ? (
          <SessionList app={app} current={current} phone />
        ) : (
          <main class="main">{main}</main>
        )}
      </div>
    );
  }
  return (
    <div class={`layout route-${route.name}`}>
      <ProjectRail app={app} />
      <aside class="sidebar" aria-label="Project sessions">
        <SessionList app={app} current={current} requests={route.name === 'requests'} />
      </aside>
      <main class="main">{main}</main>
    </div>
  );
}
