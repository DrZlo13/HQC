// @vitest-environment happy-dom
// "Settings" → "Feed" (SPEC 0.90): the full-width feed and the system font are kept in this
// browser and set as classes on the document root, which styles.css reads.

import { afterEach, expect, it } from 'vitest';
import { render } from 'preact';
import { act } from 'preact/test-utils';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import type { Api } from '../src/api';
import { AppState } from '../src/app';
import { applyLook } from '../src/theme';
import type { HubSocket } from '../src/ws';
import { Settings } from '../src/ui/Settings';

let root: HTMLElement;
afterEach(() => {
  if (root) render(null, root);
  document.body.innerHTML = '';
  document.documentElement.className = '';
  localStorage.clear();
});

const newApp = () =>
  new AppState({ workers: async () => [] } as unknown as Api, { onState: () => {} } as unknown as HubSocket);

it('keeps the full-width feed and the system font on this device', async () => {
  const app = newApp();
  expect(app.look).toEqual({ wideFeed: false, systemFont: false, railNames: false });
  root = document.createElement('div');
  document.body.appendChild(root);
  await act(async () => render(<Settings app={app} />, root));
  const wide = root.querySelector<HTMLInputElement>('input[name=wide-feed]')!;
  const font = root.querySelector<HTMLInputElement>('input[name=system-font]')!;
  expect([wide.checked, font.checked]).toEqual([false, false]);

  await act(async () => wide.click());
  expect(app.look).toEqual({ wideFeed: true, systemFont: false, railNames: false });
  await act(async () => font.click());
  expect(app.look).toEqual({ wideFeed: true, systemFont: true, railNames: false });

  // A reload reads it back, and the document root carries it.
  const again = newApp();
  expect(again.look).toEqual({ wideFeed: true, systemFont: true, railNames: false });
  applyLook(document.documentElement, again.look);
  expect(document.documentElement.classList.contains('feed-wide')).toBe(true);
  expect(document.documentElement.classList.contains('font-system')).toBe(true);
  again.setLook({ wideFeed: false });
  applyLook(document.documentElement, again.look);
  expect(document.documentElement.classList.contains('feed-wide')).toBe(false);
  expect(newApp().look).toEqual({ wideFeed: false, systemFont: true, railNames: false });
});

it('shows what this device keeps; broken storage is the default look', async () => {
  localStorage.setItem('hqc.look', '{"wideFeed":true}');
  root = document.createElement('div');
  document.body.appendChild(root);
  await act(async () => render(<Settings app={newApp()} />, root));
  expect(root.querySelector<HTMLInputElement>('input[name=wide-feed]')!.checked).toBe(true);
  expect(root.querySelector<HTMLInputElement>('input[name=system-font]')!.checked).toBe(false);
  localStorage.setItem('hqc.look', '{not json');
  expect(newApp().look).toEqual({ wideFeed: false, systemFont: false, railNames: false });
});

it('the classes lift the feed width and swap the font', () => {
  const css = readFileSync(join(import.meta.dirname, '../src/styles.css'), 'utf8');
  expect(css).toMatch(/:root\.feed-wide \.feed \{\s*max-width: none;/);
  expect(css).toMatch(/:root\.font-system \{\s*--sans: -apple-system,/);
});

it('keeps the project names in the project bar on this device (SPEC 0.91)', async () => {
  root = document.createElement('div');
  document.body.appendChild(root);
  const app = newApp();
  await act(async () => render(<Settings app={app} />, root));
  const names = root.querySelector<HTMLInputElement>('input[name=rail-names]')!;
  expect(names.checked).toBe(false);
  await act(async () => names.click());
  expect(app.look).toEqual({ wideFeed: false, systemFont: false, railNames: true });
  // A reload reads it back; the root's class widens the bar's column.
  const again = newApp();
  expect(again.look.railNames).toBe(true);
  applyLook(document.documentElement, again.look);
  expect(document.documentElement.classList.contains('rail-names')).toBe(true);
  const css = readFileSync(join(import.meta.dirname, '../src/styles.css'), 'utf8');
  expect(css).toMatch(/:root\.rail-names \.layout \{\s*grid-template-columns: 260px /);
});
