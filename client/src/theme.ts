// The theme (T49, docs/design/redesign-2026-11-01/theme.js): a base palette of neutrals for
// the whole app plus the current project's color, as CSS variables on the document root,
// recomputed when either changes. Status colors (waiting, online, danger, between sessions)
// are the same in every project and theme.

import type { ProjectColor, Theme } from './proto';

export interface Base {
  bg: string;
  side: string;
  rail: string;
  surface: string;
  border: string;
  text: string;
  text2: string;
  muted: string;
  code: string;
}

/** All dark; Warm is the default (decided 2026-11-01: no light theme). */
export const BASES: Record<Theme, Base> = {
  warm: { bg: '#1D1B18', side: '#191715', rail: '#100E0C', surface: '#282521', border: '#3A352F', text: '#F2EEE8', text2: '#D8D1C7', muted: '#ADA599', code: '#141210' },
  graphite: { bg: '#1A1C1F', side: '#16181B', rail: '#0E0F11', surface: '#25282C', border: '#353A40', text: '#EDEFF2', text2: '#CDD2D8', muted: '#A3A9B1', code: '#121315' },
  contrast: { bg: '#121110', side: '#0F0E0D', rail: '#070706', surface: '#211F1C', border: '#4A443C', text: '#FFFFFF', text2: '#E9E4DC', muted: '#C6BEB2', code: '#0A0908' },
};

export const THEMES: { id: Theme; label: string }[] = [
  { id: 'warm', label: 'Warm' },
  { id: 'graphite', label: 'Graphite' },
  { id: 'contrast', label: 'High contrast' },
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

/** Fixed status and semantic colors, the same in every project and theme. */
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
    '--accent-ink': '#15120F', // text on accent
    '--accent-soft': mix(c, B.bg, 0.2), // project tile, soft chips
    '--accent-line': mix(c, B.bg, 0.5), // hover borders
    '--bubble': mix(c, B.surface, 0.2), // user message bubble
    '--mark-bg': mix(c, B.rail, 0.42), // current project tile in the rail, with an accent pill
  };
}

/** The status colors as variables (`--waiting`, `--online`, ...), set once with the rest. */
export function statusVars(): Record<string, string> {
  const out: Record<string, string> = {};
  for (const [k, v] of Object.entries(STATUS)) out[`--${k.replace(/[A-Z]/g, (m) => '-' + m.toLowerCase())}`] = v;
  return out;
}

/** Sets the variables on `el` (the document root) and the browser's bar color. */
export function applyTheme(el: HTMLElement, color: ProjectColor, base: Theme): void {
  const vars = { ...themeVars(colorHex(color), base), ...statusVars() };
  for (const [k, v] of Object.entries(vars)) el.style.setProperty(k, v);
  el.ownerDocument.querySelector('meta[name="theme-color"]')?.setAttribute('content', vars['--side']);
}
