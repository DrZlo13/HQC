// T27: the desktop layout shrinks to the phone breakpoint without horizontal scrolling.
// happy-dom has no layout engine, so this checks the CSS rules that make it so; the widths
// themselves were checked in Chrome (docs/notes/t27-questions.md).

import { describe, expect, it } from 'vitest';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';

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
    // T49: project bar 72px, sidebar 280-310px.
    const cols = rule('.layout')['grid-template-columns'];
    expect(cols).toBe('72px clamp(280px, 25vw, 310px) minmax(0, 1fr)');
    expect(rule('.sidebar, .main')['min-width']).toBe('0');
    // At the breakpoint the rail and the sessions leave the chat more than half the window.
    const breakpoint = Number(/max-width: (\d+)px/.exec(HOOKS)![1]) + 1;
    expect(breakpoint).toBe(900);
    expect(72 + Math.max(280, breakpoint / 4)).toBeLessThan(breakpoint / 2);
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
