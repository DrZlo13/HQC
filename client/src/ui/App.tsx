import { useEffect, useState } from 'preact/hooks';
import type { AppState } from '../app';
import { Chat } from './Chat';
import { Enroll } from './Enroll';
import { useSubscription } from './hooks';
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

/** Routes whose screen is the left column (on a phone, one screen at a time). */
const SIDEBAR_ROUTES = new Set<Route['name']>(['list', 'project']);

export function App({ app }: { app: AppState }) {
  const [route, setRoute] = useState(() => parseRoute(location.hash));
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
      main = <Chat key={route.id} app={app} sessionId={route.id} />;
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
      main = <div class="placeholder muted">Выберите сессию</div>;
  }
  const sidebar = SIDEBAR_ROUTES.has(route.name) ? 'route-list' : '';
  return (
    <div class={`layout route-${route.name} ${sidebar}`}>
      <aside class="sidebar">
        <SessionList app={app} current={route.name === 'session' ? route.id : undefined} />
      </aside>
      <main class="main">{main}</main>
    </div>
  );
}
