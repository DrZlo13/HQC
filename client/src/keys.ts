// The keys of the client's actions (SPEC §6): each one a combination the person picks in
// Settings, kept on this device. Without a pick an action has its default, which is ⌘ on a
// Mac and Ctrl or Alt elsewhere; a person can also leave an action without a key.

/** A Mac (or an iPad with a keyboard): ⌘ is the shortcuts' key there, Ctrl elsewhere. */
export function onMac(): boolean {
  return /Mac|iPhone|iPad/.test(navigator.platform || navigator.userAgent);
}

export type KeyAction =
  | 'quote'
  | 'search'
  | 'projectSearch'
  | 'sessions'
  | 'newSession'
  | 'archiveSession'
  | 'previousSession'
  | 'nextSession'
  | 'previousProject'
  | 'nextProject'
  | 'back'
  | 'forward';

/** The actions, in the order Settings lists them. */
export const KEY_ACTIONS: readonly { id: KeyAction; label: string }[] = [
  { id: 'quote', label: 'Quote the selected text' },
  { id: 'search', label: 'Search the selected text' },
  { id: 'projectSearch', label: 'Search projects' },
  { id: 'sessions', label: 'Show the sessions (a narrow window)' },
  { id: 'newSession', label: 'New session' },
  { id: 'archiveSession', label: 'Archive the open session' },
  { id: 'previousSession', label: 'Previous session' },
  { id: 'nextSession', label: 'Next session' },
  { id: 'previousProject', label: 'Previous project' },
  { id: 'nextProject', label: 'Next project' },
  { id: 'back', label: 'Back' },
  { id: 'forward', label: 'Forward' },
];

/** A key and the modifiers held with it: `code` is `KeyboardEvent.code`, so the key is the same
 *  in any layout. */
export interface Key {
  code: string;
  meta: boolean;
  ctrl: boolean;
  alt: boolean;
  shift: boolean;
}

const key = (code: string, mods: Partial<Omit<Key, 'code'>>): Key => ({
  code,
  meta: false,
  ctrl: false,
  alt: false,
  shift: false,
  ...mods,
});

/** The key an action has until one is picked. */
export function defaultKey(action: KeyAction): Key {
  if (onMac()) {
    switch (action) {
      case 'quote': return key('Quote', { meta: true });
      case 'search': return key('KeyE', { meta: true });
      case 'projectSearch': return key('KeyK', { meta: true });
      case 'sessions': return key('Backslash', { meta: true });
      case 'newSession': return key('KeyN', { meta: true });
      case 'archiveSession': return key('KeyA', { ctrl: true, meta: true });
      case 'previousSession': return key('ArrowUp', { alt: true, meta: true });
      case 'nextSession': return key('ArrowDown', { alt: true, meta: true });
      case 'previousProject': return key('ArrowUp', { ctrl: true, meta: true });
      case 'nextProject': return key('ArrowDown', { ctrl: true, meta: true });
      case 'back': return key('BracketLeft', { meta: true });
      case 'forward': return key('BracketRight', { meta: true });
    }
  }
  switch (action) {
    case 'quote': return key('Quote', { ctrl: true });
    case 'search': return key('KeyE', { ctrl: true });
    case 'projectSearch': return key('KeyK', { ctrl: true });
    case 'sessions': return key('Backslash', { ctrl: true });
    case 'newSession': return key('KeyN', { alt: true });
    case 'archiveSession': return key('KeyA', { ctrl: true, alt: true });
    case 'previousSession': return key('ArrowUp', { alt: true });
    case 'nextSession': return key('ArrowDown', { alt: true });
    case 'previousProject': return key('ArrowUp', { alt: true, shift: true });
    case 'nextProject': return key('ArrowDown', { alt: true, shift: true });
    case 'back': return key('ArrowLeft', { alt: true });
    case 'forward': return key('ArrowRight', { alt: true });
  }
}

const STORE = 'hqc.keys';

/** What a person picked: an action's key, or null where they left it without one. An action
 *  that is not here has its default. */
