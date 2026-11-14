// The whole UI against the mock hub: the real <App> in a happy-dom window, with Node's
// fetch and WebSocket for the network. Open a session from the list, write, watch the
// deltas and tool cards, press the approval button, see the response and the final answer.

import { afterAll, beforeAll, expect, it } from 'vitest';
import { render } from 'preact';
import { act } from 'preact/test-utils';
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
  // Inside `act` the effects' cleanups run before it returns: unmounted plainly, preact runs
  // them after a frame or a timeout, which could come after the globals below are gone.
  act(() => render(null, root));
  app.socket.stop();
  await mock.close();
  await win.happyDOM.close();
  restoreGlobals();
});

it('runs the iteration 1 scenario through the UI', async () => {
  // Session list with state and unread.
  const row = await until('session list', () =>
    $$('.session-row').find((r) => r.textContent?.includes('Empty session')),
  );
  expect(row.textContent).toContain('Idle');
  expect($$('.session-row .badge').length).toBeGreaterThan(0);

  // Open the session.
  win.location.hash = row.getAttribute('href')!;
  await until('chat', () => $('.chat textarea'));
  await until('history', () => $('.line-system'));

  // Write.
  const area = $<HTMLTextAreaElement>('.chat textarea')!;
  area.value = 'Run the tests';
  area.dispatchEvent(new win.Event('input', { bubbles: true }) as unknown as Event);
  await until('send enabled', () => !$<HTMLButtonElement>('.composer button[type=submit]')!.disabled);
  $<HTMLButtonElement>('.composer button[type=submit]')!.click();
  await until('optimistic or echoed message', () =>
    $$('.msg-user').find((b) => b.textContent?.includes('Run the tests')),
  );

  // Deltas, then tool cards, then the approval with one button per option.
  await until('streaming', () => $('.msg-streaming'));
  await until('tool group', () => $('.tool-group'));
  const buttons = await until('approval buttons', () => {
    const b = $$('.perm button[data-option]');
    return b.length ? b : null;
  });
  // One button per option: "Allow" first, "Deny" last (T26).
  expect(buttons.map((b) => b.childNodes[0].textContent)).toEqual([
    'Allow',
    'Always in this session',
    'Always allow',
    'Always deny',
    'Deny',
  ]);
  expect($('.perm-what')!.textContent).toBe('$ cargo test');
  expect($('.chat .state')!.textContent).toBe('Waiting for permission');
  expect($('.msg-pending')).toBeNull();

  // Press "allow once": the response comes back as approval.response, the card folds.
  buttons[0].click();
  await until('approval.response', () => $('.perm-done-row'));
  expect($('.perm-done-row')!.textContent).toContain('$ cargo test');
  expect($('.perm-done-row')!.textContent).toContain('· allowed');
  expect($$('.perm button[data-option]')).toHaveLength(0);

  // Final answer.
  await until('final answer', () => $$('.msg-final').find((b) => b.textContent?.includes('All 42 tests passed.')));
  await until('idle', () => $('.chat .state')?.textContent?.includes('Idle'));
  expect($('.msg-streaming')).toBeNull();
  expect($$('.tool-group.tool-ok').length).toBeGreaterThanOrEqual(1);
  // The final answer carries the turn's usage, the header the model and the cost.
  expect($$('.msg-final').at(-1)!.querySelector('.usage')!.textContent).toContain('tokens');
  expect($('.chat-model')!.textContent).toBe('Sonnet 5.5');
  expect($('.chat-cost')!.textContent).toMatch(/^\$0\.\d+ this session$/);
  expect($('.chat-path')!.textContent).toContain('~/work/hqc');
});

it('renders a markdown answer, streamed and final', async () => {
  const area = $<HTMLTextAreaElement>('.chat textarea')!;
  area.value = 'Show markdown';
  area.dispatchEvent(new win.Event('input', { bubbles: true }) as unknown as Event);
  await until('send enabled', () => !$<HTMLButtonElement>('.composer button[type=submit]')!.disabled);
  $<HTMLButtonElement>('.composer button[type=submit]')!.click();

  await until('streamed heading', () => $('.msg-streaming h2'));
  const final = await until('final markdown answer', () =>
    $$('.msg-final').find((b) => b.querySelector('table')),
  );
  await until('idle', () => $('.chat .state')?.textContent?.includes('Idle'));
  expect(final.querySelector('h2')!.textContent).toBe('Run summary');
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
  // T19: a revoked worker stays in the settings with "Delete"; elsewhere it is gone, and
  // its service session leaves the "Workers" group.
  const revoked = await until('worker revoked', () =>
    $$('.plain-list li').find((li) => li.textContent?.includes('home-mac') && li.textContent.includes('revoked')),
  );
  expect((await app.api.workers()).some((w) => w.name === 'home-mac')).toBe(false);
  await until('workers line gone', () => !$('.wline'));
  // Its sessions are still there: the hub refuses and the screen says what to do.
  revoked.querySelector<HTMLButtonElement>('.btn-purge')!.click();
  const err = await until('has sessions', () => $('.page-body .error-text'));
  expect(err.textContent).toContain('still has 3 unarchived sessions');
});
