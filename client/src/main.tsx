import { render } from 'preact';
import { createApp } from './app';
import { watchTabIcon } from './favicon';
import { trackHistory } from './nav';
import { listenForNavigation, registerServiceWorker } from './push';
import { listenForTaps } from './taps';
import { applyTheme, storedTheme } from './theme';
import { App } from './ui/App';
import './styles.css';

declare global {
  interface Window {
    /** For a desktop wrapper of the client. */
    hqc: { newSessionInProject: () => void };
  }
}

// The base last applied on this device (Warm without one) until /api/me answers (T49); App
// recomputes it with the user's theme and project.
applyTheme(document.documentElement, 'teal', storedTheme());
const app = createApp();
// What a desktop wrapper's global shortcut calls: a new session in the project picked in Settings.
window.hqc = { newSessionInProject: () => app.openNewSessionInProject() };
// Before the first render: a kept list (`hqc.list`) the render throws on is still replaced by
// the hub's answer, so the next load works.
void app.init();
// Before the app listens to the address: its entries are stamped first.
trackHistory();
// Every touch reaches the page, and a tap on a button or a link presses it on the finger's lift:
// on an iPhone a tap lost its click when the page rendered a button or a link during it (taps.ts).
listenForTaps();
render(<App app={app} />, document.getElementById('app')!);
watchTabIcon(app);
void registerServiceWorker();
listenForNavigation();
