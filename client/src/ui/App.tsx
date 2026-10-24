import { useEffect, useState } from 'preact/hooks';
import type { AppState } from '../app';
import { Chat } from './Chat';
import { Enroll } from './Enroll';
import { useIsPhone, useSubscription } from './hooks';
import { ProjectRail } from './ProjectRail';
import { SessionList } from './SessionList';
import { Settings } from './Settings';
import { Workspaces } from './Workspaces';

export type Route =
  | { name: 'list' }
  | { name: 'project'; id: string }
  | { name: 'workspaces'; projectId: string }
  | { name: 'session'; id: string }
  | { name: 'settings' }
  | { name: 'enroll'; code: string };

/** Hash routes, so the hub can serve the client as static files: `#/p/<project>`,
 *  `#/p/<project>/workspaces`, `#/s/<session>`, `#/settings`, `#/enroll?code=ABCD-1234`. */
export function parseRoute(hash: string): Route {
  const [path, query = ''] = hash.replace(/^#/, '').split('?');
  const parts = path.split('/').filter(Boolean);
  if (parts[0] === 's' && parts[1]) return { name: 'session', id: decodeURIComponent(parts[1]) };
  if (parts[0] === 'p' && parts[1]) {
    const id = decodeURIComponent(parts[1]);
    return parts[2] === 'workspaces' ? { name: 'workspaces', projectId: id } : { name: 'project', id };
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
    route.name === 'project' ? route.id : route.name === 'workspaces' ? route.projectId : sessionProject;
  useEffect(() => {
    if (routeProject) app.selectProject(routeProject);
  }, [app, routeProject]);

  let main;
  switch (route.name) {
    case 'session':
      main = <Chat key={route.id} app={app} sessionId={route.id} phone={phone} />;
      break;
    case 'workspaces':
      main = <Workspaces key={route.projectId} app={app} projectId={route.projectId} />;
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
        <SessionList app={app} current={current} />
      </aside>
      <main class="main">{main}</main>
    </div>
  );
}
