// @vitest-environment happy-dom
// "Settings" → "Feed" (SPEC 0.90) and "Session list" (SPEC 0.95): the full-width feed, the system
// font, the time in the gap and the compact list are kept in this browser and set as classes on
// the document root, which styles.css reads.

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
const look = (wideFeed: boolean, systemFont: boolean, timeInGap: boolean, compactList = false) => ({
  wideFeed,
  systemFont,
  railNames: false,
  timeInGap,
  compactList,
});

it('keeps the full-width feed, the system font and the time in the gap on this device', async () => {
  const app = newApp();
  expect(app.look).toEqual(look(false, false, false));
  root = document.createElement('div');
  document.body.appendChild(root);
  await act(async () => render(<Settings app={app} />, root));
  const wide = root.querySelector<HTMLInputElement>('input[name=wide-feed]')!;
  const font = root.querySelector<HTMLInputElement>('input[name=system-font]')!;
  const time = root.querySelector<HTMLInputElement>('input[name=time-in-gap]')!;
  const list = root.querySelector<HTMLInputElement>('input[name=compact-list]')!;
  expect([wide.checked, font.checked, time.checked, list.checked]).toEqual([false, false, false, false]);

  await act(async () => wide.click());
  expect(app.look).toEqual(look(true, false, false));
  await act(async () => font.click());
  expect(app.look).toEqual(look(true, true, false));
  await act(async () => time.click());
  expect(app.look).toEqual(look(true, true, true));
  await act(async () => list.click());
  expect(app.look).toEqual(look(true, true, true, true));

  // A reload reads it back, and the document root carries it.
  const again = newApp();
  expect(again.look).toEqual(look(true, true, true, true));
  applyLook(document.documentElement, again.look);
  expect(document.documentElement.className.split(' ').sort()).toEqual(['feed-wide', 'font-system', 'list-compact', 'time-in-gap']);
  again.setLook({ wideFeed: false });
  applyLook(document.documentElement, again.look);
  expect(document.documentElement.classList.contains('feed-wide')).toBe(false);
  expect(newApp().look).toEqual(look(false, true, true, true));
});

it('shows what this device keeps; broken storage is the default look', async () => {
  localStorage.setItem('hqc.look', '{"wideFeed":true,"timeInGap":true}');
  root = document.createElement('div');
  document.body.appendChild(root);
  await act(async () => render(<Settings app={newApp()} />, root));
  expect(root.querySelector<HTMLInputElement>('input[name=wide-feed]')!.checked).toBe(true);
  expect(root.querySelector<HTMLInputElement>('input[name=system-font]')!.checked).toBe(false);
  expect(root.querySelector<HTMLInputElement>('input[name=time-in-gap]')!.checked).toBe(true);
  localStorage.setItem('hqc.look', '{not json');
  expect(newApp().look).toEqual(look(false, false, false));
});

it('the classes lift the feed width, swap the font and move the time into the gap', () => {
  const css = readFileSync(join(import.meta.dirname, '../src/styles.css'), 'utf8');
  expect(css).toMatch(/:root\.feed-wide \.feed \{\s*max-width: none;/);
  expect(css).toMatch(/:root\.font-system \{\s*--sans: -apple-system,/);
  expect(css).toMatch(
    /:root\.time-in-gap \.msg-agent,\s*:root\.time-in-gap \.msg-user:not\(\.msg-pending\) \{\s*position: relative;/,
  );
  expect(css).toMatch(
    /:root\.time-in-gap \.msg-agent > \.meta,\s*:root\.time-in-gap \.msg-user:not\(\.msg-pending\) > \.meta \{\s*position: absolute;\s*top: 100%;/,
  );
  expect(css).toMatch(/:root\.time-in-gap \.feed \{\s*padding-bottom: 24px;/);
  expect(css).toMatch(
    /:root\.time-in-gap \.msg-agent > \.meta \.btn-copy,\s*:root\.time-in-gap \.msg-user:not\(\.msg-pending\) > \.meta \.btn-copy \{\s*order: -1;/,
  );
  expect(css).toMatch(/:root\.time-in-gap \.chat-phone \.feed \{\s*gap: 24px;/);
  // The compact rules touch only the live list's rows in the desktop column: the archive and the
  // search hits keep their state and place lines.
  expect(css).toMatch(
    /:root\.list-compact \.sessions:not\(\.sessions-phone\) \.session-item \.session-sub,\s*:root\.list-compact \.sessions:not\(\.sessions-phone\) \.session-item \.session-where \{\s*display: none;/,
  );
  expect(css).not.toMatch(/:root\.list-compact \.session-(sub|where)\b/);
  expect(css).toMatch(/\.session-item \.dot-idle\.state-locked_vscode \{\s*background: var\(--vscode\);/);
  // The "⋯" menu stays above the rows below: no transform on the row's buttons.
  expect(css).not.toMatch(/:root\.list-compact [^{]*\.row-acts \{[^}]*transform/);
});

it('keeps the project names in the project bar on this device (SPEC 0.91)', async () => {
  root = document.createElement('div');
  document.body.appendChild(root);
  const app = newApp();
  await act(async () => render(<Settings app={app} />, root));
  const names = root.querySelector<HTMLInputElement>('input[name=rail-names]')!;
  expect(names.checked).toBe(false);
  await act(async () => names.click());
  expect(app.look).toEqual({ wideFeed: false, systemFont: false, railNames: true, timeInGap: false, compactList: false });
  // A reload reads it back; the root's class widens the bar's column.
  const again = newApp();
  expect(again.look.railNames).toBe(true);
  applyLook(document.documentElement, again.look);
  expect(document.documentElement.classList.contains('rail-names')).toBe(true);
  const css = readFileSync(join(import.meta.dirname, '../src/styles.css'), 'utf8');
  expect(css).toMatch(/:root\.rail-names \.layout \{\s*grid-template-columns: auto /);
});
