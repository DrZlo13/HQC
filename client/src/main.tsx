import { render } from 'preact';
import { createApp } from './app';
import { listenForNavigation, registerServiceWorker } from './push';
import { App } from './ui/App';
import './styles.css';

const app = createApp();
render(<App app={app} />, document.getElementById('app')!);
void app.init();
void registerServiceWorker();
listenForNavigation();
