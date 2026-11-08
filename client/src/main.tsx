import { render } from 'preact';
import { createApp } from './app';
import { watchTabIcon } from './favicon';
import { listenForNavigation, registerServiceWorker } from './push';
import { applyTheme, storedTheme } from './theme';
import { App } from './ui/App';
import './styles.css';

// The base last applied on this device (Warm without one) until /api/me answers (T49); App
// recomputes it with the user's theme and project.
applyTheme(document.documentElement, 'teal', storedTheme());
const app = createApp();
// Before the first render: a kept list (`hqc.list`) the render throws on is still replaced by
// the hub's answer, so the next load works.
void app.init();
render(<App app={app} />, document.getElementById('app')!);
watchTabIcon(app);
void registerServiceWorker();
listenForNavigation();
