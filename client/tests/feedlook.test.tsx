// @vitest-environment happy-dom
// The feed's look (SPEC §6, the feed): a message's time at the right end of its first line,
// inside a bubble or a plate; its copy and fork buttons on hover at the other edge; "joined the
// turn" on a message that went into the turn already running; the marks of the one-column
// gutter ("Settings" → "Feed" → "One column").

import { afterEach, describe, expect, it, vi } from 'vitest';
import { render } from 'preact';
import { act } from 'preact/test-utils';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { parseEvent, type HqcEvent } from '../src/proto';
import { Timeline } from '../src/timeline';
import { ChatItems, type ChatActions } from '../src/ui/cards';
import { formatTime } from '../src/ui/labels';

const WIRE = join(import.meta.dirname, '../../crates/hqc-proto/examples/wire');
const CSS = readFileSync(join(import.meta.dirname, '../src/styles.css'), 'utf8');
/** The declarations of the rule whose selector list is exactly `sel`. */
const rule = (sel: string) => {
  const m = new RegExp(`\\n${sel.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')} \\{([^}]*)\\}`).exec(CSS);
  if (!m) throw new Error(`no rule ${sel}`);
  return m[1];
};

const ev = (name: string, patch: (e: any) => void = () => {}): HqcEvent => {
  const e = JSON.parse(readFileSync(join(WIRE, `${name}.json`), 'utf8'));
  patch(e);
  return parseEvent(e);
};
const id = (seq: number) => `0199f3a2-7c10-7000-8000-${String(seq).padStart(12, '0')}`;
const at = (seq: number) => new Date(Date.parse('2026-10-22T10:00:00Z') + seq * 60_000).toISOString();
const user = (seq: number, text: string, patch: (e: any) => void = () => {}) =>
  ev('event.user.message', (e) => {
    e.seq = seq;
    e.event_id = id(seq);
    e.ts = at(seq);
    e.payload = { text, artifact_ids: [] };
    patch(e);
  });
const agent = (seq: number, text: string, inReplyTo: number, final = false) =>
  ev('event.agent.message', (e) => {
    e.seq = seq;
    e.event_id = id(seq);
    e.ts = at(seq);
    e.payload = { text, final, in_reply_to: id(inReplyTo) };
  });

let root: HTMLElement;
afterEach(() => {
  if (root) render(null, root);
  document.body.innerHTML = '';
  vi.unstubAllGlobals();
});

function show(events: HqcEvent[], over: Partial<ChatActions> = {}) {
  const timeline = new Timeline();
  for (const e of events) timeline.apply(e);
  const actions: ChatActions = {
    timeline,
    approve: vi.fn(async () => {}),
    answer: vi.fn(async () => {}),
    resend: vi.fn(),
    discard: vi.fn(),
    openArtifact: vi.fn(),
    ...over,
  };
  if (root) render(null, root);
  root = document.createElement('div');
  document.body.appendChild(root);
  const draw = () => act(() => render(<ChatItems items={timeline.items()} actions={actions} />, root));
  draw();
  return { timeline, actions, draw };
}

describe('the time on the first line', () => {
  it('stands first in the bubble and in the agent text, before what wraps around it', () => {
    show([user(1, 'Run the tests'), agent(2, 'All tests pass.', 1, true)]);
    const bubble = root.querySelector('.msg-user > .msg-bubble')!;
    const time = bubble.firstElementChild!;
    expect(time.className).toBe('msg-time');
    expect(time.textContent).toBe(formatTime(at(1)));
    expect(time.querySelector('time')!.getAttribute('datetime')).toBe(at(1));
    expect(time.nextElementSibling!.classList.contains('msg-text')).toBe(true);
    const text = root.querySelector('.msg-agent')!;
    expect(text.firstElementChild!.className).toBe('msg-time');
    expect(text.firstElementChild!.textContent).toBe(formatTime(at(2)));
    expect(text.firstElementChild!.nextElementSibling!.classList.contains('md')).toBe(true);
    // Not on a line under the message any more.
    expect(root.querySelector('.msg-user > .meta, .msg-agent > .meta')).toBeNull();
  });

  it('keeps what the time says about delivery and schedules beside it', () => {
    show([
      user(1, 'nightly', (e) => (e.payload.schedule_id = id(99))),
      user(2, 'lost', (e) => {
        e.delivery = 'failed';
        delete e.delivered_at;
      }),
    ]);
    const [a, b] = [...root.querySelectorAll('.msg-bubble > .msg-time')].map((t) => t.textContent);
    expect(a).toBe(`${formatTime(at(1))} · scheduled`);
    expect(b).toBe(`${formatTime(at(2))} · not delivered`);
  });

  it('styles.css floats it to the right, on a line as tall as the text', () => {
    const r = rule('.msg-time');
    expect(r).toMatch(/float: right;/);
    expect(r).toMatch(/line-height: var\(--feed-line\);/);
    // The setting that put it under the message is gone.
    expect(CSS).not.toMatch(/time-in-gap/);
  });
});

