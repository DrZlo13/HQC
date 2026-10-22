// `npm run dev` wiring: the Vite dev server proxies REST and WebSocket /api to HQC_HUB_URL.
// Uses the mock as the hub.

import { afterAll, beforeAll, expect, it } from 'vitest';
import { createServer, type ViteDevServer } from 'vite';
import { fileURLToPath } from 'node:url';
// @ts-expect-error plain JS module without types
import { startMock } from '../mock/server.mjs';

let mock: { url: string; close(): Promise<void> };
let vite: ViteDevServer;
let base: string;

beforeAll(async () => {
  mock = await startMock({ speed: 0.02 });
  process.env.HQC_HUB_URL = mock.url;
  vite = await createServer({
    root: fileURLToPath(new URL('..', import.meta.url)),
    server: { port: 0, host: '127.0.0.1' },
    logLevel: 'silent',
  });
  await vite.listen();
  const addr = vite.httpServer!.address() as { port: number };
  base = `127.0.0.1:${addr.port}`;
});

afterAll(async () => {
  await vite?.close();
  await mock?.close();
  delete process.env.HQC_HUB_URL;
});

it('serves the app and proxies REST', async () => {
  const html = await (await fetch(`http://${base}/`)).text();
  expect(html).toContain('<div id="app">');
  const me = await (await fetch(`http://${base}/api/me`)).json();
  expect(me.personal_project_id).toBeTruthy();
  const sw = await fetch(`http://${base}/sw.js`);
  expect(sw.ok).toBe(true);
  const manifest = await (await fetch(`http://${base}/manifest.webmanifest`)).json();
  expect(manifest.display).toBe('standalone');
});

it('proxies the WebSocket', async () => {
  const ws = new WebSocket(`ws://${base}/api/ws`);
  const frame = await new Promise<any>((resolve, reject) => {
    ws.onopen = () => ws.send(JSON.stringify({ type: 'subscribe_summary', payload: {} }));
    ws.onmessage = (m) => resolve(JSON.parse(String(m.data)));
    ws.onerror = () => reject(new Error('ws error'));
  });
  ws.close();
  expect(frame.type).toBe('summary');
  expect(frame.payload.sessions.length).toBeGreaterThan(0);
});
