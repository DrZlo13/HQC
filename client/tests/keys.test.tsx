// @vitest-environment happy-dom
// The keys of the client's actions (SPEC §6): `keys.ts`, the picks AppState keeps on the device,
// and the "Keys" section of Settings that records them.

import { afterEach, describe, expect, it, vi } from 'vitest';
import { render } from 'preact';
import { act } from 'preact/test-utils';
import type { Api } from '../src/api';
import { AppState } from '../src/app';
import {
  KEY_ACTIONS,
  defaultKey,
  keyFromEvent,
  keyLabel,
  keyMatches,
  storedKeys,
  withKey,
  type Key,
  type KeyAction,
} from '../src/keys';
import type { HubSocket } from '../src/ws';
import { useSubscription } from '../src/ui/hooks';
import { Settings } from '../src/ui/Settings';

const MAC = 'MacIntel';
const LINUX = 'Linux x86_64';
const platform = vi.spyOn(navigator, 'platform', 'get');
const key = (code: string, mods: Partial<Omit<Key, 'code'>> = {}): Key => ({ code, meta: false, ctrl: false, alt: false, shift: false, ...mods });
const down = (code: string, mods: Partial<Record<'metaKey' | 'ctrlKey' | 'altKey' | 'shiftKey', boolean>> = {}) =>
  new KeyboardEvent('keydown', { code, bubbles: true, cancelable: true, ...mods });

afterEach(() => {
  platform.mockReset();
  localStorage.clear();
});

describe('keys.ts', () => {
  it('each action has a default, ⌘ on a Mac and Ctrl or Alt elsewhere, resolved when asked', () => {
    const labels = () => KEY_ACTIONS.map((a) => keyLabel(defaultKey(a.id)));
    platform.mockReturnValue(MAC);
    expect(labels()).toEqual(["⌘'", '⌘E', '⌘K', '⌘\\', '⌘N', '⌃⌘A', '⌥⌘↑', '⌥⌘↓', '⌃⌘↑', '⌃⌘↓', '⌘[', '⌘]']);
    platform.mockReturnValue(LINUX);
    expect(labels()).toEqual(["Ctrl+'", 'Ctrl+E', 'Ctrl+K', 'Ctrl+\\', 'Alt+N', 'Ctrl+Alt+A', 'Alt+↑', 'Alt+↓', 'Alt+Shift+↑', 'Alt+Shift+↓', 'Alt+←', 'Alt+→']);
    expect(KEY_ACTIONS.map((a) => a.id)).toEqual([
      'quote', 'search', 'projectSearch', 'sessions', 'newSession', 'archiveSession',
      'previousSession', 'nextSession', 'previousProject', 'nextProject', 'back', 'forward',
    ]);
  });

  it('a key reads ⌃⌥⇧⌘ and its name on a Mac, Ctrl+Alt+Shift+Win+name elsewhere; the names', () => {
    const all = key('KeyQ', { ctrl: true, alt: true, shift: true, meta: true });
    platform.mockReturnValue(MAC);
    expect(keyLabel(all)).toBe('⌃⌥⇧⌘Q');
    platform.mockReturnValue(LINUX);
    expect(keyLabel(all)).toBe('Ctrl+Alt+Shift+Win+Q');
    const names = ['Digit7', 'Space', 'Comma', 'Period', 'Slash', 'Semicolon', 'Minus', 'Equal', 'Backquote', 'ArrowLeft', 'ArrowRight', 'F5', 'Enter'];
    expect(names.map((code) => keyLabel(key(code, { alt: true })))).toEqual([
      'Alt+7', 'Alt+Space', 'Alt+,', 'Alt+.', 'Alt+/', 'Alt+;', 'Alt+-', 'Alt+=', 'Alt+`', 'Alt+←', 'Alt+→', 'Alt+F5', 'Alt+Enter',
    ]);
    expect(withKey('Quote', key('Quote', { ctrl: true }))).toBe("Quote (Ctrl+')");
    expect(withKey('Quote', null)).toBe('Quote');
  });

  it('matches the key and all four modifiers exactly; a cleared action matches nothing', () => {
    const k = key('KeyK', { meta: true });
    expect(keyMatches(k, down('KeyK', { metaKey: true }))).toBe(true);
    expect(keyMatches(k, down('KeyK', { metaKey: true, shiftKey: true }))).toBe(false);
    expect(keyMatches(k, down('KeyK', { metaKey: true, ctrlKey: true }))).toBe(false);
    expect(keyMatches(k, down('KeyK', { metaKey: true, altKey: true }))).toBe(false);
    expect(keyMatches(k, down('KeyK', { ctrlKey: true }))).toBe(false);
    expect(keyMatches(k, down('KeyJ', { metaKey: true }))).toBe(false);
    expect(keyMatches(null, down('KeyK', { metaKey: true }))).toBe(false);
  });

  it('records a key held with ⌘, Ctrl or Alt: not a modifier alone, not a bare key', () => {
    expect(keyFromEvent(down('KeyK', { metaKey: true, shiftKey: true }))).toEqual(key('KeyK', { meta: true, shift: true }));
    expect(keyFromEvent(down('ArrowUp', { altKey: true }))).toEqual(key('ArrowUp', { alt: true }));
    expect(keyFromEvent(down('KeyK'))).toBeNull();
    expect(keyFromEvent(down('KeyK', { shiftKey: true }))).toBeNull();
    for (const code of ['ShiftLeft', 'ControlRight', 'AltLeft', 'MetaRight']) {
      expect(keyFromEvent(down(code, { metaKey: true, ctrlKey: true, altKey: true, shiftKey: true }))).toBeNull();
    }
  });

  it('reads back what was stored, a cleared action too, and ignores what is not a key of an action', () => {
    expect(storedKeys()).toEqual({});
    localStorage.setItem(
      'hqc.keys',
      JSON.stringify({
        quote: key('KeyQ', { meta: true }),
        search: null,
        back: { code: 'KeyB', meta: true },
        forward: { code: '', meta: false, ctrl: false, alt: false, shift: false },
        nextSession: 'ArrowDown',
        nextProject: { code: 'KeyP', meta: 1, ctrl: false, alt: false, shift: false },
        newSession: { ...key('KeyM', { alt: true }), extra: 1 },
        nothing: key('KeyZ', { alt: true }),
      }),
    );
    expect(storedKeys()).toEqual({ quote: key('KeyQ', { meta: true }), search: null, newSession: key('KeyM', { alt: true }) });
    for (const bad of ['{', 'null', '5', '"x"']) {
      localStorage.setItem('hqc.keys', bad);
      expect(storedKeys()).toEqual({});
    }
  });
});