describe('the hover buttons', () => {
  it("are the user's copy and fork in the bubble left of the time, the agent's copy in a column", async () => {
    const fork = vi.fn();
    show([user(1, 'Run the tests'), agent(2, 'All tests pass.', 1, true)], { fork });
    const time = root.querySelector('.msg-user .msg-bubble > .msg-time')!;
    const mine = time.firstElementChild!;
    expect(mine.className).toBe('msg-time-actions');
    expect(mine.nextElementSibling!.tagName).toBe('TIME');
    expect([...mine.querySelectorAll('button')].map((b) => b.getAttribute('aria-label'))).toEqual(['Copy', 'Fork from here']);
    expect(root.querySelector('.msg-user > .msg-actions')).toBeNull();
    const its = root.querySelector('.msg-agent > .msg-actions')!;
    expect([...its.querySelectorAll('button')].map((b) => b.getAttribute('aria-label'))).toEqual(['Copy']);
    await act(async () => mine.querySelector<HTMLButtonElement>('.btn-fork')!.click());
    expect(fork).toHaveBeenCalledWith(expect.objectContaining({ seq: 1 }));
  });

  it('sit left of the feed, at the edge opposite the time, flush with the block, inside the timeline padding', () => {
    // The block is the anchor: its left edge is the feed's.
    expect(rule('.feed-block')).toMatch(/position: relative;/);
    const r = rule('.msg-actions');
    expect(r).toMatch(/position: absolute;/);
    expect(r).toMatch(/top: 0;/);
    expect(r).toMatch(/flex-direction: column;/);
    expect(r).not.toMatch(/right:/);
    const px = (decls: string, prop: string) => Number(new RegExp(`(?:^|;|\\s)${prop}: (-?[\\d.]+)px`).exec(decls)![1]);
    // Its right edge is the block's left edge: no gap where the block's hover drops on the way
    // to the buttons. Button + 1 px padding + 1 px border on each side.
    const pad = px(r, 'padding');
    const border = Number(/border: (\d+)px solid/.exec(r)![1]);
    const across = (button: number) => button + 2 * pad + 2 * border;
    const wide = px(rule('.chat:not(.chat-phone) .msg-actions .btn-copy-idle'), 'width');
    expect(px(r, 'left')).toBe(-across(wide));
    expect(across(wide)).toBeLessThanOrEqual(px(rule('.timeline'), 'padding'));
    const column = rule(':root.feed-column .msg-actions');
    const narrow = px(rule(':root.feed-column .chat:not(.chat-phone) .msg-actions .btn-copy-idle'), 'width');
    expect(px(column, 'left')).toBe(-across(narrow));
    expect(across(narrow)).toBeLessThanOrEqual(24);
    expect(rule(':root.feed-column .chat:not(.chat-phone) .timeline')).toMatch(/padding: 16px 24px 12px;/);
    // And a transparent bridge over the seam, so a slow approach keeps the hover.
    const bridge = rule('.chat:not(.chat-phone) .msg-actions::after');
    expect(bridge).toMatch(/content: '';/);
    expect(bridge).toMatch(/position: absolute;/);
    expect(bridge).toMatch(/left: 100%;/);
    expect(CSS).toMatch(/\n\.feed-block:hover \.msg-actions,\s*\.msg-actions:focus-within \{\s*opacity: 1;/);
  });

  it("show the user's in the bubble on hover only, taking no room while hidden, small enough for the time's line", () => {
    const r = rule('.msg-time-actions');
    expect(r).toMatch(/opacity: 0;/);
    expect(r).toMatch(/position: absolute;/);
    expect(r).toMatch(/right: 100%;/);
    expect(rule('.msg-bubble > .msg-time')).toMatch(/position: relative;/);
    // A bubble with them holds them; on a plate with a note, and on a touch screen, they are in
    // the line's flow.
    expect(rule('.msg-bubble-acts')).toMatch(/min-width: 7\.5em;/);
    expect(rule(':root.feed-column .msg-noted .msg-time-actions')).toMatch(/position: static;/);
    expect(CSS).toMatch(/@media \(hover: none\) \{\s*\.msg-time-actions \{\s*position: static;/);
    expect(CSS).toMatch(/\n\.feed-block:hover \.msg-time-actions,\s*\.msg-time-actions:focus-within \{\s*opacity: 1;/);
    expect(rule('.chat:not(.chat-phone) .msg-time-actions .btn-copy-idle')).toMatch(/height: 18px;/);
  });

  it("on a phone are a row under the user's message, not in the bubble", () => {
    vi.stubGlobal('matchMedia', (media: string) => ({ matches: true, media, addEventListener: () => {}, removeEventListener: () => {} }));
    try {
      show([user(1, 'Run the tests')], { fork: vi.fn() });
      const row = root.querySelector('.msg-user > .msg-actions')!;
      expect([...row.querySelectorAll('button')].map((b) => b.getAttribute('aria-label'))).toEqual(['Copy', 'Fork from here']);
      expect(root.querySelector('.msg-time-actions')).toBeNull();
    } finally {
      vi.unstubAllGlobals();
    }
  });

  it("keep the phone size: the smaller buttons are a computer's only", () => {
    // No rule makes them smaller than the 24 px `.btn-copy-idle` outside `.chat:not(.chat-phone)`.
    expect(rule('.btn-copy-idle')).toMatch(/width: 24px;/);
    const small = [...CSS.matchAll(/\n([^\n{}]*\.msg-actions[^\n{}]*) \{([^}]*)\}/g)].filter(([, , d]) => /(width|height): (1\d|2[0-3])px/.test(d));
    expect(small.length).toBeGreaterThan(0);
    for (const [, sel] of small) expect(sel).toContain('.chat:not(.chat-phone)');
  });

  it('show the copy result as an icon in the same button, not a caption that widens the column', async () => {
    const writeText = vi.fn(async (_: string) => {});
    vi.stubGlobal('isSecureContext', true);
    Object.defineProperty(window.navigator, 'clipboard', { value: { writeText }, configurable: true });
    show([agent(2, 'All tests pass.', 1, true)]);
    const btn = () => root.querySelector<HTMLButtonElement>('.msg-agent > .msg-actions .btn-copy')!;
    // The clipboard answers after the click.
    const click = () => act(async () => {
      btn().click();
      await new Promise((r) => setTimeout(r, 0));
    });
    await click();
    expect(writeText).toHaveBeenCalledWith('All tests pass.');
    expect(btn().classList.contains('btn-copy-idle')).toBe(true);
    expect(btn().classList.contains('btn-copy-done')).toBe(true);
    expect(btn().textContent).toBe('');
    expect(btn().querySelector('svg')).not.toBeNull();
    expect(btn().getAttribute('aria-label')).toBe('Copied');
    writeText.mockRejectedValueOnce(new Error('denied'));
    show([agent(2, 'All tests pass.', 1, true)]);
    await click();
    expect(btn().classList.contains('btn-copy-failed')).toBe(true);
    expect(btn().textContent).toBe('');
    expect(btn().getAttribute('aria-label')).toBe('Copy failed');
    // Coloured over `.btn-copy:hover`, which the pointer on it would apply.
    expect(rule('.msg-actions .btn-copy.btn-copy-done')).toMatch(/color: var\(--online\);/);
    expect(rule('.msg-actions .btn-copy.btn-copy-failed')).toMatch(/color: var\(--danger\);/);
  });
});

describe('joined the turn', () => {
  // The hub keeps it on the message (`payload.joined`) when the worker acks the input
  // `joined`, and says so in the `delivery` frame (SPEC §17.2, §6): the client shows the fact,
  // it does not guess it from the order of the events.
  const waiting = (seq: number, text: string) =>
    user(seq, text, (e) => {
      e.delivery = 'pending';
      delete e.delivered_at;
    });
  const notes = () => [...root.querySelectorAll('.msg-user')].map((m) => m.querySelector('.msg-note')?.textContent ?? null);

  it('a message the hub keeps as joined has the note, in its bubble after the time', () => {
    show([user(1, 'Fix the search'), agent(2, 'Looking.', 1), user(3, 'and keep it fast', (e) => (e.payload.joined = true))]);
    expect(notes()).toEqual([null, 'joined the turn']);
    // In the bubble or the plate, after the time and before the text: the time keeps its first
    // place; styles.css puts the note on the first line of a bubble with the time at its end,
    // left of the time on a plate.
    const bubble = root.querySelectorAll('.msg-user')[1].querySelector('.msg-bubble')!;
    expect(bubble.classList.contains('msg-noted')).toBe(true);
    expect([...bubble.children].map((c) => c.className)).toEqual(['msg-time', 'msg-note', 'text msg-text']);
    expect(bubble.firstElementChild!.textContent).toBe(formatTime(at(3)));
    expect(root.querySelectorAll('.msg-noted')).toHaveLength(1);
  });

  it('styles.css: the first line of a bubble with the time, left of the time on a plate', () => {
    expect(rule('.msg-note')).toMatch(/display: inline;/);
    expect(rule(':root.feed-column .msg-note')).toMatch(/float: right;/);
    expect(rule(':root.feed-column .msg-noted > .msg-time')).toMatch(/margin: 0 0 0 8px;/);
  });

  it('not a message that waited for the turn to end: typed during it, the final answer after it, then delivered', () => {
    const { timeline, draw } = show([user(1, 'Fix the search'), agent(2, 'Looking.', 1), waiting(3, 'then the list'), agent(4, 'Search done.', 1, true)]);
    timeline.applyDelivery({ session_id: 's', event_id: id(3), seq: 3, delivery: 'delivered', delivered_at: at(5) });
    draw();
    expect(notes()).toEqual([null, null]);
  });

  it('a delivery frame that says joined puts the note on the message', () => {
    const { timeline, draw } = show([user(1, 'Fix the search'), agent(2, 'Looking.', 1), waiting(3, 'and keep it fast')]);
    expect(notes()).toEqual([null, null]);
    timeline.applyDelivery({ session_id: 's', event_id: id(3), seq: 3, delivery: 'delivered', delivered_at: at(3), joined: true });
    draw();
    expect(notes()).toEqual([null, 'joined the turn']);
  });

  it("the hub's copy of a message that joined meanwhile carries it (a frame lost with the socket)", () => {
    const { timeline, draw } = show([user(1, 'Fix the search'), agent(2, 'Looking.', 1), waiting(3, 'and keep it fast')]);
    const copy = user(3, 'and keep it fast', (e) => (e.payload.joined = true));
    timeline.refresh([copy]);
    draw();
    expect(notes()).toEqual([null, 'joined the turn']);
    // A copy in the stream (the subscription again) does the same.
    const t2 = show([user(1, 'go'), waiting(2, 'and this')]);
    t2.timeline.apply(user(2, 'and this', (e) => (e.payload.joined = true)));
    t2.draw();
    expect(notes()).toEqual([null, 'joined the turn']);
  });
});

describe('the one-column gutter', () => {
  it('a mark for every item but the user plates and the service lines, which span the feed', () => {
    const seq = (s: number) => (e: any) => ((e.seq = s), (e.ts = at(s)));
    show([
      user(1, 'Run the tests'),
      ev('event.agent.thinking', seq(2)),
      agent(3, 'Looking.', 1),
      ev('event.tool.call', (e) => (seq(4)(e), (e.payload = { call_id: 'c1', tool: 'Bash', args_preview: '{"command":"ls"}' }))),
      ev('event.tool.result', (e) => (seq(5)(e), (e.payload = { call_id: 'c1', error: false, result_preview: 'ok', duration_ms: 5 }))),
      ev('event.peer.message', seq(6)),
      ev('event.attachment', seq(7)),
      ev('event.clarify.request', seq(8)),
      ev('event.system', seq(9)),
      ev('event.approval.request', seq(10)),
    ]);
    const blocks = [...root.querySelectorAll<HTMLElement>('.feed-block')].map((b) => {
      const m = [...b.children].find((c) => c.classList.contains('feed-mark'));
      return [b.classList.contains('feed-full'), m ? m.className.replace('feed-mark feed-mark-', '') : null];
    });
    expect(blocks).toEqual([
      [true, null],
      [false, 'fold'],
      [false, 'dot'],
      [false, 'ok'],
      [false, 'relay'],
      [false, 'file'],
      [false, 'ask'],
      [true, null],
      [false, 'perm'],
    ]);
    // A mark is decoration: the block says what it is in words.
    expect(root.querySelector('.feed-mark:not([aria-hidden=true])')).toBeNull();
  });

  it('styles.css shows the marks and the gutter only in one column, and drops the marks the items carry', () => {
    expect(rule('.feed-mark')).toMatch(/display: none;/);
    expect(rule(':root.feed-column .feed-mark')).toMatch(/position: absolute;/);
    expect(rule(':root.feed-column .feed-block')).toMatch(/padding-left: 22px;/);
    expect(CSS).toMatch(
      /:root\.feed-column \.feed-block > \.tool-group > \.tool-group-head > :is\(\.mark, \.pulse-dot\),\s*:root\.feed-column \.feed-block > :is\(\.thinking, \.line-fold\) > summary > \.icon,\s*:root\.feed-column \.feed-block > \.relay > \.relay-head > \.relay-arrow \{\s*display: none;/,
    );
    // A fold's chevron turns with it: the mark comes after the block.
    expect(CSS).toMatch(/:root\.feed-column \.feed-block > details\[open\] ~ \.feed-mark \.icon \{\s*transform: rotate\(90deg\);/);
  });
});
