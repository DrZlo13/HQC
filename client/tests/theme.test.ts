// T49: the theme tokens are those of the mockups' theme.js for every base and project color,
// the status colors do not depend on either, and the app puts them on the document root.

import { describe, expect, it } from 'vitest';
import { Window } from 'happy-dom';
// @ts-expect-error plain JS module of the mockups, without types
import * as mockup from '../../docs/design/redesign-2026-11-01/theme.js';
import { PROJECT_COLOR_NAMES, THEME_NAMES } from '../src/proto';
import { BASES, PROJECT_COLORS, STATUS, alpha, applyTheme, colorHex, mix, statusVars, themeVars } from '../src/theme';

describe('theme tokens', () => {
  it('match the mockups for every base and color', () => {
    expect(Object.keys(BASES)).toEqual(Object.keys(mockup.BASES));
    expect(BASES).toEqual(mockup.BASES);
    expect(PROJECT_COLORS.map((c) => [c.label, c.hex])).toEqual(Object.entries(mockup.PROJECT_COLORS));
    for (const base of THEME_NAMES) {
      for (const c of PROJECT_COLORS) {
        expect(themeVars(c.hex, base), `${base} ${c.id}`).toEqual(mockup.themeVars(c.hex, base));
      }
    }
    expect(STATUS).toMatchObject(mockup.STATUS);
  });

  it('names the same colors as the hub, the project tile at 24 %', () => {
    expect(PROJECT_COLORS.map((c) => c.id)).toEqual([...PROJECT_COLOR_NAMES]);
    expect(colorHex('coral')).toBe('#F08A5D');
    expect(alpha('#6E9BFF', 0.24)).toBe('rgba(110,155,255,0.24)');
    expect(mix('#3FB8A8', '#100E0C', 0.42)).toBe('#24554e');
  });

  it('tints the bar and the sidebar by the project, keeps the neutrals of the base', () => {
    const teal = themeVars(colorHex('teal'), 'warm');
    const plum = themeVars(colorHex('plum'), 'warm');
    const graphite = themeVars(colorHex('teal'), 'graphite');
    for (const k of ['--side', '--rail', '--row-on', '--accent', '--bubble', '--mark-bg']) expect(teal[k]).not.toBe(plum[k]);
    for (const k of ['--bg', '--surface', '--text', '--muted', '--side-plain']) expect(teal[k]).toBe(plum[k]);
    expect(graphite['--bg']).toBe('#1A1C1F');
    expect(teal['--mark-bg']).toBe(mix('#3FB8A8', BASES.warm.rail, 0.42));
  });

  it('sets the variables on the root, status colors included', () => {
    const win = new Window();
    const doc = win.document as unknown as Document;
    doc.head.innerHTML = '<meta name="theme-color" content="#000000">';
    const root = doc.documentElement;
    applyTheme(root, 'blue', 'contrast');
    expect(root.style.getPropertyValue('--accent')).toBe('#6E9BFF');
    expect(root.style.getPropertyValue('--bg')).toBe('#121110');
    expect(root.style.getPropertyValue('--waiting')).toBe('#F2B35B');
    expect(root.style.getPropertyValue('--waiting-ink')).toBe(STATUS.waitingInk);
    expect(statusVars()['--danger-fill']).toBe('#C8483A');
    expect(doc.querySelector('meta[name="theme-color"]')!.getAttribute('content')).toBe(themeVars('#6E9BFF', 'contrast')['--side']);
    // Another project: the accent follows, the amber stays.
    applyTheme(root, 'moss', 'contrast');
    expect(root.style.getPropertyValue('--accent')).toBe('#8DBA5B');
    expect(root.style.getPropertyValue('--waiting')).toBe('#F2B35B');
    void win.happyDOM.close();
  });
});
