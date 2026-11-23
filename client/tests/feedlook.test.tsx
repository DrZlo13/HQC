// @vitest-environment happy-dom
// The feed's look (SPEC §6, the feed): a message's time at the right end of its first line,
// inside a bubble or a plate; its search, copy, quote and fork buttons, always shown, left of
// the time; "joined the turn" on a message that went into the turn already running; the marks
// of the one-column gutter ("Settings" → "Feed" → "One column").

import { afterEach, describe, expect, it, vi } from 'vitest';
import { render } from 'preact';
import { act } from 'preact/test-utils';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { parseEvent, type HqcEvent } from '../src/proto';
import { Timeline } from '../src/timeline';
import { useRef } from 'preact/hooks';
import { ChatItems, type ChatActions } from '../src/ui/cards';
import { SelectionContext, useChatSelection } from '../src/ui/selection';
import { formatTime } from '../src/ui/labels';
import { WEB_SEARCHES } from '../src/websearch';

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

describe('the text of a clipped selection', () => {
  /** The chat's selection around the feed, as Chat has it. */
  function Scope({ children }: { children: preact.ComponentChildren }) {
    const ref = useRef<HTMLDivElement>(null);
    return (
      <SelectionContext.Provider value={useChatSelection(ref)}>
        <div ref={ref}>{children}</div>
      </SelectionContext.Provider>
    );
  }
  /** Draws `events`, selects from `from` to `to(root)` and clicks the Quote of the message that holds it: what it quotes. */
  async function quoted(events: HqcEvent[], from: (r: HTMLElement) => [Node, number], to: (r: HTMLElement) => [Node, number], over: Partial<ChatActions> = {}) {
    const timeline = new Timeline();
    for (const e of events) timeline.apply(e);
    const quote = vi.fn();
    const actions: ChatActions = { timeline, approve: vi.fn(async () => {}), answer: vi.fn(async () => {}), resend: vi.fn(), discard: vi.fn(), openArtifact: vi.fn(), quote, webSearch: WEB_SEARCHES[0], ...over };
    root = document.createElement('div');
    document.body.appendChild(root);
    await act(() =>
      render(
        <Scope>
          <ChatItems items={timeline.items()} actions={actions} />
        </Scope>,
        root,
      ),
    );
    await act(async () => {
      const r = document.createRange();
      r.setStart(...from(root));
      r.setEnd(...to(root));
      document.getSelection()!.removeAllRanges();
      document.getSelection()!.addRange(r);
      document.dispatchEvent(new Event('selectionchange'));
    });
    // The Quote beside the Search: the message that holds the selection.
    await act(async () => root.querySelector('.btn-msg-search:not(.btn-msg-search-idle)')!.parentElement!.querySelector<HTMLButtonElement>('.btn-msg-quote')!.click());
    return quote.mock.calls[0][0] as string;
  }
  const textOf = (r: HTMLElement, text: string) => {
    const w = document.createTreeWalker(r, NodeFilter.SHOW_TEXT);
    for (let n = w.nextNode(); n; n = w.nextNode()) if (n.textContent === text) return n;
    throw new Error(`no text «${text}»`);
  };

  it('has a tab after a table cell and one line break after a row and after a list item', async () => {
    const md = 'Intro\n\n| File | Status |\n|---|---|\n| a.rs | ok |\n| b.rs | no |\n\n- one\n- two\n\nOutro';
    const got = await quoted(
      [agent(1, md, 0, true), user(2, 'next')],
      (r) => [textOf(r, 'Intro'), 0],
      (r) => [r.querySelector('.msg-user')!, 0],
    );
    expect(got).toBe('Intro\n\nFile\tStatus\na.rs\tok\nb.rs\tno\n\none\ntwo\n\nOutro');
  });

  it("leaves the answer's usage line out when the range runs past the answer's end", async () => {
    const usage = ev('event.usage', (e) => {
      e.seq = 2;
      e.event_id = id(2);
      e.ts = at(2);
      e.payload = { input_tokens: 2, output_tokens: 234, cost_usd: 0.163 };
    });
    const got = await quoted(
      [agent(1, 'Para one.\n\nPara two.', 0, true), usage, user(3, 'next')],
      (r) => [textOf(r, 'Para two.'), 0],
      (r) => [r.querySelector('.msg-user')!, 0],
    );
    expect(got).toBe('Para two.');
  });

  it("leaves a code block's caption (its language and Copy) out", async () => {
    const got = await quoted(
      [agent(1, 'Intro\n\n```rust\nfn main() {}\n```\n\nOutro', 0, true), user(2, 'next')],
      (r) => [textOf(r, 'Intro'), 0],
      (r) => [r.querySelector('.msg-user')!, 0],
    );
    expect(got).toBe('Intro\n\nfn main() {}\n\nOutro');
  });

  it("keeps a paragraph's single line breaks", async () => {
    const got = await quoted(
      [agent(1, 'line one\nline two', 0, true), user(2, 'next')],
      (r) => [textOf(r, 'line one'), 0],
      (r) => [r.querySelector('.msg-user')!, 0],
    );
    expect(got).toBe('line one\nline two');
  });

  it("leaves a room post's head (name, model, time) out when the range enters the post from before it", async () => {
    const post = ev('event.peer.message', (e) => {
      e.seq = 2;
      e.event_id = id(2);
      e.ts = at(2);
      e.payload = { ...e.payload, from_session_id: id(77), from: 'Optimist', text: 'Opening statement.' };
    });
    const got = await quoted(
      [user(1, 'Question'), post],
      (r) => [r, 0],
      (r) => [textOf(r, 'Opening statement.'), 7],
      { roomAgent: (sid) => (sid === id(77) ? { index: 0, name: 'Optimist', model: 'claude-sonnet-5-5' } : undefined) },
    );
    expect(got).toBe('Opening');
  });

  it('keeps the Search slot in place, hidden, until the message holds a selection, then shows it', async () => {
    const timeline = new Timeline();
    timeline.apply(agent(1, 'All tests pass.', 0, true));
    const actions: ChatActions = { timeline, approve: vi.fn(async () => {}), answer: vi.fn(async () => {}), resend: vi.fn(), discard: vi.fn(), openArtifact: vi.fn(), quote: vi.fn(), webSearch: WEB_SEARCHES[0] };
    root = document.createElement('div');
    document.body.appendChild(root);
    await act(() =>
      render(
        <Scope>
          <ChatItems items={timeline.items()} actions={actions} />
        </Scope>,
        root,
      ),
    );
    const slot = () => root.querySelector<HTMLButtonElement>('.btn-msg-search')!;
    expect(root.querySelector('.msg-buttons')!.firstElementChild).toBe(slot());
    expect(slot().classList.contains('btn-msg-search-idle')).toBe(true);
    expect(slot().getAttribute('aria-hidden')).toBe('true');
    expect(slot().tabIndex).toBe(-1);
    await act(async () => {
      const r = document.createRange();
      r.selectNodeContents(textOf(root, 'All tests pass.'));
      document.getSelection()!.removeAllRanges();
      document.getSelection()!.addRange(r);
      document.dispatchEvent(new Event('selectionchange'));
    });
    expect(slot().classList.contains('btn-msg-search-idle')).toBe(false);
    expect(slot().getAttribute('aria-hidden')).toBeNull();
    expect(slot().tabIndex).toBe(0);
  });

  const thinking = (text: string) =>
    ev('event.agent.thinking', (e) => {
      e.seq = 1;
      e.event_id = id(1);
      e.ts = at(1);
      e.payload = { text };
    });

  it("quotes only the visible line of a folded reasoning when the range runs out of it, the whole text of an open one", async () => {
    const text = 'Idea one.\nIdea two.';
    const got = await quoted(
      [thinking(text), user(2, 'next')],
      (r) => [r.querySelector('.thinking-peek')!, 0],
      (r) => [r.querySelector('.msg-user')!, 0],
    );
    // The line as it is shown, without its "Thinking · ", not the hidden text.
    expect(got).toBe('Idea one. Idea two.');
    const open = await quoted(
      [thinking(text), user(2, 'next')],
      (r) => {
        r.querySelector('details')!.open = true;
        return [textOf(r, text), 0];
      },
      (r) => [r.querySelector('.msg-user')!, 0],
    );
    expect(open).toBe('Idea one.\nIdea two.');
  });

  it("quotes a reasoning's selection, else its first two sentences; its buttons do not fold it", async () => {
    const text = 'First idea. Second idea. Third idea.';
    const got = await quoted(
      [thinking(text)],
      (r) => [textOf(r, text), 12],
      (r) => [textOf(r, text), 23],
    );
    expect(got).toBe('Second idea');
    const timeline = new Timeline();
    timeline.apply(thinking(text));
    const quote = vi.fn();
    const actions: ChatActions = { timeline, approve: vi.fn(async () => {}), answer: vi.fn(async () => {}), resend: vi.fn(), discard: vi.fn(), openArtifact: vi.fn(), quote, webSearch: WEB_SEARCHES[0] };
    root = document.createElement('div');
    document.body.appendChild(root);
    await act(() => render(<ChatItems items={timeline.items()} actions={actions} />, root));
    expect(root.querySelector('.thinking summary .msg-buttons')).not.toBeNull();
    await act(async () => root.querySelector<HTMLButtonElement>('.thinking .btn-msg-quote')!.click());
    expect(quote).toHaveBeenLastCalledWith('First idea. Second idea. …');
    const details = root.querySelector('details')!;
    expect(details.open).toBe(false);
    await act(async () => root.querySelector<HTMLButtonElement>('.thinking .btn-copy:not(.btn-msg-search):not(.btn-msg-quote)')!.click());
    expect(details.open).toBe(false);
  });

  it("leaves the head of a message from another session out too", async () => {
    const peer = ev('event.peer.message', (e) => {
      e.seq = 2;
      e.event_id = id(2);
      e.ts = at(2);
      e.payload = { ...e.payload, text: 'Is the build green?' };
    });
    const got = await quoted(
      [user(1, 'Question'), peer],
      (r) => [r, 0],
      (r) => [textOf(r, 'Is the build green?'), 6],
    );
    expect(got).toBe('Is the');
  });
});