export type PickedKeys = Partial<Record<KeyAction, Key | null>>;

function wellFormed(v: unknown): v is Key {
  if (typeof v !== 'object' || v === null) return false;
  const k = v as Record<string, unknown>;
  return typeof k.code === 'string' && k.code !== '' && ['meta', 'ctrl', 'alt', 'shift'].every((m) => typeof k[m] === 'boolean');
}

/** The picks kept on this device; storage that fails or holds something else means none, and
 *  an entry that is not a key (or null) of a known action is left out. */
export function storedKeys(): PickedKeys {
  const picked: PickedKeys = {};
  try {
    const raw: unknown = JSON.parse(localStorage.getItem(STORE) ?? 'null');
    if (typeof raw !== 'object' || raw === null) return picked;
    for (const { id } of KEY_ACTIONS) {
      const v = (raw as Record<string, unknown>)[id];
      if (v === null) picked[id] = null;
      else if (wellFormed(v)) picked[id] = { code: v.code, meta: v.meta, ctrl: v.ctrl, alt: v.alt, shift: v.shift };
    }
  } catch {
    // None.
  }
  return picked;
}

export function keepKeys(picked: PickedKeys): void {
  try {
    localStorage.setItem(STORE, JSON.stringify(picked));
  } catch {
    // Kept until a reload.
  }
}

/** Whether the event is `k` exactly: its key and all four modifiers. A cleared action (null)
 *  matches nothing. */
export function keyMatches(k: Key | null, e: KeyboardEvent): boolean {
  return !!k && e.code === k.code && e.metaKey === k.meta && e.ctrlKey === k.ctrl && e.altKey === k.alt && e.shiftKey === k.shift;
}

export function sameKey(a: Key | null, b: Key | null): boolean {
  return a === null || b === null ? a === b : a.code === b.code && a.meta === b.meta && a.ctrl === b.ctrl && a.alt === b.alt && a.shift === b.shift;
}

const MODIFIER_CODES = new Set(['ShiftLeft', 'ShiftRight', 'ControlLeft', 'ControlRight', 'AltLeft', 'AltRight', 'MetaLeft', 'MetaRight']);

/** The key a press records: not a modifier alone, and not without ⌘ / Ctrl / Alt (a bare key
 *  would take the typing); null otherwise. */
export function keyFromEvent(e: KeyboardEvent): Key | null {
  if (MODIFIER_CODES.has(e.code) || !(e.metaKey || e.ctrlKey || e.altKey)) return null;
  return { code: e.code, meta: e.metaKey, ctrl: e.ctrlKey, alt: e.altKey, shift: e.shiftKey };
}

const NAMES: Record<string, string> = {
  ArrowUp: '↑',
  ArrowDown: '↓',
  ArrowLeft: '←',
  ArrowRight: '→',
  Space: 'Space',
  Quote: "'",
  Backslash: '\\',
  BracketLeft: '[',
  BracketRight: ']',
  Comma: ',',
  Period: '.',
  Slash: '/',
  Semicolon: ';',
  Minus: '-',
  Equal: '=',
  Backquote: '`',
};

/** How a key reads: ⌃⌥⇧⌘ and the name on a Mac, `Ctrl+Alt+Shift+Win+name` elsewhere. */
export function keyLabel(k: Key): string {
  const name = NAMES[k.code] ?? /^(?:Key|Digit)(.)$/.exec(k.code)?.[1] ?? k.code;
  if (onMac()) return `${k.ctrl ? '⌃' : ''}${k.alt ? '⌥' : ''}${k.shift ? '⇧' : ''}${k.meta ? '⌘' : ''}${name}`;
  return [k.ctrl && 'Ctrl', k.alt && 'Alt', k.shift && 'Shift', k.meta && 'Win', name].filter(Boolean).join('+');
}

/** A tooltip's text with the action's key after it, none when the action has none. */
export function withKey(text: string, k: Key | null): string {
  return k ? `${text} (${keyLabel(k)})` : text;
}
