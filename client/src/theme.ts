// The theme (T49, docs/design/redesign-2026-11-01/theme.js): a base palette of neutrals for
// the whole app plus the current project's color, as CSS variables on the document root,
// recomputed when either changes. Status colors (waiting, online, danger, between sessions)
// are the same in every project; light bases get darker ones.

import { THEME_NAMES, type ProjectColor, type Theme } from './proto';

export interface Base {
  /** A light base: darker status colors, the project color darkened as text. */
  light: boolean;
  bg: string;
  side: string;
  rail: string;
  surface: string;
  border: string;
  text: string;
  text2: string;
  muted: string;
  code: string;
  /** The tone of overlays and shadows (`--shade`): black on dark bases, a mid tone on light. */
  shade: string;
}

/** Three dark bases, Warm the default, and their light counterparts. */
export const BASES: Record<Theme, Base> = {
  warm: { light: false, bg: '#1D1B18', side: '#191715', rail: '#100E0C', surface: '#282521', border: '#3A352F', text: '#F2EEE8', text2: '#D8D1C7', muted: '#ADA599', code: '#141210', shade: '#000000' },
  graphite: { light: false, bg: '#1A1C1F', side: '#16181B', rail: '#0E0F11', surface: '#25282C', border: '#353A40', text: '#EDEFF2', text2: '#CDD2D8', muted: '#A3A9B1', code: '#121315', shade: '#000000' },
  contrast: { light: false, bg: '#121110', side: '#0F0E0D', rail: '#070706', surface: '#211F1C', border: '#4A443C', text: '#FFFFFF', text2: '#E9E4DC', muted: '#C6BEB2', code: '#0A0908', shade: '#000000' },
  warm_light: { light: true, bg: '#FAF7F2', side: '#F2EDE5', rail: '#E6DFD3', surface: '#FFFFFF', border: '#DDD5C8', text: '#1F1B16', text2: '#3E372F', muted: '#6B6257', code: '#F0EBE3', shade: '#8A8070' },
  graphite_light: { light: true, bg: '#F6F7F9', side: '#EDEFF2', rail: '#E0E3E8', surface: '#FFFFFF', border: '#D5D9DF', text: '#16191D', text2: '#353B43', muted: '#5E6670', code: '#EDEFF2', shade: '#7F8690' },
  contrast_light: { light: true, bg: '#FFFFFF', side: '#F4F2EF', rail: '#E4DFD8', surface: '#FFFFFF', border: '#8A8276', text: '#000000', text2: '#1C1915', muted: '#463F37', code: '#F1EEE9', shade: '#6B645A' },
};

/** The bases as the Settings page offers them: a row of dark and a row of light tiles. */
export const THEMES: { id: Theme; label: string; group: 'dark' | 'light' }[] = [
  { id: 'warm', label: 'Warm', group: 'dark' },
  { id: 'graphite', label: 'Graphite', group: 'dark' },
  { id: 'contrast', label: 'High contrast', group: 'dark' },
  { id: 'warm_light', label: 'Warm', group: 'light' },
  { id: 'graphite_light', label: 'Graphite', group: 'light' },
  { id: 'contrast_light', label: 'High contrast', group: 'light' },
];

/** Project colors, in the order Appearance offers them. No yellows: amber means waiting. */
export const PROJECT_COLORS: { id: ProjectColor; label: string; hex: string }[] = [
  { id: 'teal', label: 'Teal', hex: '#3FB8A8' },
  { id: 'plum', label: 'Plum', hex: '#B57BD6' },
  { id: 'blue', label: 'Blue', hex: '#6E9BFF' },
  { id: 'rose', label: 'Rose', hex: '#E77A9A' },
  { id: 'moss', label: 'Moss', hex: '#8DBA5B' },
  { id: 'coral', label: 'Coral', hex: '#F08A5D' },
  { id: 'cyan', label: 'Cyan', hex: '#4FC3E8' },
  { id: 'slate', label: 'Slate', hex: '#8A93A6' },
];

export function colorHex(c: ProjectColor): string {
  return (PROJECT_COLORS.find((x) => x.id === c) ?? PROJECT_COLORS[0]).hex;
}

export function colorLabel(c: ProjectColor): string {
  return (PROJECT_COLORS.find((x) => x.id === c) ?? PROJECT_COLORS[0]).label;
}

/** Fixed status and semantic colors, the same in every project, on the dark bases. */
export const STATUS = {
  waiting: '#F2B35B', // dot, "Waiting for…" text, rail badge
  waitingInk: '#1A1206', // text on amber
  online: '#5CC98A', // worker dot, success checks
  allowedText: '#6FD49A',
  danger: '#F59486', // danger text
  dangerFill: '#C8483A', // destructive confirm button
  relay: '#A99BFF', // messages between sessions (bg 8%, border 32%)
  relayText: '#C4BAFF',
  permissionBg: '#1F1A12',
  permissionBorder: '#5A4520',
};

/** The same colors on the light bases: darker, to read on a light background. */
export const STATUS_LIGHT: typeof STATUS = {
  waiting: '#9C5B00',
  waitingInk: '#FFFFFF',
  online: '#1A7F45',
  allowedText: '#1B7A43',
  danger: '#B83A2C',
  dangerFill: '#C8483A',
  relay: '#6E5BD8',
  relayText: '#5642B8',
  permissionBg: '#FFF5E3',
  permissionBorder: '#E3C48A',
};

