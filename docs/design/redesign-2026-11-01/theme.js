// hqc theme: base theme (neutrals) + project color (tint and accent).
// Returns CSS custom properties to set on the app root.
// The same logic runs in the mockups (method theme() in every *.dc.html of round 2).

export const BASES = {
  warm:     { bg: '#1D1B18', side: '#191715', rail: '#100E0C', surface: '#282521', border: '#3A352F', text: '#F2EEE8', text2: '#D8D1C7', muted: '#ADA599', code: '#141210' },
  graphite: { bg: '#1A1C1F', side: '#16181B', rail: '#0E0F11', surface: '#25282C', border: '#353A40', text: '#EDEFF2', text2: '#CDD2D8', muted: '#A3A9B1', code: '#121315' },
  contrast: { bg: '#121110', side: '#0F0E0D', rail: '#070706', surface: '#211F1C', border: '#4A443C', text: '#FFFFFF', text2: '#E9E4DC', muted: '#C6BEB2', code: '#0A0908' },
};

// Project colors offered in Appearance. No yellows: amber is reserved for "waiting".
export const PROJECT_COLORS = {
  Teal: '#3FB8A8', Plum: '#B57BD6', Blue: '#6E9BFF', Rose: '#E77A9A',
  Moss: '#8DBA5B', Coral: '#F08A5D', Cyan: '#4FC3E8', Slate: '#8A93A6',
};

// Fixed status/semantic colors, same in every project and theme.
export const STATUS = {
  waiting: '#F2B35B',       // dot, "Waiting for…" text, rail badge
  online: '#5CC98A',        // worker dot, success checks
  allowedText: '#6FD49A',
  danger: '#F59486',        // danger text
  dangerFill: '#C8483A',    // destructive confirm button
  relay: '#A99BFF',         // messages between sessions (bg 8%, border 32%)
  relayText: '#C4BAFF',
  permissionBg: '#1F1A12', permissionBorder: '#5A4520',
};

const rgb = (h) => [1, 3, 5].map((i) => parseInt(h.slice(i, i + 2), 16));
// mix(a, b, t): t of color a over color b
export const mix = (a, b, t) => {
  const A = rgb(a), B = rgb(b);
  return '#' + A.map((v, i) => Math.round(v * t + B[i] * (1 - t)).toString(16).padStart(2, '0')).join('');
};

export function themeVars(projectColor, base = 'warm') {
  const B = BASES[base] || BASES.warm;
  const c = projectColor;
  return {
    '--bg': B.bg,
    '--side': mix(c, B.side, 0.12),      // sidebar, tinted
    '--side-plain': B.side,              // archive mode sidebar, no tint
    '--rail': mix(c, B.rail, 0.18),      // project bar
    '--row-on': mix(c, B.side, 0.30),    // selected session row
    '--row-hover': mix(c, B.side, 0.18),
    '--surface': B.surface,
    '--border': B.border,
    '--text': B.text,
    '--text2': B.text2,
    '--muted': B.muted,
    '--code': B.code,
    '--accent': c,                       // primary buttons, running dot, links
    '--accent-ink': '#15120F',           // text on accent
    '--accent-soft': mix(c, B.bg, 0.20), // project tile, soft chips
    '--accent-line': mix(c, B.bg, 0.50), // hover borders
    '--bubble': mix(c, B.surface, 0.20), // user message bubble
    '--mark-bg': mix(c, B.rail, 0.42),   // current project tile in the rail (paired with an accent pill at the left edge)
  };
}
