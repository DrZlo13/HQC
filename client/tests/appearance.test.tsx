// T49 through the UI against the mock hub: the project settings page (Appearance, the
// workspaces moved in from the sidebar, Project), the Appearance dialog with its live preview,
// the app theme on the Settings page, and the theme on the document root following the user's
// theme and the current project.

import { afterAll, beforeAll, expect, it } from 'vitest';
import { render } from 'preact';
import { Window } from 'happy-dom';
// @ts-expect-error plain JS module without types
import { startMock } from '../mock/server.mjs';
import { Api } from '../src/api';
import { AppState } from '../src/app';
import { colorHex, themeVars } from '../src/theme';
import { App } from '../src/ui/App';
import { ProjectSettings } from '../src/ui/ProjectSettings';
import { HubSocket } from '../src/ws';

let mock: { url: string; close(): Promise<void> };
let win: Window;
let app: AppState;
let root: HTMLElement;
const saved: Record<string, PropertyDescriptor | undefined> = {};

function installGlobals(w: Window) {
  const g = globalThis as Record<string, unknown>;
  const map: Record<string, unknown> = {
    window: w,
    document: w.document,
    location: w.location,
    addEventListener: w.addEventListener.bind(w),
    removeEventListener: w.removeEventListener.bind(w),
    prompt: () => '',
    confirm: () => true,
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
const button = (text: string, scope = '') =>
  $$(`${scope} button`).find((b) => b.textContent?.trim() === text) as HTMLButtonElement | undefined;
const rootVar = (k: string) => document.documentElement.style.getPropertyValue(k);

function pickFile(input: HTMLInputElement, file: File) {
  Object.defineProperty(input, 'files', { value: [file], configurable: true });
  input.dispatchEvent(new win.Event('change', { bubbles: true }) as unknown as Event);
}

/** A PNG header: the hub checks the magic bytes and the size in pixels, the mock the first. */
function png(): File {
  const b = new Uint8Array([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0, 0, 0, 13, 0x49, 0x48, 0x44, 0x52, 0, 0, 1, 0, 0, 0, 1, 0]);
  return new File([b], 'logo.png', { type: 'image/png' });
}

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
  await new Promise((r) => setTimeout(r, 50));
  app.socket.stop();
  await mock.close();
  await win.happyDOM.close();
  restoreGlobals();
});

it('opens the project settings from the sidebar gear: appearance, workspaces, project', async () => {
  const personal = app.me!.personal_project_id;
  const gear = await until('gear', () => $<HTMLAnchorElement>('.side-head .btn-project-settings'));
  expect(gear.getAttribute('href')).toBe(`#/p/${personal}/settings`);
  // The workspaces left the sidebar.
  expect($$('.sessions a').some((a) => a.textContent === 'Project workspaces')).toBe(false);
  location.hash = gear.getAttribute('href')!;
  await until('settings page', () => $('.project-settings'));
  expect($('.project-settings .bar .title')!.textContent).toBe('Project settings');
  expect($('.project-settings .bar .state')!.textContent).toBe('Personal');
  expect($$('.project-settings h2').map((h) => h.textContent)).toEqual(['Appearance', 'Workspaces']);
  // The personal project gets an appearance, but no rename or delete (SPEC §3).
  expect($('.ps-look .ps-color')!.textContent).toBe('Slate');
  const row = await until('workspace row', () => $('.workspace-row'));
  expect(row.querySelector('.workspace-name')!.textContent).toBe('home-mac');
  // The worker's name once the workers are loaded.
  await until('worker name', () => row.querySelector('.workspace-path')!.textContent === 'home-mac:~/work/hqc');
  expect(row.querySelector('.tag')!.textContent).toBe('default');
  expect(row.querySelector('.mirror-toggle')!.textContent).toBe('VS Code sessions');
  expect(button('Remove', '.workspace-row')).toBeDefined();
  expect(button('Add a workspace')).toBeDefined();
  expect(button('Import from Claude Code')).toBeDefined();
  expect($('.btn-rename-project')).toBeNull();
  expect($('.btn-delete-project')).toBeNull();
});