describe('the message buttons', () => {
  const labels = (el: Element) => [...el.querySelectorAll('button')].map((b) => b.getAttribute('aria-label'));
  const phoneMedia = () =>
    vi.stubGlobal('matchMedia', (media: string) => ({ matches: true, media, addEventListener: () => {}, removeEventListener: () => {} }));
  const answer = (seq: number) =>
    ev('event.approval.response', (e) => {
      e.seq = seq;
      e.event_id = id(seq);
      e.ts = at(seq);
      e.payload = { request_id: 'req-1', option_id: 'deny', message: 'Use rg instead', by: e.author.id };
    });
  const usage = (seq: number) =>
    ev('event.usage', (e) => {
      e.seq = seq;
      e.event_id = id(seq);
      e.ts = at(seq);
      e.payload = { input_tokens: 2, output_tokens: 234, cost_usd: 0.163 };
    });
  const search = { webSearch: WEB_SEARCHES[0] };

  it("are Search, Copy, Quote and the user's Fork in the time's line of a message, left of the time", async () => {
    const fork = vi.fn();
    show([user(1, 'Run the tests'), agent(2, 'All tests pass.', 1, true), answer(3)], { fork, quote: vi.fn(), ...search });
    const strip = (sel: string) => root.querySelector(`${sel} > .msg-time`)!.firstElementChild!;
    const mine = strip('.msg-user:not(.msg-answer) .msg-bubble');
    expect(mine.className).toBe('msg-buttons');
    expect(mine.nextElementSibling!.tagName).toBe('TIME');
    expect(labels(mine)).toEqual(['Search in Google', 'Copy', 'Quote', 'Fork from here']);
    const its = strip('.msg-agent');
    expect(its.className).toBe('msg-buttons');
    expect(its.nextElementSibling!.tagName).toBe('TIME');
    expect(labels(its)).toEqual(['Search in Google', 'Copy', 'Quote']);
    expect(labels(strip('.msg-answer .msg-bubble'))).toEqual(['Search in Google', 'Copy', 'Quote']);
    expect(root.querySelector('.msg-actions')).toBeNull();
    await act(async () => mine.querySelector<HTMLButtonElement>('.btn-fork')!.click());
    expect(fork).toHaveBeenCalledWith(expect.objectContaining({ seq: 1 }));
  });

  it('are in the head of a message from another session, a reply to one and a room post, the same on a phone', () => {
    const peer = ev('event.peer.message', (e) => {
      e.seq = 1;
      e.event_id = id(1);
      e.ts = at(1);
    });
    const room = ev('event.peer.message', (e) => {
      e.seq = 2;
      e.event_id = id(2);
      e.ts = at(2);
      e.payload = { ...e.payload, from_session_id: id(77), text: 'Opening.' };
    });
    const strips = () => [...root.querySelectorAll('.relay-when .msg-buttons, .room-post-when .msg-buttons')].map(labels);
    const over = { quote: vi.fn(), ...search, roomAgent: (sid: string) => (sid === id(77) ? { index: 0, name: 'Optimist' } : undefined) };
    show([peer, room], over);
    expect(strips()).toEqual([['Search in Google', 'Copy', 'Quote'], ['Search in Google', 'Copy', 'Quote']]);
    phoneMedia();
    try {
      show([peer, room], over);
      expect(strips()).toHaveLength(2);
      expect(root.querySelector('.msg-actions')).toBeNull();
    } finally {
      vi.unstubAllGlobals();
    }
  });

  it('are Copy alone where the chat has no input and no search', () => {
    show([user(1, 'Run the tests'), agent(2, 'All tests pass.', 1, true)]);
    expect(labels(root.querySelector('.msg-agent .msg-buttons')!)).toEqual(['Copy']);
    expect(labels(root.querySelector('.msg-user .msg-buttons')!)).toEqual(['Copy']);
  });

  it("are always shown, in the line's flow, small enough for the time's line, a phone's no taller than a text line", () => {
    const r = rule('.msg-buttons');
    expect(r).toMatch(/display: inline-flex;/);
    expect(r).not.toMatch(/position: absolute;/);
    expect(r).not.toMatch(/opacity: 0;/);
    expect(r).not.toMatch(/background/);
    expect(CSS).not.toMatch(/msg-bubble-acts/);
    // The time's float with its buttons (24 px on a phone) stays within the first text line, as
    // WebKit rounds it down to whole pixels: the second line beside it would be shortened too,
    // and a long word there split.
    expect(rule('.msg-time')).toMatch(/height: calc\(var\(--feed-line\) - 1px\);/);
    // The buttons' icons are not turned with the chevron of an open reasoning.
    expect(CSS).toMatch(/\.thinking\[open\] summary > \.icon \{/);
    expect(CSS).not.toMatch(/\.thinking\[open\] summary \.icon/);
    expect(rule('.chat:not(.chat-phone) .msg-buttons .btn-copy-idle')).toMatch(/height: 18px;/);
    expect(rule('.msg-agent > .meta .msg-buttons,\n.thinking summary .msg-buttons')).toMatch(/margin-left: auto;/);
    expect(rule('.btn-msg-search-idle')).toMatch(/visibility: hidden;/);
  });

  it('leave no hover rules, no phone row, no margin column and no panel background behind', () => {
    expect(CSS).not.toMatch(/msg-actions/);
    expect(CSS).not.toMatch(/msg-time-actions/);
    expect(CSS).not.toMatch(/:hover \.msg-buttons/);
    expect(CSS).not.toMatch(/--msg-bg/);
    expect(CSS).not.toMatch(/\.meta \.btn-copy/);
    expect(CSS).not.toMatch(/\.btn-quote \{/);
    expect(rule('.msg-bubble > .msg-time')).not.toMatch(/position: relative;/);
    expect(CSS).not.toMatch(/\.msg-agent > \.msg-time \{/);
  });

  it("on a phone are at the end of the agent message's usage line, and not in its time line", () => {
    phoneMedia();
    try {
      show([user(1, 'Run the tests'), agent(2, 'All tests pass.', 1, true), usage(3)], { quote: vi.fn(), ...search });
      expect(root.querySelector('.msg-agent .msg-time .msg-buttons')).toBeNull();
      const inUsage = root.querySelector('.msg-agent > .meta')!;
      expect(labels(inUsage.querySelector('.msg-buttons')!)).toEqual(['Search in Google', 'Copy', 'Quote']);
      expect(inUsage.lastElementChild!.className).toBe('msg-buttons');
      expect(root.querySelector('.msg-agent > .msg-actions')).toBeNull();
      // The user's message keeps them by its time.
      expect(root.querySelector('.msg-user .msg-time .msg-buttons')).not.toBeNull();
    } finally {
      vi.unstubAllGlobals();
    }
  });

  it('on a phone are by the time of a message without a usage line, and of the user\'s and the answer', () => {
    phoneMedia();
    try {
      show([user(1, 'Run the tests'), agent(2, 'All tests pass.', 1, true), answer(3)], { fork: vi.fn(), quote: vi.fn(), ...search });
      for (const sel of ['.msg-user:not(.msg-answer) .msg-bubble', '.msg-agent', '.msg-answer .msg-bubble'])
        expect(root.querySelector(`${sel} > .msg-time .msg-buttons`), sel).not.toBeNull();
      expect(root.querySelector('.msg-actions')).toBeNull();
    } finally {
      vi.unstubAllGlobals();
    }
  });

  it('add to quick messages where fork is: not on a message waiting for delivery or not delivered', () => {
    const quickMessages = { has: () => false, add: vi.fn(async () => {}) };
    const labels = () => [...root.querySelectorAll('.msg-user .msg-buttons button')].map((b) => b.getAttribute('aria-label'));
    show([user(1, 'go')], { fork: vi.fn(), quickMessages });
    expect(labels()).toEqual(['Copy', 'Add to quick messages', 'Fork from here']);
    for (const delivery of ['pending', 'failed']) {
      show([user(1, 'go', (e) => (e.delivery = delivery))], { fork: vi.fn(), quickMessages });
      expect(labels()).toEqual(['Copy']);
    }
  });

  it('take back and send now where the user\'s message waits for the agent, before fork and the others', () => {
    const withdraw = vi.fn(async () => {});
    const sendNow = vi.fn(async () => {});
    const labels = () => [...root.querySelectorAll('.msg-user .msg-buttons button')].map((b) => b.getAttribute('aria-label'));
    const pending = (patch: (e: any) => void = () => {}) =>
      user(1, 'go', (e) => {
        e.delivery = 'pending';
        e.payload.human = true;
        patch(e);
      });
    show([pending()], { withdraw, sendNow, fork: vi.fn() });
    expect(labels()).toEqual(['Copy', 'Take back', 'Send now']);
    root.querySelector<HTMLButtonElement>('.msg-user .btn-withdraw')!.click();
    expect(withdraw).toHaveBeenCalledWith(expect.objectContaining({ event_id: id(1) }));
    root.querySelector<HTMLButtonElement>('.msg-user .btn-send-now')!.click();
    expect(sendNow).toHaveBeenCalledWith(expect.objectContaining({ event_id: id(1) }));
    // Send now is offered only while a turn runs.
    show([pending()], { withdraw });
    expect(labels()).toEqual(['Copy', 'Take back']);
    // Not on a message the agent took, one that failed, one the user did not type (a schedule's
    // run, a note of the hub's own: none of them has `human`).
    for (const patch of [
      (e: any) => (e.delivery = 'delivered'),
      (e: any) => (e.delivery = 'failed'),
      (e: any) => delete e.payload.human,
      (e: any) => (e.payload.schedule_id = id(99)) && delete e.payload.human,
      (e: any) => (e.payload.resume = true) && delete e.payload.human,
    ]) {
      show([pending(patch)], { withdraw, sendNow });
      expect(labels()).toEqual(['Copy']);
    }
    // A slash command is taken back, but not sent now: the runtime runs it by itself.
    show([user(1, '  /compact now', (e) => ((e.delivery = 'pending'), (e.payload.human = true)))], { withdraw, sendNow });
    expect(labels()).toEqual(['Copy', 'Take back']);
    // No input to put it back into: no button.
    show([pending()], {});
    expect(labels()).toEqual(['Copy']);
  });

  it('a message withdrawn is no part of the feed, however it came', () => {
    const { timeline, draw } = show([user(1, 'go', (e) => (e.delivery = 'pending')), user(2, 'next')]);
    expect(root.querySelectorAll('.msg-user')).toHaveLength(2);
    timeline.applyDelivery({ session_id: 's1', event_id: id(1), seq: 1, delivery: 'withdrawn' });
    draw();
    expect([...root.querySelectorAll('.msg-user .msg-text')].map((t) => t.textContent)).toEqual(['next']);
    // The hub's copy of the event (a reload) with the delivery it has now.
    const fresh = new Timeline();
    fresh.apply(user(1, 'go', (e) => (e.delivery = 'withdrawn')));
    fresh.apply(user(2, 'next'));
    expect(fresh.events().map((e) => e.seq)).toEqual([2]);
  });

  it("keep the phone size: the smaller buttons are a computer's only", () => {
    // No rule makes them smaller than the 24 px `.btn-copy-idle` outside `.chat:not(.chat-phone)`.
    expect(rule('.btn-copy-idle')).toMatch(/width: 24px;/);
    const small = [...CSS.matchAll(/\n([^\n{}]*\.msg-buttons[^\n{}]*) \{([^}]*)\}/g)].filter(([, , d]) => /(width|height): (1\d|2[0-3])px/.test(d));
    expect(small.length).toBeGreaterThan(0);
    for (const [, sel] of small) expect(sel).toContain('.chat:not(.chat-phone)');
  });

  it('show the copy result as an icon in the same button, not a caption that widens the strip', async () => {
    const writeText = vi.fn(async (_: string) => {});
    vi.stubGlobal('isSecureContext', true);
    Object.defineProperty(window.navigator, 'clipboard', { value: { writeText }, configurable: true });
    show([agent(2, 'All tests pass.', 1, true)]);
    const btn = () => root.querySelector<HTMLButtonElement>('.msg-agent .msg-buttons .btn-copy')!;
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
    expect(rule('.msg-buttons .btn-copy.btn-copy-done')).toMatch(/color: var\(--online\);/);
    expect(rule('.msg-buttons .btn-copy.btn-copy-failed')).toMatch(/color: var\(--danger\);/);
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
  const notes = () => [...root.querySelectorAll('.msg-user')].map((m) => m.querySelector('.msg-note')?.getAttribute('title') ?? null);

  it('a message the hub keeps as joined has the mark: an icon right before its time, the words its tooltip', () => {
    show([user(1, 'Fix the search'), agent(2, 'Looking.', 1), user(3, 'and keep it fast', (e) => (e.payload.joined = true))]);
    expect(notes()).toEqual([null, 'Joined the turn']);
    // Inside the time's span, so it takes an icon's room of the first line, not a line or a
    // run of words of its own.
    const bubble = root.querySelectorAll('.msg-user')[1].querySelector('.msg-bubble')!;
    expect([...bubble.children].map((c) => c.className)).toEqual(['msg-time', 'text msg-text']);
    const time = bubble.firstElementChild!;
    const mark = time.querySelector('.msg-note')!;
    expect(mark.parentElement).toBe(time);
    expect(mark.nextElementSibling!.tagName).toBe('TIME');
    expect(mark.textContent).toBe('');
    expect(mark.querySelector('svg')).not.toBeNull();
    expect(mark.getAttribute('role')).toBe('img');
    expect(mark.getAttribute('aria-label')).toBe('Joined the turn');
    expect(time.textContent).toBe(formatTime(at(3)));
    expect(root.querySelectorAll('.msg-note')).toHaveLength(1);
  });

  it('styles.css: the mark stands in the line of the time, neither floated nor on a line of its own', () => {
    const r = rule('.msg-note');
    expect(r).toMatch(/display: inline-flex;/);
    expect(r).not.toMatch(/float/);
    expect(CSS).not.toMatch(/\.msg-note::after/);
    expect(CSS).not.toMatch(/\.msg-noted/);
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
    expect(notes()).toEqual([null, 'Joined the turn']);
  });

  it("the hub's copy of a message that joined meanwhile carries it (a frame lost with the socket)", () => {
    const { timeline, draw } = show([user(1, 'Fix the search'), agent(2, 'Looking.', 1), waiting(3, 'and keep it fast')]);
    const copy = user(3, 'and keep it fast', (e) => (e.payload.joined = true));
    timeline.refresh([copy]);
    draw();
    expect(notes()).toEqual([null, 'Joined the turn']);
    // A copy in the stream (the subscription again) does the same.
    const t2 = show([user(1, 'go'), waiting(2, 'and this')]);
    t2.timeline.apply(user(2, 'and this', (e) => (e.payload.joined = true)));
    t2.draw();
    expect(notes()).toEqual([null, 'Joined the turn']);
  });
});

describe('the one-column gutter', () => {
  it("an agent's post in its room has the agent text's dot, not the arrow of a message from another session", () => {
    const post = (s: number, from: string) =>
      ev('event.peer.message', (e) => ((e.seq = s), (e.ts = at(s)), (e.payload.from_session_id = from)));
    show([post(1, 'm1'), post(2, 'elsewhere')], { roomAgent: (sid) => (sid === 'm1' ? { index: 0 } : undefined) });
    const marks = [...root.querySelectorAll('.feed-block > .feed-mark')].map((m) => m.className.replace('feed-mark feed-mark-', ''));
    expect(marks).toEqual(['dot', 'relay']);
  });

  it('a mark for every item but the user plates and the service lines, which span the feed', () => {
    const seq = (s: number) => (e: any) => ((e.seq = s), (e.ts = at(s)));
    const agentCall = (s: number, call_id: string, args: object) =>
      ev('event.tool.call', (e) => (seq(s)(e), (e.event_id = id(s)), (e.payload = { call_id, tool: 'Agent', args_preview: JSON.stringify(args) })));
    const agentResult = (s: number, call_id: string, error: boolean, result_preview: string) =>
      ev('event.tool.result', (e) => (seq(s)(e), (e.event_id = id(s)), (e.payload = { call_id, error, result_preview, duration_ms: 5 })));
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
      // Subagents: done, failed, in the background, running.
      agentCall(11, 'a1', { description: 'Fix' }),
      agentResult(12, 'a1', false, 'Fixed'),
      agentCall(13, 'a2', { description: 'Fix' }),
      agentResult(14, 'a2', true, 'Crashed'),
      agentCall(15, 'a3', { description: 'Fix', run_in_background: true }),
      agentResult(16, 'a3', false, 'Async agent launched successfully.\nagentId: a3'),
      agentCall(17, 'a4', { description: 'Fix' }),
    ]);
    const blocks = [...root.querySelectorAll<HTMLElement>('.feed-block')].map((b) => {
      const m = [...b.children].find((c) => c.classList.contains('feed-mark'));
      return [b.classList.contains('feed-full'), m ? m.className.replace('feed-mark feed-mark-', '') : null];
    });
    // The question, pending while the turn runs, waits for a turn of its own at the bottom.
    expect(blocks).toEqual([
      [true, null],
      [false, 'fold'],
      [false, 'dot'],
      [false, 'ok'],
      [false, 'file'],
      [false, 'ask'],
      [true, null],
      [false, 'perm'],
      // A subagent's start and, for one that ran in the foreground, its end.
      [false, 'ok'],
      [false, 'ok'],
      [false, 'error'],
      [false, 'error'],
      [false, 'dot'],
      [false, 'running'],
      [false, 'relay'],
    ]);
    // A mark is decoration: the block says what it is in words.
    expect(root.querySelector('.feed-mark:not([aria-hidden=true])')).toBeNull();
  });

  it('the thinking being written has the mark of the thinking it becomes (SPEC §6)', () => {
    const { timeline, draw } = show([user(1, 'Run the tests')]);
    timeline.apply(ev('event.agent.delta', (e) => (e.payload = { text: 'Checking', thinking: true })));
    draw();
    const marks = () =>
      [...root.querySelectorAll<HTMLElement>('.feed-block')].map((b) => {
        const m = [...b.children].find((c) => c.classList.contains('feed-mark'));
        return m ? m.className.replace('feed-mark feed-mark-', '') : null;
      });
    expect(marks()).toEqual([null, 'fold']);
    timeline.apply(ev('event.agent.thinking', (e) => ((e.seq = 2), (e.payload.text = 'Checking.'))));
    draw();
    expect(marks()).toEqual([null, 'fold']);
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