const socket = { onState: () => () => {} } as unknown as HubSocket;
function newApp(): AppState {
  const api = { workers: async () => [], pushKey: async () => null } as unknown as Api;
  const app = new AppState(api, socket);
  app.me = { id: 'u', email: 'who@example.com', personal_project_id: 'p0', theme: 'warm', look: 'spacious', project_sort: 'custom', new_session_project_id: null, quick_messages: [] };
  return app;
}

describe('the picks of an AppState', () => {
  it('keyFor is the default until a pick; setKey stores it, null clears, undefined gives the default back; a new AppState reads them', () => {
    platform.mockReturnValue(MAC);
    const app = newApp();
    const changed = vi.fn();
    app.onChange(changed);
    expect(app.keyFor('back')).toEqual(key('BracketLeft', { meta: true }));
    app.setKey('back', key('KeyB', { alt: true }));
    app.setKey('forward', null);
    expect(changed).toHaveBeenCalledTimes(2);
    expect(app.keyFor('back')).toEqual(key('KeyB', { alt: true }));
    expect(app.keyFor('forward')).toBeNull();
    const again = newApp();
    expect(again.keyFor('back')).toEqual(key('KeyB', { alt: true }));
    expect(again.keyFor('forward')).toBeNull();
    expect(again.keyFor('quote')).toEqual(defaultKey('quote'));
    app.setKey('back', undefined);
    app.setKey('forward', undefined);
    expect(newApp().keyFor('back')).toEqual(key('BracketLeft', { meta: true }));
    expect(newApp().keyFor('forward')).toEqual(key('BracketRight', { meta: true }));
    expect(JSON.parse(localStorage.getItem('hqc.keys')!)).toEqual({});
  });

  it('a key another action has leaves that one without, the default of the first too', () => {
    platform.mockReturnValue(MAC);
    const app = newApp();
    app.setKey('back', defaultKey('forward'));
    expect(app.keyFor('forward')).toBeNull();
    expect(app.keyFor('back')).toEqual(key('BracketRight', { meta: true }));
    // `forward` back to its default takes it from `back`.
    app.setKey('forward', undefined);
    expect(app.keyFor('forward')).toEqual(key('BracketRight', { meta: true }));
    expect(app.keyFor('back')).toBeNull();
    expect(newApp().keyFor('back')).toBeNull();
    // A cleared action takes nobody's key.
    app.setKey('quote', null);
    expect(app.keyFor('search')).toEqual(defaultKey('search'));
  });
});

