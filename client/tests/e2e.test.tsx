// The whole UI against the mock hub: the real <App> in a happy-dom window, with Node's
// fetch and WebSocket for the network. Open a session from the list, write, watch the
// deltas and tool cards, press the approval button, see the response and the final answer.

import { afterAll, beforeAll, expect, it } from 'vitest';
import { render } from 'preact';
import { Window } from 'happy-dom';
// @ts-expect-error plain JS module without types
import { startMock } from '../mock/server.mjs';
import { Api } from '../src/api';
import { AppState } from '../src/app';
import { App } from '../src/ui/App';
import { HubSocket } from '../src/ws';

let mock: { url: string; close(): Promise<void> };
let win: Window;
let app: AppState;
let root: HTMLElement;
const saved: Record<string, PropertyDescriptor | undefined> = {};

// The UI reads a few browser globals; point them at the happy-dom window for this file.
function installGlobals(w: Window) {
  const g = globalThis as Record<string, unknown>;
  const map: Record<string, unknown> = {
    window: w,
    document: w.document,
    location: w.location,
    addEventListener: w.addEventListener.bind(w),
    removeEventListener: w.removeEventListener.bind(w),
    prompt: () => 'e2e',
    confirm: () => true,
    // preact/hooks runs effects after the next animation frame.
    requestAnimationFrame: w.requestAnimationFrame.bind(w),
    cancelAnimationFrame: w.cancelAnimationFrame.bind(w),
  };
  for (const [k, v] of Object.entries(map)) {
    saved[k] = Object.getOwnPropertyDescriptor(g, k);
    Object.defineProperty(g, k, { value: v, configurable: true, writable: true });
  }
}

function restoreGlobals() {
  const g = globalThis as Record<string, unknown>;
  for (const [k, d] of Object.entries(saved)) {
    if (d) Object.defineProperty(g, k, d);
    else delete g[k];
  }
}

async function until<T>(what: string, fn: () => T | null | undefined | false, ms = 5000): Promise<T> {
  const end = Date.now() + ms;
  for (;;) {
    const v = fn();
    if (v) return v;
    if (Date.now() > end) throw new Error(`timeout waiting for ${what}\n${root.textContent}`);
    await new Promise((r) => setTimeout(r, 10));
  }
}

const $ = <E extends Element = HTMLElement>(sel: string) => root.querySelector<E>(sel);
const $$ = (sel: string) => [...root.querySelectorAll<HTMLElement>(sel)];

beforeAll(async () => {
  mock = await startMock({ speed: 0.05 });
  win = new Window({ url: 'http://hqc.test/' });
  installGlobals(win);
  app = new AppState(
    new Api(mock.url, fetch),
    new HubSocket({ url: mock.url.replace('http', 'ws') + '/api/ws', WebSocketImpl: WebSocket }),
  );
  root = win.document.createElement('div') as unknown as HTMLElement;
  win.document.body.appendChild(root as never);
  render(<App app={app} />, root);
  await app.init();
});

afterAll(async () => {
  render(null, root);
  app.socket.stop();
  await mock.close();
  await win.happyDOM.close();
  restoreGlobals();
});

it('runs the iteration 1 scenario through the UI', async () => {
  // Session list with state and unread.
  const row = await until('session list', () =>
    $$('.session-row').find((r) => r.textContent?.includes('Пустая сессия')),
  );
  expect(row.textContent).toContain('свободна');
  expect($$('.session-row .badge').length).toBeGreaterThan(0);

  // Open the session.
  win.location.hash = row.getAttribute('href')!;
  await until('chat', () => $('.chat textarea'));
  await until('history', () => $('.line-system'));

  // Write.
  const area = $<HTMLTextAreaElement>('.chat textarea')!;
  area.value = 'Запусти тесты';
  area.dispatchEvent(new win.Event('input', { bubbles: true }) as unknown as Event);
  await until('send enabled', () => !$<HTMLButtonElement>('.composer button[type=submit]')!.disabled);
  $<HTMLButtonElement>('.composer button[type=submit]')!.click();
  await until('optimistic or echoed message', () =>
    $$('.bubble-user').find((b) => b.textContent?.includes('Запусти тесты')),
  );

  // Deltas, then tool cards, then the approval with one button per option.
  await until('streaming', () => $('.bubble-streaming'));
  await until('tool card', () => $('.card-tool'));
  const buttons = await until('approval buttons', () => {
    const b = $$('.card-approval button');
    return b.length ? b : null;
  });
  expect(buttons.map((b) => b.textContent)).toEqual([
    'Разрешить',
    'Разрешить в сессии',
    'Разрешать всегда',
    'Отклонить',
    'Запрещать всегда',
  ]);
  expect($('.chat .state')!.textContent).toContain('ждёт ответа');
  expect($('.bubble-pending')).toBeNull();

  // Press "allow once": the response comes back as approval.response, buttons go away.
  buttons[0].click();
  await until('approval.response', () => $('.line-approval'));
  expect($('.line-approval')!.textContent).toContain('Bash: Разрешить');
  expect($$('.card-approval button')).toHaveLength(0);

  // Final answer.
  await until('final answer', () => $$('.bubble-final').find((b) => b.textContent?.includes('Все 42 теста прошли.')));
  await until('idle', () => $('.chat .state')?.textContent?.includes('свободна'));
  expect($('.bubble-streaming')).toBeNull();
  expect($$('.card-tool.tool-ok').length).toBeGreaterThanOrEqual(2);
});

it('renders a markdown answer, streamed and final', async () => {
  const area = $<HTMLTextAreaElement>('.chat textarea')!;
  area.value = 'Покажи markdown';
  area.dispatchEvent(new win.Event('input', { bubbles: true }) as unknown as Event);
  await until('send enabled', () => !$<HTMLButtonElement>('.composer button[type=submit]')!.disabled);
  $<HTMLButtonElement>('.composer button[type=submit]')!.click();

  await until('streamed heading', () => $('.bubble-streaming h2'));
  const final = await until('final markdown answer', () =>
    $$('.bubble-final').find((b) => b.querySelector('table')),
  );
  await until('idle', () => $('.chat .state')?.textContent?.includes('свободна'));
  expect(final.querySelector('h2')!.textContent).toBe('Итоги прогона');
  expect(final.querySelectorAll('table tbody tr')).toHaveLength(3);
  expect(final.querySelector('.md-code-lang')!.textContent).toBe('rust');
  expect(final.querySelector('a[href="https://example.com/ci/42"]')).not.toBeNull();
  expect(final.querySelector('img, script')).toBeNull();
  expect(final.textContent).toContain('<img src=x onerror=alert(1)>');
  expect(final.textContent).toContain('[javascript:](javascript:alert(1))');
});

it('revokes a worker from the settings screen', async () => {
  win.location.hash = '#/settings';
  const row = await until('worker list', () =>
    $$('.plain-list li').find((li) => li.textContent?.includes('home-mac')),
  );
  row.querySelector<HTMLButtonElement>('.btn-revoke')!.click();
  await until('worker gone', () => !$$('.plain-list li').some((li) => li.textContent?.includes('home-mac')));
  expect((await app.api.workers()).some((w) => w.name === 'home-mac')).toBe(false);
});
