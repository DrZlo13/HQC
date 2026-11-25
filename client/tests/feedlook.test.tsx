// @vitest-environment happy-dom
// The feed's look (SPEC §6, the feed): a message's time at the right end of its first line,
// inside a bubble or a plate; its copy and fork buttons, always shown, at its
// bottom right: the end of its usage line, else of its text's last line; "joined the turn" on a message that went into the turn already running; the marks
// of the one-column gutter ("Settings" → "Feed" → "One column").

import { afterEach, describe, expect, it, vi } from 'vitest';
import { render } from 'preact';
import { act } from 'preact/test-utils';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { parseEvent, type HqcEvent } from '../src/proto';
import { Timeline } from '../src/timeline';
import { ChatItems, type ChatActions } from '../src/ui/cards';
import { useRef } from 'preact/hooks';
import { selectedText, useQuoteSelection } from '../src/ui/selection';
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

describe('the text of a clipped selection', () => {
  /** Draws `events` in a chat's feed, selects from `from` to `to(root)`: the text of the selection
   *  (selection.ts: what the Quote button over the composer and the right-click menu take). */
  async function quoted(events: HqcEvent[], from: (r: HTMLElement) => [Node, number], to: (r: HTMLElement) => [Node, number], over: Partial<ChatActions> = {}) {
    const timeline = new Timeline();
    for (const e of events) timeline.apply(e);
    const actions: ChatActions = { timeline, approve: vi.fn(async () => {}), answer: vi.fn(async () => {}), resend: vi.fn(), discard: vi.fn(), openArtifact: vi.fn(), ...over };
    root = document.createElement('div');
    document.body.appendChild(root);
    await act(() =>
      render(
        <section class="scope">
          <div class="feed">
            <ChatItems items={timeline.items()} actions={actions} />
          </div>
          <footer>After the feed</footer>
        </section>,
        root,
      ),
    );
    await act(async () => {
      const r = document.createRange();
      r.setStart(...from(root));
      r.setEnd(...to(root));
      document.getSelection()!.removeAllRanges();
      document.getSelection()!.addRange(r);
    });
    return selectedText(root.querySelector('.scope'));
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

  it("reads a quote block of the owner's message on lines of its own", async () => {
    const got = await quoted(
      [agent(1, 'Intro', 0, true), user(2, 'before\n\n> q one\n> q two\n\nafter')],
      (r) => [textOf(r, 'Intro'), 0],
      (r) => [textOf(r, '\nafter'), 6],
    );
    expect(got).toBe('Intro\n\nbefore\nq one\nq two\nafter');
  });

  it("reads a quote that starts the owner's message above its text, on a line of its own", async () => {
    const got = await quoted(
      [agent(1, 'Intro', 0, true), user(2, '> q one\n> q two\n\nafter text')],
      (r) => [textOf(r, 'Intro'), 0],
      (r) => [textOf(r, 'after text'), 10],
    );
    expect(got).toBe('Intro\n\nq one\nq two\n\nafter text');
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

  it('cuts a range that ends outside the feed it starts in at the feed\'s end', async () => {
    const got = await quoted(
      [agent(1, 'Para one.\n\nPara two.', 0, true)],
      (r) => [textOf(r, 'Para two.'), 0],
      (r) => [textOf(r, 'After the feed'), 5],
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
      (r) => [textOf(r, 'Question'), 8],
      (r) => [textOf(r, 'Opening statement.'), 7],
      { roomAgent: (sid) => (sid === id(77) ? { index: 0, name: 'Optimist', model: 'claude-sonnet-5-5' } : undefined) },
    );
    expect(got).toBe('Opening');
  });

  it('takes exactly the text of a selection that runs to the end of a message, the buttons at the end of its last line left out', async () => {
    const got = await quoted(
      [agent(1, 'Para one.\n\nPara two.', 0, true), user(2, 'next')],
      (r) => [textOf(r, 'Para two.'), 5],
      (r) => [r.querySelector('.msg-user')!, 0],
    );
    expect(got).toBe('two.');
    // Inside the last paragraph, up to the strip: the browser's own text (happy-dom joins the
    // paragraphs without a break).
    const inside = await quoted(
      [agent(1, 'Para one.\n\nPara two.', 0, true)],
      (r) => [textOf(r, 'Para one.'), 0],
      (r) => [r.querySelector('.msg-agent p:last-child')!, 1],
    );
    expect(inside).toBe('Para one.Para two.');
    // A list: the strip is in the last item.
    const list = await quoted(
      [agent(1, '- one\n- two', 0, true), user(2, 'next')],
      (r) => [textOf(r, 'one'), 0],
      (r) => [r.querySelector('.msg-user')!, 0],
    );
    expect(list).toBe('one\ntwo');
  });

  const thinking = (text: string) =>
    ev('event.agent.thinking', (e) => {
      e.seq = 1;
      e.event_id = id(1);
      e.ts = at(1);
      e.payload = { text };
    });

  it("takes the whole text of an open reasoning when the range runs out of it", async () => {
    const text = 'Idea one.\nIdea two.';
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

  it("copies a reasoning's whole text from its Copy, which does not fold it", async () => {
    const text = 'First idea. Second idea. Third idea.';
    const writeText = vi.fn(async (_: string) => {});
    vi.stubGlobal('isSecureContext', true);
    Object.defineProperty(window.navigator, 'clipboard', { value: { writeText }, configurable: true });
    const timeline = new Timeline();
    timeline.apply(thinking(text));
    const actions: ChatActions = { timeline, approve: vi.fn(async () => {}), answer: vi.fn(async () => {}), resend: vi.fn(), discard: vi.fn(), openArtifact: vi.fn() };
    root = document.createElement('div');
    document.body.appendChild(root);
    await act(() => render(<ChatItems items={timeline.items()} actions={actions} />, root));
    expect(root.querySelector('.thinking summary .msg-buttons')).not.toBeNull();
    // Part of the text selected: the whole text is copied.
    const r = document.createRange();
    r.setStart(textOf(root, text), 12);
    r.setEnd(textOf(root, text), 23);
    document.getSelection()!.removeAllRanges();
    document.getSelection()!.addRange(r);
    await act(async () => {
      root.querySelector<HTMLButtonElement>('.thinking .btn-copy')!.click();
      await new Promise((res) => setTimeout(res, 0));
    });
    expect(writeText).toHaveBeenLastCalledWith(text);
    expect(root.querySelector('details')!.open).toBe(false);
  });

  it("begins with the selected part of the line a folded reasoning shows when the range starts in it, and leaves the line out when it only passes through", async () => {
    const text = 'Idea one.\nIdea two.';
    // From the middle of the visible line (past its "Thinking · ") to the start of the next message.
    const head = await quoted(
      [thinking(text), user(2, 'next')],
      (r) => [r.querySelector('.thinking-peek')!.lastChild!, 5],
      (r) => [r.querySelector('.msg-user')!, 0],
    );
    expect(head).toBe('one. Idea two.');
    // On into the next message: the line's part, then the message's.
    const more = await quoted(
      [thinking(text), user(2, 'next')],
      (r) => [r.querySelector('.thinking-peek')!.lastChild!, 0],
      (r) => [textOf(r, 'next'), 2],
    );
    expect(more).toBe('Idea one. Idea two.\n\nne');
    // A range from before it to after it: the line is not in it.
    const through = await quoted(
      [
        agent(1, 'First.', 0),
        ev('event.agent.thinking', (e) => {
          e.seq = 2;
          e.event_id = id(2);
          e.ts = at(2);
          e.payload = { text };
        }),
        user(3, 'next'),
      ],
      (r) => [textOf(r, 'First.'), 0],
      (r) => [textOf(r, 'next'), 2],
    );
    expect(through).toBe('First.\n\nne');
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
      (r) => [textOf(r, 'Question'), 8],
      (r) => [textOf(r, 'Is the build green?'), 6],
    );
    expect(got).toBe('Is the');
  });
});

describe('the chat follows its selection for the Quote button', () => {
  it('renders when a quotable selection comes and goes, not on every step of a drag, and not for one outside a feed', async () => {
    let renders = 0;
    let shown = false;
    function Chat() {
      const ref = useRef<HTMLElement>(null);
      shown = useQuoteSelection(ref, true).shown;
      renders++;
      return (
        <section ref={ref}>
          <header>Title</header>
          <div class="feed">All tests pass.</div>
        </section>
      );
    }
    root = document.createElement('div');
    document.body.appendChild(root);
    await act(() => render(<Chat />, root));
    const select = (node: Node, from: number, to: number) =>
      act(async () => {
        const r = document.createRange();
        r.setStart(node, from);
        r.setEnd(node, to);
        document.getSelection()!.removeAllRanges();
        document.getSelection()!.addRange(r);
        document.dispatchEvent(new Event('selectionchange'));
      });
    const feed = root.querySelector('.feed')!.firstChild!;
    const before = renders;
    await select(feed, 0, 2);
    expect(shown).toBe(true);
    const shownAt = renders;
    expect(shownAt - before).toBeGreaterThan(0);
    // A drag grows the selection step by step.
    for (const to of [3, 4, 5, 6, 7, 8]) await select(feed, 0, to);
    expect(renders).toBe(shownAt);
    // One that starts in the header is none.
    await select(root.querySelector('header')!.firstChild!, 0, 3);
    expect(shown).toBe(false);
  });

  it('is off while disabled', async () => {
    let shown = true;
    function Chat() {
      const ref = useRef<HTMLElement>(null);
      shown = useQuoteSelection(ref, false).shown;
      return (
        <section ref={ref}>
          <div class="feed">All tests pass.</div>
        </section>
      );
    }
    root = document.createElement('div');
    document.body.appendChild(root);
    await act(() => render(<Chat />, root));
    await act(async () => {
      const r = document.createRange();
      r.selectNodeContents(root.querySelector('.feed')!);
      document.getSelection()!.removeAllRanges();
      document.getSelection()!.addRange(r);
      document.dispatchEvent(new Event('selectionchange'));
    });
    expect(shown).toBe(false);
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

  for (const phone of [false, true]) {
    const where = phone ? 'on a phone' : 'on a computer';
    const maybePhone = () => phone && phoneMedia();

    it(`are at the end of the text's last line of a message, not by its time, ${where}`, async () => {
      maybePhone();
      const fork = vi.fn();
      show([user(1, 'Run the tests'), agent(2, 'All tests pass.', 1, true), answer(3)], { fork });
      const mine = root.querySelector('.msg-user:not(.msg-answer) .msg-bubble > .msg-text')!.lastElementChild!;
      expect(mine.className).toBe('msg-buttons msg-tail');
      expect(labels(mine)).toEqual(['Copy', 'Fork from here']);
      const its = root.querySelector('.msg-agent .md > p')!.lastElementChild!;
      expect(its.className).toBe('msg-buttons msg-tail');
      expect(labels(its)).toEqual(['Copy']);
      expect(labels(root.querySelector('.msg-answer .msg-bubble > .msg-text')!.lastElementChild!)).toEqual(['Copy']);
      // None by the time, nor under the message.
      expect(root.querySelector('.msg-time .msg-buttons')).toBeNull();
      expect(root.querySelector('.msg-actions')).toBeNull();
      await act(async () => mine.querySelector<HTMLButtonElement>('.btn-fork')!.click());
      expect(fork).toHaveBeenCalledWith(expect.objectContaining({ seq: 1 }));
    });

    it(`are at the right end of the usage line of an agent message that has one, ${where}`, () => {
      maybePhone();
      show([user(1, 'Run the tests'), agent(2, 'All tests pass.', 1, true), usage(3)], {});
      const meta = root.querySelector('.msg-agent > .meta')!;
      expect(meta.lastElementChild!.className).toBe('msg-buttons');
      expect(labels(meta.lastElementChild!)).toEqual(['Copy']);
      // Not also at the end of the text, nor by the time.
      expect(root.querySelectorAll('.msg-agent .msg-buttons')).toHaveLength(1);
      expect(root.querySelector('.msg-agent .msg-time .msg-buttons')).toBeNull();
      expect(root.querySelector('.msg-agent .msg-tail')).toBeNull();
    });

    it(`are by the time of a bubble that has no text, ${where}`, () => {
      maybePhone();
      show([user(1, '', (e) => (e.payload.artifact_ids = [id(50)]))], { fork: vi.fn() });
      expect(root.querySelectorAll('.msg-buttons'), 'one strip, not wrapped twice').toHaveLength(1);
      const strip = root.querySelector('.msg-bubble > .msg-time > .msg-buttons')!;
      expect(strip.className).toBe('msg-buttons');
      expect(strip.nextElementSibling!.tagName).toBe('TIME');
      expect(root.querySelector('.msg-tail')).toBeNull();
    });

    it(`are at the end of the text of a message from another session and of a room post, none in their heads, ${where}`, () => {
      maybePhone();
      const peer = ev('event.peer.message', (e) => {
        e.seq = 1;
        e.event_id = id(1);
        e.ts = at(1);
        e.payload = { ...e.payload, text: 'Is the build green?' };
      });
      const room = ev('event.peer.message', (e) => {
        e.seq = 2;
        e.event_id = id(2);
        e.ts = at(2);
        e.payload = { ...e.payload, from_session_id: id(77), text: 'Opening.' };
      });
      show([peer, room], { roomAgent: (sid: string) => (sid === id(77) ? { index: 0, name: 'Optimist' } : undefined) });
      for (const sel of ['.relay .md > p', '.room-post .md > p']) {
        const tail = root.querySelector(sel)!.lastElementChild!;
        expect(tail.className, sel).toBe('msg-buttons msg-tail');
        expect(labels(tail), sel).toEqual(['Copy']);
      }
      expect(root.querySelector('.relay-head .msg-buttons, .room-post-head .msg-buttons')).toBeNull();
    });
  }

  it('are at the end of an interim agent message\'s last line, inside the last list item, or on a line after a code block or a table', () => {
    show([agent(1, 'Steps:\n\n- one\n- two', 0), agent(2, 'Run:\n\n```sh\nls\n```', 0), agent(3, 'Table:\n\n| a |\n|---|\n| 1 |', 0)], {});
    const [list, code, table] = [...root.querySelectorAll('.msg-agent')];
    expect(list.querySelector('.md > ul > li:last-child')!.lastElementChild!.lastElementChild!.className).toBe('msg-buttons msg-tail');
    expect(list.querySelectorAll('.msg-buttons')).toHaveLength(1);
    for (const m of [code, table]) {
      const line = m.querySelector('.md')!.lastElementChild!;
      expect(line.className).toBe('msg-tail-line');
      expect(line.firstElementChild!.className).toBe('msg-buttons msg-tail');
      expect(m.querySelectorAll('.msg-buttons')).toHaveLength(1);
    }
  });

  it("are in a reasoning's summary while it is folded and at the end of its text, the summary's hidden by the CSS when it is open", () => {
    show([
      ev('event.agent.thinking', (e) => {
        e.seq = 1;
        e.event_id = id(1);
        e.ts = at(1);
        e.payload = { text: 'Idea one.' };
      }),
    ], {});
    const summary = root.querySelector('.thinking > summary')!;
    expect(summary.lastElementChild!.className).toBe('msg-buttons');
    const text = root.querySelector('.thinking-text')!;
    expect(text.lastElementChild!.className).toBe('msg-buttons msg-tail');
    expect(labels(text.lastElementChild!)).toEqual(['Copy']);
  });

  it("stand on the last line of a reasoning or a user's text that ends in blank lines, not below them", async () => {
    const copied = vi.fn(async (_: string) => {});
    vi.stubGlobal('isSecureContext', true);
    Object.defineProperty(window.navigator, 'clipboard', { value: { writeText: copied }, configurable: true });
    show([
      user(1, 'Run the tests\n\n'),
      ev('event.agent.thinking', (e) => {
        e.seq = 2;
        e.event_id = id(2);
        e.ts = at(2);
        e.payload = { text: 'Idea one.\n\n' };
      }),
    ], {});
    for (const sel of ['.msg-text', '.thinking-text']) {
      const tail = root.querySelector(`${sel} > .msg-tail`)!;
      expect(tail.previousSibling!.textContent, sel).toMatch(/\S$/);
    }
    // Copy still gives the text as it came.
    await act(async () => {
      root.querySelector<HTMLButtonElement>('.msg-text button[aria-label="Copy"]')!.click();
      await new Promise((r) => setTimeout(r, 0));
    });
    expect(copied).toHaveBeenCalledWith('Run the tests\n\n');
  });

  it('do not take the focus from the input: a press on a button is cancelled', () => {
    show([user(1, 'Run the tests'), agent(2, 'All tests pass.', 1, true)], { fork: vi.fn(), quickMessages: { has: () => false, add: vi.fn(async () => {}) } });
    const buttons = [...root.querySelectorAll('.msg-buttons button')];
    expect(buttons.length).toBeGreaterThan(2);
    for (const b of buttons) expect(b.dispatchEvent(new PointerEvent('pointerdown', { bubbles: true, cancelable: true })), b.getAttribute('aria-label')!).toBe(false);
  });

  it('keep the Quote button over the input out of the layout: absolute in a positioned row, as tall as the buttons', () => {
    expect(rule('.btn-quote')).toMatch(/position: absolute;/);
    expect(rule('.composer-row')).toMatch(/position: relative;/);
    const wide = rule(':root.feed-column .composer:not(.composer-phone) .btn-quote');
    expect(wide).toMatch(/right: 32px;/);
    expect(wide).toMatch(/height: 28px;/);
  });

  it('are Copy alone on a message that has no fork or quick messages', () => {
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
    expect(rule('.msg-agent > .meta .msg-buttons,\n.relay > .meta .msg-buttons,\n.thinking summary .msg-buttons')).toMatch(/margin-left: auto;/);
  });

  it('styles.css floats the buttons at the end of the text to the right, a pixel less than a text line, in message roots that hold floats and text blocks that do not', () => {
    const tail = rule('.msg-tail');
    expect(tail).toMatch(/float: right;/);
    expect(tail).toMatch(/height: calc\(var\(--feed-line\) - 1px\);/);
    expect(tail).toMatch(/align-items: center;/);
    expect(tail).toMatch(/margin: 0 0 0 12px;/);
    // The roots hold the tail a full last line pushed down; the text blocks must not, or their
    // first line would not wrap around the time.
    expect(rule('.msg-agent,\n.msg-bubble,\n.thinking-text')).toMatch(/display: flow-root;/);
    expect(CSS.match(/display: flow-root;/g), 'no other rule makes a block a formatting context of its own').toHaveLength(1);
    // What follows a text starts under that tail; a bubble of attachments only is not moved.
    expect(rule('.msg-text ~ .attachments,\n.msg-agent > .md ~ .relay-files')).toMatch(/clear: right;/);
    expect(rule('.msg-tail-line')).toMatch(/justify-content: flex-end;/);
    // A reasoning's summary holds them while folded, its text when open.
    expect(rule('.thinking[open] summary .msg-buttons')).toMatch(/display: none;/);
  });

  it('leave no hover rules, no phone row, no margin column and no panel background behind', () => {
    expect(CSS).not.toMatch(/msg-actions/);
    expect(CSS).not.toMatch(/msg-time-actions/);
    expect(CSS).not.toMatch(/:hover \.msg-buttons/);
    expect(CSS).not.toMatch(/--msg-bg/);
    expect(CSS).not.toMatch(/\.meta \.btn-copy/);
    expect(rule('.msg-bubble > .msg-time')).not.toMatch(/position: relative;/);
    expect(CSS).not.toMatch(/\.msg-agent > \.msg-time \{/);
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

describe('FeedBlocks without `swipe` (ChatItems, a subagent\'s history)', () => {
  it('has no swipe to quote, whatever its actions offer', () => {
    show([user(1, 'Run the tests'), agent(2, 'All tests pass.', 1, true)], { quote: vi.fn() });
    expect(root.querySelector('.msg-agent')).not.toBeNull();
    expect(root.querySelector('.feed-swipe, .feed-swipe-icon')).toBeNull();
  });
});
