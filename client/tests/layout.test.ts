// T27: the desktop layout shrinks to the phone breakpoint without horizontal scrolling.
// happy-dom has no layout engine, so this checks the CSS rules that make it so; the widths
// themselves were checked in Chrome (docs/notes/t27-questions.md).

import { describe, expect, it } from 'vitest';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { LIST_WIDTH_MAX, LIST_WIDTH_MIN } from '../src/ui/SessionList';

const CSS = readFileSync(join(import.meta.dirname, '../src/styles.css'), 'utf8').replace(/\/\*[\s\S]*?\*\//g, '');
const HOOKS = readFileSync(join(import.meta.dirname, '../src/ui/hooks.ts'), 'utf8');

/** The declarations of the rule whose selector list is exactly `selector`. */
function rule(selector: string): Record<string, string> {
  for (const m of CSS.matchAll(/([^{}]+)\{([^{}]*)\}/g)) {
    const sels = m[1].split(',').map((s) => s.trim());
    if (sels.join(', ') !== selector) continue;
    return Object.fromEntries(
      m[2]
        .split(';')
        .map((d) => d.split(':'))
        .filter((d) => d.length >= 2)
        .map(([k, ...v]) => [k.trim(), v.join(':').trim()]),
    );
  }
  throw new Error(`no rule ${selector}`);
}

describe('layout widths', () => {
  it('the chat column has no minimum and the session column narrows with the window', () => {
    // T49: project bar 72px, sidebar 280-310px unless its edge was dragged (`--list-w`), and
    // never wider than leaves the chat 300px.
    const cols = rule('.layout')['grid-template-columns'];
    expect(cols).toBe('72px min(var(--list-w, clamp(280px, 25vw, 310px)), calc(100vw - 372px)) minmax(0, 1fr)');
    expect(rule('.sidebar, .main')['min-width']).toBe('0');
    // At the breakpoint the rail and the sessions leave the chat more than half the window.
    const breakpoint = Number(/max-width: (\d+)px/.exec(HOOKS)![1]) + 1;
    expect(breakpoint).toBe(900);
    expect(72 + Math.max(280, breakpoint / 4)).toBeLessThan(breakpoint / 2);
    // In the Compact look the bar is as wide as the longest name, at most
    // 320px: a choice of the user, which leaves the chat at least 300px at the breakpoint.
    const named = rule(':root.rail-names .layout')['grid-template-columns'];
    expect(named).toBe('auto min(var(--list-w, clamp(280px, 25vw, 310px)), calc(100vw - 620px)) minmax(0, 1fr)');
    expect(rule(':root.rail-names .rail')['max-width']).toBe('320px');
    expect(breakpoint - 320 - Math.max(280, breakpoint / 4)).toBe(300);

    // The session column as the grid sizes it, for a window `vw` px wide and a dragged width
    // (the grip's whole range) or none: the chat keeps 300px with either bar, the default
    // column stays as it was.
    const column = (template: string, vw: number, dragged?: number) => {
      const reserve = Number(/calc\(100vw - (\d+)px\)/.exec(template)![1]);
      return Math.min(dragged ?? Math.min(310, Math.max(280, vw / 4)), vw - reserve);
    };
    for (const vw of [breakpoint, 1000, 1280, 1920]) {
      for (const dragged of [undefined, LIST_WIDTH_MIN, 400, LIST_WIDTH_MAX]) {
        expect(vw - 72 - column(cols, vw, dragged), `${vw} ${dragged}`).toBeGreaterThanOrEqual(300);
        expect(vw - 320 - column(named, vw, dragged), `names ${vw} ${dragged}`).toBeGreaterThanOrEqual(300);
      }
      expect(column(cols, vw)).toBe(Math.min(310, Math.max(280, vw / 4)));
      expect(column(named, vw)).toBe(Math.min(310, Math.max(280, vw / 4)));
    }
    // The column's grip sits on the grid, out of the column's clipped box.
    expect(rule('.layout')['position']).toBe('relative');
    expect(rule('.col-grip')['position']).toBe('absolute');
    expect(rule('.col-grip')['grid-column']).toBe('2 / 3');
  });

  it('no fixed width, minimum width or flex basis of 300px or more anywhere', () => {
    const wide = [...CSS.matchAll(/(?:^|[;{\s])((?:min-)?width|flex-basis|flex)\s*:\s*([^;}]*)/g)]
      .filter(([, , v]) => [...v.matchAll(/(\d+)px/g)].some(([, n]) => Number(n) >= 300))
      .map(([, k, v]) => `${k}: ${v}`);
    // `max-width` (the 780px feed) caps, it does not push; it is not matched.
    expect(wide).toEqual([]);
  });

  it('long code and wide tables scroll inside themselves', () => {
    expect(rule('.md-code pre')['overflow-x']).toBe('auto');
    expect(rule('.md-code pre')['white-space']).toBe('pre');
    expect(rule('.md-code')['overflow']).toBe('hidden');
    expect(rule('.md-table')['overflow-x']).toBe('auto');
    expect(rule('.md-table th, .md-table td')['overflow-wrap']).toBe('normal');
    expect(rule('.pre')['overflow']).toBe('auto');
    expect(rule('.timeline')['overflow-y']).toBe('auto');
  });

  it("the user's message keeps its line breaks", () => {
    expect(rule('.msg-text')['white-space']).toBe('pre-wrap');
  });

  it("an option's box keeps its 18px beside a long description, the Other field takes the rest", () => {
    expect(rule('.choice input')['flex']).toBe('none');
    expect(rule('.choice-other .input')['flex']).toBe('1');
  });
});