describe('Settings, "Keys"', () => {
  let root: HTMLElement;
  afterEach(() => {
    render(null, root);
    root.remove();
  });
  function Live({ app }: { app: AppState }) {
    useSubscription((fn) => app.onChange(fn), [app]);
    return <Settings app={app} />;
  }
  async function mount(app: AppState) {
    root = document.createElement('div');
    document.body.appendChild(root);
    await act(async () => render(<Live app={app} />, root));
  }
  const row = (action: KeyAction) => root.querySelectorAll<HTMLElement>('.settings-keys .key-row')[KEY_ACTIONS.findIndex((a) => a.id === action)];
  const pick = (action: KeyAction) => row(action).querySelector<HTMLButtonElement>('.btn-key')!;
  const reset = (action: KeyAction) => row(action).querySelector<HTMLButtonElement>('.btn-key-default');
  const shown = (action: KeyAction) => pick(action).textContent;
  /** A key pressed on the window, as the browser delivers it; whether the page kept it. */
  const press = async (code: string, mods: Partial<Record<'metaKey' | 'ctrlKey' | 'altKey' | 'shiftKey', boolean>> = {}) => {
    const e = down(code, mods);
    await act(async () => void document.body.dispatchEvent(e));
    return e;
  };
  // A click, without the focus: WebKit does not focus a button on a click.
  const record = async (action: KeyAction) => {
    await act(async () => pick(action).click());
  };
  const pressDown = async (el: Element) => {
    await act(async () => void el.dispatchEvent(new Event('pointerdown', { bubbles: true })));
  };

  it('lists an action per row with its key, the text of the section; no "Default" while the key is the default', async () => {
    platform.mockReturnValue(MAC);
    await mount(newApp());
    expect(KEY_ACTIONS.map((a) => shown(a.id))).toEqual(["⌘'", '⌘E', '⌘K', '⌘\\', '⌘N', '⌃⌘A', '⌥⌘↑', '⌥⌘↓', '⌃⌘↑', '⌃⌘↓', '⌘[', '⌘]']);
    expect(root.querySelectorAll('.btn-key-default')).toHaveLength(0);
    const texts = [...root.querySelectorAll('.muted')].map((p) => p.textContent!.replace(/\s+/g, ' ').trim());
    expect(texts).toContain(
      'Click a key and press the new one with ⌘, ⌃ or ⌥ (Ctrl or Alt off a Mac); Esc cancels, Delete leaves the action without a key. A key given to another action leaves that one without. On this device.',
    );
    expect(texts).toContain(
      'Where "Search in …" of the menu on a right click, and its key, search the text selected in the feed, in a new tab. On this device.',
    );
  });

  it('records the key pressed, after a bare key and a modifier alone, nothing else gets the keys meanwhile; the key is kept and the Default button comes', async () => {
    platform.mockReturnValue(MAC);
    const app = newApp();
    const acted = vi.fn();
    addEventListener('keydown', acted);
    await mount(app);
    await record('back');
    expect(shown('back')).toBe('Press keys…');
    // A bare key, a modifier alone: it keeps waiting. Nothing else gets the keys meanwhile.
    const bare = await press('KeyX');
    const alone = await press('MetaLeft', { metaKey: true });
    expect([bare, alone].map((e) => e.defaultPrevented)).toEqual([true, true]);
    expect(acted).not.toHaveBeenCalled();
    expect(shown('back')).toBe('Press keys…');
    const picked = await press('KeyB', { altKey: true, shiftKey: true });
    expect(picked.defaultPrevented).toBe(true);
    expect(acted).not.toHaveBeenCalled();
    expect(shown('back')).toBe('⌥⇧B');
    expect(app.keyFor('back')).toEqual(key('KeyB', { alt: true, shift: true }));
    expect(reset('back')).not.toBeNull();
    expect(reset('forward')).toBeNull();
    // Recording is over: the key is the page's again.
    expect((await press('KeyK', { metaKey: true })).defaultPrevented).toBe(false);
    expect(acted).toHaveBeenCalledTimes(1);
    removeEventListener('keydown', acted);
    expect(newApp().keyFor('back')).toEqual(key('KeyB', { alt: true, shift: true }));
  });

  it('Esc cancels, Delete or Backspace leaves the action without a key ("None"), blur cancels; Default gives the key back', async () => {
    platform.mockReturnValue(MAC);
    const app = newApp();
    await mount(app);
    await record('quote');
    await press('Escape');
    expect(shown('quote')).toBe("⌘'");
    expect(app.keyFor('quote')).toEqual(defaultKey('quote'));
    // Esc with a modifier is a key like another.
    await record('quote');
    await press('Escape', { altKey: true });
    expect(shown('quote')).toBe('⌥Escape');
    await record('search');
    await press('Delete');
    expect(shown('search')).toBe('None');
    expect(app.keyFor('search')).toBeNull();
    expect(newApp().keyFor('search')).toBeNull();
    await record('projectSearch');
    await press('Backspace');
    expect(shown('projectSearch')).toBe('None');
    // Blur where the button had the focus: nothing was recorded, the next key is the page's.
    pick('sessions').focus();
    await record('sessions');
    await act(async () => pick('sessions').blur());
    expect(shown('sessions')).toBe('⌘\\');
    expect((await press('KeyQ', { metaKey: true })).defaultPrevented).toBe(false);
    // Default: the quote key is back, and so is the search key.
    await act(async () => reset('quote')!.click());
    expect(shown('quote')).toBe("⌘'");
    expect(reset('quote')).toBeNull();
    await act(async () => reset('search')!.click());
    expect(shown('search')).toBe('⌘E');
    expect(app.keyFor('search')).toEqual(defaultKey('search'));
    expect(JSON.parse(localStorage.getItem('hqc.keys')!)).toEqual({ projectSearch: null });
  });

  it('a press anywhere but on the button cancels, with no focus on it (WebKit); one row records at a time', async () => {
    platform.mockReturnValue(MAC);
    const app = newApp();
    await mount(app);
    const field = document.createElement('input');
    root.appendChild(field);
    await record('back');
    // A press on the button itself keeps it waiting.
    await pressDown(pick('back'));
    expect(shown('back')).toBe('Press keys…');
    // A press in another field: the recording is over, the field gets its keys.
    await pressDown(field);
    expect(shown('back')).toBe('⌘[');
    expect((await press('KeyA')).defaultPrevented).toBe(false);
    // Another row's button takes the recording; the first one no longer waits.
    await record('back');
    await pressDown(pick('forward'));
    await record('forward');
    expect([shown('back'), shown('forward')]).toEqual(['⌘[', 'Press keys…']);
    await press('KeyF', { metaKey: true });
    expect(app.keyFor('forward')).toEqual(key('KeyF', { meta: true }));
    expect(app.keyFor('back')).toEqual(defaultKey('back'));
    expect(shown('forward')).toBe('⌘F');
  });

  it('a key given to another action leaves that one with "None" and a Default button', async () => {
    platform.mockReturnValue(MAC);
    const app = newApp();
    await mount(app);
    await record('back');
    await press('BracketRight', { metaKey: true });
    expect(shown('back')).toBe('⌘]');
    expect(shown('forward')).toBe('None');
    expect(reset('forward')).not.toBeNull();
    expect(app.keyFor('forward')).toBeNull();
    const again = newApp();
    expect([again.keyFor('back'), again.keyFor('forward')]).toEqual([key('BracketRight', { meta: true }), null]);
  });
});
