// Text selected in a chat: for the Quote button over the composer and the menu on a right click
// (SPEC §6).

import { useEffect, useMemo, useRef, useState } from 'preact/hooks';
import type { RefObject } from 'preact';

/** The root element of a message the chat shows: an agent's or the user's, one from another
 *  session or a reply to it, the agent's reasoning. */
const MESSAGE = '.msg-agent, .msg-user, .relay, .thinking';

/** What a selection that runs out of a message leaves out of the message's part (a selection
 *  inside the message is the browser's own text): its time, the head of a message from another
 *  session or of a room post, the peek of a reasoning, the caption bar of a code block, the meta
 *  line and the buttons. */
const LEFT_OUT = '.msg-time, .relay-head, .room-post-head, .side-question-head, .side-wait, .thinking-peek, .md-code-bar, .meta, .msg-buttons';
const STRUCTURE = new Set(['TABLE', 'THEAD', 'TBODY', 'TR', 'UL', 'OL']);
/** A line break around these. */
const BLOCKS = new Set(['P', 'H1', 'H2', 'H3', 'H4', 'H5', 'H6', 'PRE', 'BLOCKQUOTE', 'DIV', 'UL', 'OL', 'TABLE', 'HR']);

/** The text of a clipped range as it reads: a line break around a block, after a list item and
 *  a table row, a tab after a table cell, a break at a `<br>`; none of what `LEFT_OUT` names
 *  (`Range.toString` joins blocks without a break). */
function flat(n: Node): string {
  if (n.nodeType === Node.TEXT_NODE) {
    // The line breaks of the markup between the items of a table or a list are no text.
    const text = n.textContent ?? '';
    return !text.trim() && STRUCTURE.has(n.parentElement?.tagName ?? '') ? '' : text;
  }
  if (n instanceof Element) {
    if (n.matches(LEFT_OUT)) return '';
    if (n.tagName === 'BR') return '\n';
    // A folded block (a reasoning, a run of calls, a task's report) shows its summary line only:
    // the rest is in the DOM but not on the screen, so it is no text of the selection.
    if (n.tagName === 'DETAILS' && !n.hasAttribute('open')) {
      const summary = [...n.children].find((c) => c.tagName === 'SUMMARY');
      return summary ? flat(summary) : '';
    }
  }
  const inner = [...n.childNodes].map(flat).join('');
  if (!(n instanceof Element)) return inner;
  switch (n.tagName) {
    case 'TD':
    case 'TH':
      return `${inner}\t`;
    case 'LI':
    case 'TR':
      return `${inner.replace(/\t$/, '')}\n`;
    default:
      return BLOCKS.has(n.tagName) ? `\n${inner}\n` : inner;
  }
}

/** The message a node is in. */
const rootOf = (n: Node) => (n instanceof Element ? n : n.parentElement)?.closest(MESSAGE);

/** The feed a node is in: the chat's, or a subagent's history (a feed of its own). */
const feedOf = (n: Node) => (n instanceof Element ? n : n.parentElement)?.closest('.feed') ?? null;

/** The selection inside `scope` (the chat), or the range `from` (a copy of an earlier one), for
 *  the Quote button and the menu about the selection: inside one message, its text as the browser
 *  lays it out; across several, the range's text by lines, without what `LEFT_OUT` names (times,
 *  heads, meta lines, buttons). A range that ends outside the feed it starts in (a triple click
 *  on the last paragraph of the last message ends past the feed) is cut at that feed's end. A
 *  range that starts in the visible line of a folded reasoning begins with its selected part of
 *  that line (without "Thinking · "); one that only passes through a folded reasoning leaves its
 *  line out. Undefined when the selection is empty or not inside `scope`. */