it('previews in the dialog, changes nothing on Cancel and saves color and icon', async () => {
  const pid = app.me!.personal_project_id;
  expect(rootVar('--accent')).toBe(colorHex('slate'));
  expect(rootVar('--bg')).toBe('#1D1B18');
  button('Change icon and color')!.click();
  const dialog = await until('dialog', () => $('[role=dialog][aria-label="Project appearance"]'));
  expect(dialog.querySelector('.sub')!.textContent).toContain('Personal');
  // No icon yet: Letters, with the first letters.
  expect(dialog.querySelector('.seg.is-on')!.textContent).toBe('Letters');
  expect($<HTMLInputElement>('input[name=letters]')!.value).toBe('P');

  button('Emoji', '[role=dialog]')!.click();
  (await until('emoji grid', () => $<HTMLButtonElement>('button[aria-label="Use 🛰️"]')))!.click();
  $<HTMLButtonElement>('button.color-pick[aria-label="Plum"]')!.click();
  // The app theme is on the Settings page, not here.
  expect(button('Graphite', '[role=dialog]')).toBeUndefined();
  // Live: the dialog and its preview take the choice, in the app theme; the app does not
  // until Save.
  await until('preview', () => $('.preview-tile.is-current')?.textContent === '🛰️');
  const preview = themeVars(colorHex('plum'), 'warm');
  const box = $('[role=dialog]')!;
  expect(box.style.getPropertyValue('--accent')).toBe(preview['--accent']);
  expect(box.style.getPropertyValue('--rail')).toBe(preview['--rail']);
  expect(box.style.getPropertyValue('--mark-bg')).toBe(preview['--mark-bg']);
  expect(rootVar('--accent')).toBe(colorHex('slate'));
  expect($$('.preview-row .preview-sub').map((s) => s.textContent)).toEqual(['Running', 'Waiting for permission', 'Idle']);

  button('Cancel', '[role=dialog]')!.click();
  await until('closed', () => !$('[role=dialog]'));
  expect(app.projects.get(pid)!.color).toBe('slate');
  expect(app.theme()).toBe('warm');

  button('Change icon and color')!.click();
  await until('dialog again', () => $('[role=dialog]'));
  button('Emoji', '[role=dialog]')!.click();
  (await until('emoji grid', () => $<HTMLButtonElement>('button[aria-label="Use 🛰️"]')))!.click();
  $<HTMLButtonElement>('button.color-pick[aria-label="Plum"]')!.click();
  await until('picked', () => $('.color-pick.is-on')?.getAttribute('aria-label') === 'Plum');
  button('Save', '[role=dialog]')!.click();
  await until('saved', () => !$('[role=dialog]'));
  expect(app.projects.get(pid)).toMatchObject({ color: 'plum', icon: { kind: 'emoji', value: '🛰️' } });
  expect(app.me!.theme).toBe('warm');
  // On the hub, as another device sees it after a reload.
  expect((await app.api.projects()).find((p) => p.id === pid)).toMatchObject({ color: 'plum', icon: { value: '🛰️' } });
  // The root follows: the project's accent, the neutrals of the theme; the rail shows the emoji.
  await until('root vars', () => rootVar('--accent') === colorHex('plum'));
  expect(rootVar('--bg')).toBe('#1D1B18');
  expect(rootVar('--waiting')).toBe('#F2B35B');
  expect($('.rail-project.is-current')!.textContent).toBe('🛰️');
  expect($('.ps-look .ps-color')!.textContent).toBe('Plum');
});

