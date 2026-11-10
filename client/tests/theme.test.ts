// T49: the theme tokens are those of the mockups' theme.js for every dark base and project
// color (the light bases and their tokens are on top of them), the status colors do not depend
// on the project, and the app puts them on the document root.

import { describe, expect, it, vi } from 'vitest';
import { Window } from 'happy-dom';
// @ts-expect-error plain JS module of the mockups, without types
import * as mockup from '../../docs/design/redesign-2026-11-01/theme.js';
import { PROJECT_COLOR_NAMES, type Theme } from '../src/proto';
import {
  BASES,
  PROJECT_COLORS,
  STATUS,
  STATUS_LIGHT,
  alpha,
  applyTheme,
  colorHex,
  mix,
  statusVars,
  storedTheme,
  themeVars,
} from '../src/theme';

describe('theme tokens', () => {
  it('match the mockups for every dark base and color', () => {
    expect(BASES).toMatchObject(mockup.BASES);
    expect(PROJECT_COLORS.map((c) => [c.label, c.hex])).toEqual(Object.entries(mockup.PROJECT_COLORS));
    for (const base of Object.keys(mockup.BASES) as Theme[]) {
      for (const c of PROJECT_COLORS) {
        // All but the user's bubble: the base's surface in every project, not the mockup's tint.
        const { '--bubble': _bubble, ...want } = mockup.themeVars(c.hex, base);
        expect(themeVars(c.hex, base), `${base} ${c.id}`).toMatchObject(want);
        expect(themeVars(c.hex, base)['--bubble']).toBe(BASES[base].surface);
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
    for (const k of ['--side', '--rail', '--row-on', '--accent', '--mark-bg']) expect(teal[k]).not.toBe(plum[k]);
    // The user's messages are gray in every project.
    for (const k of ['--bg', '--surface', '--text', '--muted', '--side-plain', '--bubble']) expect(teal[k]).toBe(plum[k]);
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
    expect(root.style.getPropertyValue('color-scheme')).toBe('dark');
    // Another project: the accent follows, the amber stays.
    applyTheme(root, 'moss', 'contrast');
    expect(root.style.getPropertyValue('--accent')).toBe('#8DBA5B');
    expect(root.style.getPropertyValue('--waiting')).toBe('#F2B35B');
    void win.happyDOM.close();
  });

  it('gives a light base a light color scheme, darker status colors and a darker accent text', () => {
    const win = new Window();
    const doc = win.document as unknown as Document;
    doc.head.innerHTML = '<meta name="apple-mobile-web-app-status-bar-style" content="black-translucent">';
    const bar = () => doc.querySelector('meta[name="apple-mobile-web-app-status-bar-style"]')!.getAttribute('content');
    const root = doc.documentElement;
    applyTheme(root, 'blue', 'warm_light');
    expect(root.style.getPropertyValue('color-scheme')).toBe('light');
    expect(root.style.getPropertyValue('--bg')).toBe('#FAF7F2');
    expect(root.style.getPropertyValue('--waiting')).toBe(STATUS_LIGHT.waiting);
    expect(root.style.getPropertyValue('--accent')).toBe('#6E9BFF');
    expect(root.style.getPropertyValue('--accent-text')).toBe(mix('#6E9BFF', '#000000', 0.55));
    // The iPhone status bar: dark text over a light base, white over a dark one.
    expect(bar()).toBe('default');
    applyTheme(root, 'blue', 'graphite');
    expect(bar()).toBe('black-translucent');
    void win.happyDOM.close();
  });

  it('remembers the applied base on this device for the next start', () => {
    const win = new Window();
    vi.stubGlobal('localStorage', win.localStorage);
    try {
      expect(storedTheme()).toBe('warm');
      applyTheme(win.document.documentElement as unknown as HTMLElement, 'teal', 'graphite_light');
      expect(storedTheme()).toBe('graphite_light');
      expect(win.localStorage.getItem('hqc.theme')).toBe('graphite_light');
      // Not a theme name: Warm.
      win.localStorage.setItem('hqc.theme', 'light');
      expect(storedTheme()).toBe('warm');
    } finally {
      vi.unstubAllGlobals();
      void win.happyDOM.close();
    }
  });
});