export function selectedText(scope: HTMLElement | null, from?: Range): string | undefined {
  const sel = scope?.ownerDocument.getSelection();
  let range = from ?? (sel && !sel.isCollapsed && sel.rangeCount > 0 ? sel.getRangeAt(0) : null);
  if (!scope || !range || range.collapsed) return undefined;
  if (!scope.contains(range.commonAncestorContainer)) return undefined;
  const feed = feedOf(range.startContainer);
  if (feed && !feed.contains(range.endContainer)) {
    range = range.cloneRange();
    range.setEnd(feed, feed.childNodes.length);
  }
  const start = rootOf(range.startContainer);
  let text: string;
  if (start && start === rootOf(range.endContainer)) {
    // The live selection reads as the browser lays it out. A copy (the selection the press has
    // cleared) has no layout: its text by lines, as across messages; a range only in what
    // `LEFT_OUT` names (a folded reasoning's line, a relay's head) has its plain text.
    const live = sel && sel.rangeCount > 0 ? sel.getRangeAt(0) : null;
    const same =
      live &&
      !live.collapsed &&
      live.compareBoundaryPoints(Range.START_TO_START, range) === 0 &&
      live.compareBoundaryPoints(Range.END_TO_END, range) === 0;
    text = same ? sel!.toString() : flat(range.cloneContents()).replace(/\n{3,}/g, '\n\n').trim() || range.toString();
  } else {
    const rest = range.cloneRange();
    // A range that starts in the line a folded reasoning shows: that line's selected part comes
    // first (its hidden text is not on the screen), then the rest of the range.
    const peek = start instanceof HTMLDetailsElement && !start.open ? start.querySelector('.thinking-peek') : null;
    let head = '';
    if (peek && peek.contains(range.startContainer)) {
      const line = range.cloneRange();
      line.setEnd(peek, peek.childNodes.length);
      head = line.toString().replace(/^Thinking · /, '').trim();
      rest.setStartAfter(start!);
    }
    const tail = rest.collapsed ? '' : flat(rest.cloneContents()).replace(/\n{3,}/g, '\n\n').trim();
    text = head && tail ? `${head}\n\n${tail}` : head || tail;
  }
  return text.trim() || undefined;
}

/** The Quote button over the composer: whether it is shown, and what it quotes. */
export interface QuoteSelection {
  shown: boolean;
  /** The text a press took, else the last one seen. */
  text(): string | undefined;
  /** A press on the button begins: the text is taken now and kept, and the button stays, though
   *  the press clears the selection (a phone's tap does, before the click). */
  press(): void;
  release(): void;
}

/** The selection to quote: a copy of the range of the selection in `scope` when it is not
 *  collapsed, starts in a feed of `scope` and holds some text (the feed, or a subagent's history;
 *  a selection that starts outside a feed, the header or the composer, is not). Cheap, for every
 *  `selectionchange` of a drag: its text is read only by `selectedText`, when it is quoted. */
function quotable(scope: HTMLElement | null): Range | null {
  const sel = scope?.ownerDocument.getSelection();
  if (!scope || !sel || sel.isCollapsed || sel.rangeCount === 0) return null;
  const range = sel.getRangeAt(0);
  const feed = feedOf(range.startContainer);
  return feed && scope.contains(feed) && sel.toString().trim() ? range.cloneRange() : null;
}

/** The text of the selection in `scope` (the chat) that starts in a feed, as the Quote button
 *  takes it: what the keys for Quote and Search read (SPEC §6). */
export function feedSelectionText(scope: HTMLElement | null): string | undefined {
  const range = quotable(scope);
  return range ? selectedText(scope, range) : undefined;
}

/** Follows the selection in `scope` (the chat) while `enabled`; `shown` changes only when there
 *  is a quotable selection or is none, not on every `selectionchange` of a drag. */
export function useQuoteSelection(scope: RefObject<HTMLElement | null>, enabled: boolean): QuoteSelection {
  const [shown, setShown] = useState(false);
  // The range last seen: what is quoted when the press has cleared the selection (iOS does).
  const seen = useRef<Range | null>(null);
  // The text a press took, kept until it is let go.
  const taken = useRef<string | undefined>(undefined);
  const frozen = useRef(false);
  const api = useMemo(() => {
    const sync = () => {
      if (frozen.current) return;
      seen.current = quotable(scope.current);
      setShown(seen.current !== null);
    };
    // The live selection, else the last one seen.
    const read = () => {
      const range = quotable(scope.current) ?? seen.current;
      return range ? selectedText(scope.current, range) : undefined;
    };
    return {
      sync,
      text: () => (frozen.current ? taken.current : read()),
      press: () => {
        taken.current = read();
        frozen.current = true;
      },
      release: () => {
        frozen.current = false;
        taken.current = undefined;
        sync();
      },
    };
  }, [scope]);
  useEffect(() => {
    if (!enabled) {
      frozen.current = false;
      seen.current = null;
      setShown(false);
      return;
    }
    api.sync();
    const away = (e: Event) => {
      // A press elsewhere ends one on the button.
      if (!frozen.current || (e.target as Element | null)?.closest?.('.btn-quote')) return;
      api.release();
    };
    document.addEventListener('selectionchange', api.sync);
    document.addEventListener('pointerdown', away, true);
    // A press the browser took over (a swipe that began on the button) has no click.
    document.addEventListener('pointercancel', api.release, true);
    return () => {
      document.removeEventListener('selectionchange', api.sync);
      document.removeEventListener('pointerdown', away, true);
      document.removeEventListener('pointercancel', api.release, true);
    };
  }, [api, enabled]);
  return useMemo(() => ({ shown, text: api.text, press: api.press, release: api.release }), [shown, api]);
}
