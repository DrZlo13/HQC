import { render } from 'preact';
import { createApp } from './app';
import { listenForNavigation, registerServiceWorker } from './push';
import { applyTheme } from './theme';
import { App } from './ui/App';
import './styles.css';

// Warm until /api/me answers (T49); App recomputes it with the user's theme and project.
applyTheme(document.documentElement, 'teal', 'warm');
const app = createApp();
render(<App app={app} />, document.getElementById('app')!);
void app.init();
void registerServiceWorker();
listenForNavigation();
