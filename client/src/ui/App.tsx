import { useEffect, useLayoutEffect, useRef, useState } from 'preact/hooks';
import type { AppState } from '../app';
import { applyLook, applyTheme } from '../theme';
import { Chat } from './Chat';
import { Enroll } from './Enroll';
import { useIsPhone, useSubscription } from './hooks';
import { NewSessionChat } from './NewSession';
import { ProjectRail } from './ProjectRail';
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

/** Routes whose screen is the session list (on a phone, one screen at a time). */
const SIDEBAR_ROUTES = new Set<Route['name']>(['list', 'project']);

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

  // Where the search page goes back to: the last address before it, else the project.
  const beforeSearch = useRef<string | null>(null);
  useEffect(() => {
    if (route.name !== 'search') beforeSearch.current = location.hash || '#/';
  }, [route]);

  // A project opens its last session (on a desktop; a phone's project screen is its list).
  useEffect(() => {
    if (route.name === 'session' && sessionProject) rememberSession(sessionProject, route.id);
  }, [route, sessionProject]);
  const opened = route.name === 'project' && !phone ? route.id : undefined;
  const reopen = opened ? reopenedSession(app, opened) : undefined;
  useEffect(() => {
    if (reopen) location.replace(`#/s/${reopen}`);
  }, [reopen]);

  // The theme (T49): the user's base and the current project's color, on the document root.
  const color = app.currentProject()?.color ?? 'teal';
  const theme = app.theme();
  useLayoutEffect(() => applyTheme(document.documentElement, color, theme), [color, theme]);
  // The look (SPEC §3 User): the user's, Compact or Spacious, with this device's column width.
  const look = app.lookName();
  useLayoutEffect(() => applyLook(document.documentElement, look, app.device), [look, app.device]);

  let main;
  switch (route.name) {
    case 'session':
      main = <Chat key={route.id} app={app} sessionId={route.id} phone={phone} focusSeq={route.seq} />;
      break;
    case 'newSession':
      main = <NewSessionChat key={route.projectId} app={app} projectId={route.projectId} phone={phone} />;
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
  // The search page takes the session column's place and the chat's.
  const page = route.name === 'search';
  return (
    <div class={`layout route-${route.name}`}>
      <ProjectRail app={app} />
      {!page && (
        <aside class="sidebar" aria-label="Project sessions">
          <SessionList app={app} current={current} requests={route.name === 'requests'} />
        </aside>
      )}
      {/* Out of the grid's flow, on the border between the session column and the chat. */}
      {!page && <WidthGrip app={app} />}
      <main class="main">{main}</main>
    </div>
  );
}
