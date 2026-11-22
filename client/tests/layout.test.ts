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

  it("a chat header's meta row is one line: the address of \"started by\" and the folder give way to an ellipsis", () => {
    const meta = rule('.chat-meta');
    expect([meta['flex-wrap'], meta['white-space'], meta['overflow']]).toEqual(['nowrap', 'nowrap', 'clip']);
    for (const sel of ['.chat-spawned', '.chat-path .mono']) {
      expect([rule(sel)['min-width'], rule(sel)['overflow'], rule(sel)['text-overflow']], sel).toEqual(['0', 'clip', 'ellipsis']);
    }
    // At its narrowest the folder holds its copy button: it does not run onto the "/" after it.
    expect(rule('.chat-path')['min-width']).toBe('28px');
    // The copy button neither gives way nor grows with "Copied"; a capped mode keeps its name.
    expect([rule('.chat-path .btn-copy')['flex'], rule('.chat-path .btn-copy')['height']]).toEqual(['none', '24px']);
    expect(rule('.chat:not(.chat-phone) .chat-meta .perm-capped')['min-width']).toBe('5em');
    expect(rule('.chat-spawned a:focus-visible, .chat-path .btn-copy:focus-visible')['outline-offset']).toBe('-2px');
  });

  it("an option's box keeps its 18px beside a long description, the Other field takes the rest", () => {
    expect(rule('.choice input')['flex']).toBe('none');
    expect(rule('.choice-other .input')['flex']).toBe('1');
  });

  it("an answered request's row on a phone wraps the button instead of squeezing the command to nothing", () => {
    // Checked in Chromium and WebKit at 390px: the command ~110px wide, the button on a line of
    // its own inside the row (before: the command 0px, the button 56px past the row).
    expect(rule('.perm-done-row')['flex-wrap']).toBe('wrap');
    expect(rule('.perm-done-what')['flex']).toBe('1 1 4em');
    expect(rule('.perm-done-what')['max-width']).toBe('max-content');
    expect(rule('.perm-done-outcome')['min-width']).toBe('0');
    expect(rule('.perm-done-row .btn-link')['margin-left']).toBe('auto');
  });

  it("a subagent's end card keeps its head inside the card on a phone: the spending cuts first, the task keeps the room", () => {
    // Checked in Chromium and WebKit at 393px on a real end card ("32.7k tokens · 5 tools ·
    // 12s"): before, the task was 0px and the head ran past the card; a card with "367.6k tokens
    // · 265 tools · 1h 17m 22s" made the feed scroll sideways.
    const stats = rule('.subagent-head .subagent-stats');
    expect([stats['flex'], stats['min-width'], stats['overflow'], stats['text-overflow']]).toEqual([
      '0 1000 auto',
      '0',
      'hidden',
      'ellipsis',
    ]);
    const task = rule('.subagent-task');
    expect([task['flex'], task['min-width'], task['overflow']]).toEqual(['0 1 auto', '0', 'hidden']);
  });

  it('a "Background task ·" line keeps inside the feed on a phone: its spending is cut where it does not fit beside the label', () => {
    // The text takes only the room left (a basis of 0); the label stays whole.
    expect(rule('.fold-peek')['flex']).toBe('1');
    expect(rule('.fold-label')['flex']).toBe('none');
    const stats = rule('.fold-stats');
    expect([stats['flex'], stats['min-width'], stats['overflow'], stats['text-overflow']]).toEqual([
      '0 1 auto',
      '0',
      'hidden',
      'ellipsis',
    ]);
  });

  it("a call's line and its result share one line, the result three parts to the line's one; a thinking line is one line", () => {
    // Checked in Chromium and WebKit at 1280 and 390px: one line each, the arguments ~25% when
    // both are long, the time and the mark at the right edge.
    const text = rule('.tool-text');
    expect([text['flex'], text['min-width'], text['display']]).toEqual(['1', '0', 'flex']);
    const line = rule('.tool-group-head > .tool-peek.mono, .tool-line');
    expect([line['flex'], line['max-width']]).toEqual(['1 1 0', 'max-content']);
    const result = rule('.tool-result');
    expect([result['flex'], result['min-width'], result['max-width']]).toEqual(['3 1 0', '0', 'max-content']);
    expect([result['white-space'], result['overflow'], result['text-overflow']]).toEqual(['nowrap', 'hidden', 'ellipsis']);
    expect(rule('.tool-line:empty')['display']).toBe('none');
    const think = rule('.thinking-peek');
    expect([think['min-width'], think['white-space'], think['overflow'], think['text-overflow']]).toEqual([
      '0',
      'nowrap',
      'hidden',
      'ellipsis',
    ]);
  });

  it('on a phone the "started by" line takes the header\'s whole width, one line: a long address ends in an ellipsis instead of breaking', () => {
    expect(rule('.chat-phone .chat-head:has(> .chat-spawned)')['flex-wrap']).toBe('wrap');
    // The title's column has a basis of 0: it stays on the first line beside the buttons.
    expect(rule('.chat-head-main')['flex']).toBe('1');
    const spawned = rule('.chat-phone .chat-spawned');
    expect(spawned['flex-basis']).toBe('100%');
    expect([spawned['white-space'], spawned['overflow'], spawned['text-overflow']]).toEqual(['nowrap', 'clip', 'ellipsis']);
  });

  it("a message's images stand in one row while they fit, any other file keeps a row of its own", () => {
    for (const box of ['.attachments', '.relay-files']) {
      expect(rule(box)['flex-wrap'], box).toBe('wrap');
      expect(rule(box)['flex-direction'], box).toBeUndefined();
    }
    expect(rule('.attachments > .attachment')['flex']).toBe('1 1 100%');
    expect(rule('.relay-files > *')['flex']).toBe('1 1 100%');
    // Two classes, as `.attachments > .attachment`, and after it: it wins for an image.
    expect(rule('.attachments > .attachment-kind-image, .relay-files > .attachment-kind-image')['flex']).toBe('0 1 auto');
    expect(CSS.indexOf('.attachments > .attachment-kind-image')).toBeGreaterThan(CSS.indexOf('.attachments > .attachment {'));
  });

  it("a room's agents in its header wrap, and its agent's menu is the window's width less the gutter, up to 560px", () => {
    // Not checked in a browser.
    expect(rule('.chat-room-agents')['flex-wrap']).toBe('wrap');
    expect(rule('.chat-room-agent')['overflow-wrap']).toBe('anywhere');
    expect([rule('.menu-agent')['width'], rule('.menu-agent')['max-width']]).toEqual(['calc(100vw - 32px)', '560px']);
    expect([rule('.room-prompt')['resize'], rule('.room-prompt')['max-height']]).toEqual(['vertical', '45vh']);
  });

  it("a menu and the layer under it start no text selection or browser menu; a field in a menu still does", () => {
    // A phone row's menu opens under the finger of the long press; on an iPhone the press went
    // on into the browser's own selection of the page. Not checkable outside iOS.
    const menu = rule('.menu-scrim, .menu.ctx-menu');
    expect([menu['-webkit-user-select'], menu['user-select'], menu['-webkit-touch-callout']]).toEqual(['none', 'none', 'none']);
    const field = rule('.ctx-menu input, .ctx-menu textarea');
    expect([field['-webkit-user-select'], field['user-select']]).toEqual(['text', 'text']);
  });

  it("a dot has a width as well as a flex basis: Safari sizes a project's running mark by its items' widths", () => {
    expect([rule('.dot')['flex'], rule('.dot')['width']]).toEqual(['0 0 8px', '8px']);
    for (const sel of ['.sessions-phone .dot', '.preview-row .dot']) {
      expect(rule(sel)['width']).toBe(rule(sel)['flex-basis']);
    }
  });
});