const rgb = (h: string) => [1, 3, 5].map((i) => parseInt(h.slice(i, i + 2), 16));

/** `t` of color `a` over color `b`. */
export function mix(a: string, b: string, t: number): string {
  const A = rgb(a);
  const B = rgb(b);
  return '#' + A.map((v, i) => Math.round(v * t + B[i] * (1 - t)).toString(16).padStart(2, '0')).join('');
}

/** `#RRGGBB` at `alpha`, for tiles of other projects (~24 % of their color). */
export function alpha(hex: string, a: number): string {
  return `rgba(${rgb(hex).join(',')},${a})`;
}

/** The CSS variables of a base theme and a project color (`#RRGGBB`). */
export function themeVars(projectColor: string, base: Theme = 'warm'): Record<string, string> {
  const B = BASES[base] ?? BASES.warm;
  const c = projectColor;
  return {
    '--bg': B.bg,
    '--side': mix(c, B.side, 0.12), // sidebar, tinted
    '--side-plain': B.side, // archive mode sidebar, no tint
    '--rail': mix(c, B.rail, 0.18), // project bar
    '--row-on': mix(c, B.side, 0.3), // selected session row
    '--row-hover': mix(c, B.side, 0.18),
    '--surface': B.surface,
    '--border': B.border,
    '--text': B.text,
    '--text2': B.text2,
    '--muted': B.muted,
    '--code': B.code,
    '--accent': c, // primary buttons, running dot, links
    '--accent-text': B.light ? mix(c, '#000000', 0.55) : c, // the project color as text
    '--accent-ink': '#15120F', // text on accent
    '--accent-soft': mix(c, B.bg, 0.2), // project tile, soft chips
    '--accent-line': mix(c, B.bg, 0.5), // hover borders
    '--bubble': mix(c, B.surface, 0.2), // user message bubble
    '--mark-bg': mix(c, B.rail, 0.42), // current project tile in the rail, with an accent pill
    '--shade': B.shade, // overlays and shadows
  };
}

/** The status colors of `base` as variables (`--waiting`, `--online`, ...), set with the rest. */
export function statusVars(base: Theme = 'warm'): Record<string, string> {
  const set = (BASES[base] ?? BASES.warm).light ? STATUS_LIGHT : STATUS;
  const out: Record<string, string> = {};
  for (const [k, v] of Object.entries(set)) out[`--${k.replace(/[A-Z]/g, (m) => '-' + m.toLowerCase())}`] = v;
  return out;
}

/** How text reads on this device ("Settings" → "Feed"): the feed across the whole chat column
 *  instead of at most 780 px, the platform's own font instead of Inter, a message's time in
 *  the gap below it instead of a line of its own; and ("Settings" → "Project bar") a row with
 *  each project's full name instead of a tile. */
export interface Look {
  wideFeed: boolean;
  systemFont: boolean;
  railNames: boolean;
  timeInGap: boolean;
}

/** Sets the look as classes on `el` (the document root), read by styles.css. */
export function applyLook(el: HTMLElement, look: Look): void {
  el.classList.toggle('feed-wide', look.wideFeed);
  el.classList.toggle('font-system', look.systemFont);
  el.classList.toggle('rail-names', look.railNames);
  el.classList.toggle('time-in-gap', look.timeInGap);
}

const THEME_KEY = 'hqc.theme';

/** The base last applied on this device: a convenience, so a light theme does not start dark
 *  before `/api/me` answers; storage that fails or is empty means Warm. */
export function storedTheme(): Theme {
  try {
    const v = localStorage.getItem(THEME_KEY);
    return (THEME_NAMES as readonly string[]).includes(v ?? '') ? (v as Theme) : 'warm';
  } catch {
    return 'warm';
  }
}

/** Sets the variables on `el` (the document root), its color scheme (scrollbars, native
 *  controls), the browser's bar color and the iPhone status bar, and remembers the base on this
 *  device. */
export function applyTheme(el: HTMLElement, color: ProjectColor, base: Theme): void {
  const vars = { ...themeVars(colorHex(color), base), ...statusVars(base) };
  for (const [k, v] of Object.entries(vars)) el.style.setProperty(k, v);
  const light = (BASES[base] ?? BASES.warm).light;
  el.style.colorScheme = light ? 'light' : 'dark';
  const doc = el.ownerDocument;
  doc.querySelector('meta[name="theme-color"]')?.setAttribute('content', vars['--side']);
  // The app on an iPhone's Home Screen: `black-translucent` draws white status text over the
  // page, which a light base would hide; `default` keeps the page below a dark-text bar. When
  // iOS reads the switched value is not verified on a device (SPEC §6 [verify]).
  doc
    .querySelector('meta[name="apple-mobile-web-app-status-bar-style"]')
    ?.setAttribute('content', light ? 'default' : 'black-translucent');
  try {
    localStorage.setItem(THEME_KEY, base);
  } catch {
    // Not kept: the next load starts with Warm until `/api/me` answers.
  }
}