it('saves letters in upper case and uploads an image; a wrong file never leaves the client', async () => {
  const pid = app.me!.personal_project_id;
  button('Change icon and color')!.click();
  await until('dialog', () => $('[role=dialog]'));
  expect($('[role=dialog] .seg.is-on')!.textContent).toBe('Emoji');
  button('Letters', '[role=dialog]')!.click();
  const input = await until('letters', () => $<HTMLInputElement>('input[name=letters]'));
  input.value = 'hq';
  input.dispatchEvent(new win.Event('input', { bubbles: true }) as unknown as Event);
  await until('preview letters', () => $('.preview-tile.is-current')?.textContent === 'HQ');
  button('Save', '[role=dialog]')!.click();
  await until('saved', () => !$('[role=dialog]'));
  expect(app.projects.get(pid)!.icon).toEqual({ kind: 'letters', value: 'HQ' });
  expect($('.rail-project.is-current')!.textContent).toBe('HQ');

  button('Change icon and color')!.click();
  await until('dialog', () => $('[role=dialog]'));
  button('Image', '[role=dialog]')!.click();
  const file = await until('file input', () => $<HTMLInputElement>('.drop-zone input[type=file]'));
  expect(file.getAttribute('accept')).toBe('image/png,image/jpeg,image/webp');
  // SVG: refused here, nothing uploaded; Save without an image says so.
  pickFile(file, new File(['<svg/>'], 'logo.svg', { type: 'image/svg+xml' }));
  await until('svg refused', () => $('.appearance-error')?.textContent === 'PNG, JPEG or WebP only');
  pickFile(file, new File([new Uint8Array(256 * 1024 + 1)], 'big.png', { type: 'image/png' }));
  await until('too large', () => $('.appearance-error')?.textContent === 'At most 256 KB');
  button('Save', '[role=dialog]')!.click();
  await until('no image', () => $('.appearance-error')?.textContent === 'Choose an image first');
  expect(app.projects.get(pid)!.icon?.kind).toBe('letters');

  pickFile(file, png());
  await until('picked', () => $('.drop-main')?.textContent === 'logo.png');
  button('Save', '[role=dialog]')!.click();
  await until('saved', () => !$('[role=dialog]'));
  const icon = app.projects.get(pid)!.icon;
  expect(icon?.kind).toBe('image');
  const url = icon?.kind === 'image' ? icon.url : '';
  expect(url).toMatch(new RegExp(`^/api/projects/${pid}/icon\\?v=`));
  expect($<HTMLImageElement>('.rail-project.is-current img')!.getAttribute('src')).toBe(url);
  const res = await fetch(mock.url + url);
  expect(res.headers.get('content-type')).toBe('image/png');
});

it('picks the app theme on the Settings page: a light tile applies at once and goes to the hub', async () => {
  location.hash = '#/settings';
  await until('theme tiles', () => $$('.theme-group .base-pick').length === 6);
  expect($$('.page-body h2').map((h) => h.textContent)).toEqual(['Theme', 'Notifications', 'Workers']);
  expect($$('.theme-group-label').map((l) => l.textContent)).toEqual(['Dark', 'Light']);
  expect($('.base-pick[aria-pressed=true]')!.textContent).toBe('Warm');
  button('Graphite', '[aria-label="Light themes"]')!.click();
  await until('picked', () => app.me?.theme === 'graphite_light');
  // On the hub (`PATCH /api/me`), as another device sees it after a reload.
  expect((await app.api.me()).theme).toBe('graphite_light');
  // No "Save": the root takes it at once, light neutrals and a light color scheme.
  await until('root vars', () => rootVar('--bg') === '#F6F7F9');
  expect(document.documentElement.style.getPropertyValue('color-scheme')).toBe('light');
  const on = $$('.base-pick[aria-pressed=true]');
  expect(on.map((b) => b.closest('.theme-group')!.getAttribute('aria-label') + ' ' + b.textContent)).toEqual(['Light themes Graphite']);
});

it('is a full-screen sheet on a phone, with the same sections', async () => {
  const pid = app.me!.personal_project_id;
  location.hash = '#/';
  render(<ProjectSettings app={app} projectId={pid} phone />, root);
  const change = await until('appearance card', () => $<HTMLButtonElement>('.is-phone button.ps-look'));
  expect(change.querySelector('.ps-change')!.textContent).toBe('Change');
  expect(change.querySelector('.ps-color')!.textContent).toBe('Plum');
  await until('workspace card', () => $('.is-phone .workspace-row'));
  change.click();
  const scrim = await until('sheet', () => $('.modal-scrim.modal-phone'));
  expect(scrim.querySelector('[role=dialog]')).not.toBeNull();
  expect($$('.modal-phone .ap-label').map((l) => l.textContent)).toEqual(['Icon', 'Project color']);
  button('Cancel', '[role=dialog]')!.click();
  await until('closed', () => !$('.modal-scrim'));
  render(<App app={app} />, root);
});
