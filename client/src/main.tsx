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
render(<App app={app} />, document.getElementById('app')!);
watchTabIcon(app);
void app.init();
void registerServiceWorker();
listenForNavigation();
