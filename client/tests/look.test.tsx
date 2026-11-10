// @vitest-environment happy-dom
// The look (SPEC §3 User): one switch, Compact or Spacious, kept on the user like the theme and
// the same on every device. Compact sets five classes on the document root, which styles.css
// reads; Spacious none. A copy on this device paints the first frame of a reload before
// `/api/me` answers; the hub's look wins when it does.

import { afterEach, expect, it } from 'vitest';
import { render } from 'preact';
import { act } from 'preact/test-utils';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { Api } from '../src/api';
import { AppState } from '../src/app';
import type { LookName, Me } from '../src/proto';
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

const ME: Me = { id: 'u', email: 'me@example.com', personal_project_id: 'p', theme: 'warm', look: 'spacious', project_sort: 'custom' };
const socket = { onState: () => {}, onRequests: () => {}, onSummary: () => {}, start: () => {} } as unknown as HubSocket;
/** The hub as `init` reaches it: `/api/me` answers `me`, no projects or sessions. */
const hubApi = (me: Me) =>
  ({
    me: async () => me,
    projects: async () => [],
    archivedProjects: async () => [],
    allSessions: async () => [],
  }) as unknown as Api;
const FIVE = ['feed-column', 'feed-wide', 'font-system', 'list-compact', 'rail-names'];
const classes = () => [...document.documentElement.classList].sort();

/** Waits for `fn`, up to a second: the hub's answer takes a few turns of the event loop. */
async function until(what: string, fn: () => boolean): Promise<void> {
  for (let i = 0; i < 100 && !fn(); i++) await act(() => new Promise((r) => setTimeout(r, 10)));
  expect(fn(), what).toBe(true);
}

it('Compact sets all five classes on the root, Spacious none of them', () => {
  const el = document.documentElement;
  el.classList.add('other');
  for (const name of ['compact', 'compact', 'spacious', 'spacious', 'compact'] as LookName[]) {
    applyLook(el, name, {});
    expect(classes()).toEqual(name === 'compact' ? [...FIVE, 'other'].sort() : ['other']);
  }
  // The column's width stays this device's, whatever the look.
  applyLook(el, 'spacious', { listWidth: 320 });
  expect(el.style.getPropertyValue('--list-w')).toBe('320px');
  applyLook(el, 'compact', {});
  expect(el.style.getPropertyValue('--list-w')).toBe('');
});

it("the hub's look wins over the copy on this device, which then follows it", async () => {
  // The last look applied here was Compact; the user picked Spacious on another device.
  applyLook(document.documentElement, 'compact', {});
  const app = new AppState(hubApi(ME), socket);
  // Before `/api/me` answers, the first paint is the copy's.
  expect(app.lookName()).toBe('compact');
  await app.init();
  expect(app.error).toBeNull();
  expect(app.lookName()).toBe('spacious');
  applyLook(document.documentElement, app.lookName(), app.device);
  expect(classes()).toEqual([]);
  // The next reload starts Spacious.
  expect(new AppState(hubApi(ME), socket).lookName()).toBe('spacious');
  // Broken storage is Spacious too; the old switches of this device are not read any more.
  localStorage.setItem('hqc.lookName', 'dense');
  localStorage.setItem('hqc.look', '{"wideFeed":true,"oneColumn":true,"railNames":true,"compactList":true,"listWidth":300}');
  const again = new AppState(hubApi(ME), socket);
  expect(again.lookName()).toBe('spacious');
  expect(again.device).toEqual({ listWidth: 300 });
});

it('Settings has one Look switch next to the theme; a pick goes to the hub as `PATCH /api/me {look}`', async () => {
  const sent: { method?: string; url: string; body: unknown }[] = [];
  let me = ME;
  const fetchHub: typeof fetch = async (input, init) => {
    const body = JSON.parse(String(init?.body ?? 'null'));
    sent.push({ method: init?.method, url: String(input), body });
    me = { ...me, ...body };
    return new Response(JSON.stringify(me), { status: 200, headers: { 'content-type': 'application/json' } });
  };
  // Settings lists the workers too; this test is about the look.
  const app = new AppState(Object.assign(new Api('', fetchHub), { workers: async () => [] }), socket);
  app.me = ME;
  root = document.createElement('div');
  document.body.appendChild(root);
  await act(async () => render(<Settings app={app} />, root));
  const heads = [...root.querySelectorAll('.page-body h2')].map((h) => h.textContent);
  expect(heads.slice(0, 2)).toEqual(['Theme', 'Look']);
  expect(heads).not.toContain('Feed');
  expect(root.querySelector('input[type=checkbox]')).toBeNull();
  const group = root.querySelector('[role=group][aria-label=Look]')!;
  const pick = (label: string) => [...group.querySelectorAll('button')].find((b) => b.textContent === label)!;
  expect([...group.querySelectorAll('button')].map((b) => [b.textContent, b.getAttribute('aria-pressed')])).toEqual([
    ['Compact', 'false'],
    ['Spacious', 'true'],
  ]);

  await act(async () => pick('Compact').click());
  await until('the hub answered', () => app.lookName() === 'compact');
  expect(sent).toEqual([{ method: 'PATCH', url: '/api/me', body: { look: 'compact' } }]);
  // App draws Settings again on the change.
  await act(async () => render(<Settings app={app} />, root));
  expect(pick('Compact').getAttribute('aria-pressed')).toBe('true');
  // The one already on is not sent again.
  await act(async () => pick('Compact').click());
  expect(sent).toHaveLength(1);
  await act(async () => pick('Spacious').click());
  await until('the hub answered', () => app.lookName() === 'spacious');
  expect(sent.map((r) => r.body)).toEqual([{ look: 'compact' }, { look: 'spacious' }]);
});

it("Compact's classes lift the feed width, swap the font, turn bubbles into plates with a gutter and name the projects", () => {
  const css = readFileSync(join(import.meta.dirname, '../src/styles.css'), 'utf8');
  expect(css).toMatch(/:root\.feed-wide \.feed,\s*:root\.feed-wide \.composer,\s*:root\.feed-wide \.lockbar,\s*:root\.feed-wide \.live-agents-list \{\s*max-width: none;/);
  expect(css).toMatch(/:root\.font-system \{\s*--sans: -apple-system,/);
  expect(css).not.toMatch(/time-in-gap/);
  // One column: the user's message is a plate across the feed, every other item has a gutter
  // with its mark; without it the marks are not shown and the user's messages stay bubbles.
  expect(css).toMatch(/:root\.feed-column \.msg-user \{\s*display: block;/);
  expect(css).toMatch(/:root\.feed-column \.msg-bubble \{\s*max-width: none;/);
  expect(css).toMatch(/\n\.msg-bubble \{\s*max-width: 80%;/);
  expect(css).toMatch(/:root\.feed-column \.feed-block \{\s*padding-left: 22px;/);
  expect(css).toMatch(/:root\.feed-column \.feed-block\.feed-full \{\s*padding-left: 0;/);
  expect(css).toMatch(/\n\.feed-mark \{\s*display: none;/);
  expect(css).toMatch(/:root\.feed-column \.feed-mark \{\s*position: absolute;\s*left: 0;/);
  // The compact rules touch only the live list's rows in the desktop column: the archive and the
  // search hits keep their state and place lines.
  expect(css).toMatch(
    /:root\.list-compact \.sessions:not\(\.sessions-phone\) \.session-item \.session-sub,\s*:root\.list-compact \.sessions:not\(\.sessions-phone\) \.session-item \.session-where \{\s*display: none;/,
  );
  expect(css).not.toMatch(/:root\.list-compact \.session-(sub|where)\b/);
  // A compact row is one 26 px line; open in VS Code is `</>`, not a blue dot.
  expect(css).toMatch(/:root\.list-compact \.sessions:not\(\.sessions-phone\) \.session-item > \.session-row \{[^}]*height: 26px;/);
  expect(css).not.toMatch(/state-locked_vscode/);
  expect(css).toMatch(/\.dot-vs \{[^}]*flex: 0 0 12px;/);
  // The context menu stands where it was opened: the plain `.menu` rule (absolute, right: 0)
  // comes later in the file, so the menu's own rule needs both classes to win.
  expect(css).toMatch(/\.menu\.ctx-menu \{\s*position: fixed;\s*right: auto;/);
  expect(css.indexOf('.menu.ctx-menu {')).toBeLessThan(css.indexOf('\n.menu {'));
  // The project bar: a row per project, the bar's column widened.
  expect(css).toMatch(/:root\.rail-names \.layout \{\s*grid-template-columns: auto /);
});

it("a session row's dot is as wide as `</>`, so the title stays put when the mark changes", () => {
  const css = readFileSync(join(import.meta.dirname, '../src/styles.css'), 'utf8');
  const px = (re: RegExp) => Number(css.match(re)![1]);
  const vs = px(/\n\.dot-vs \{\s*flex: 0 0 (\d+)px;/);
  // Desktop: the 8 px dot and its side margins.
  const dot = px(/\n\.dot \{\s*flex: 0 0 (\d+)px;/);
  const side = px(/\n\.session-item \.dot \{\s*margin-left: (\d+)px;\s*margin-right: \1px;/);
  expect(dot + 2 * side).toBe(vs);
  // Phone: the 10 px dot.
  const phoneDot = px(/\n\.sessions-phone \.dot \{\s*flex-basis: (\d+)px;/);
  const phoneSide = px(/\n\.sessions-phone \.session-item \.dot \{\s*margin-left: (\d+)px;\s*margin-right: \1px;/);
  expect(phoneDot + 2 * phoneSide).toBe(vs);
});

it("a bubble's text shares a line box with the floated time, so a short message is not broken at every letter", () => {
  const css = readFileSync(join(import.meta.dirname, '../src/styles.css'), 'utf8');
  expect(css).toMatch(/\n\.msg-time \{\s*float: right;/);
  expect(css).toMatch(/\n\.msg-bubble > \.msg-text \{\s*display: inline;\s*\}/);
  // The note too, with a line break after it; not on a plate, where it floats left of the time.
  expect(css).toMatch(/\n\.msg-note \{\s*display: inline;/);
  expect(css).toMatch(/\n\.msg-note::after \{\s*content: '\\A';\s*white-space: pre;/);
  expect(css).toMatch(/\n:root\.feed-column \.msg-note::after \{\s*content: none;/);
});
