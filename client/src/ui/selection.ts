// Text selected in a chat, for the buttons of the message that holds it (Copy, Quote, Search;
// SPEC §6).

import { createContext } from 'preact';
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

/** What a message's buttons ask of the chat's selection. */
export interface MessageSelection {
  /** The message that holds the selection: the one the selection starts in, else the one it
   *  ends in. */
  holder: Element | null;
  /** The selected text of the message `root`, trimmed; undefined when it holds none. */
  text(root: Element | null): string | undefined;
  /** A press on the buttons begins: the selection is kept as it is until `release`, though the
   *  press clears it (a phone's tap does, before the click). */
  press(): void;
  release(): void;
}

export const SelectionContext = createContext<MessageSelection>({
  holder: null,
  text: () => undefined,
  press: () => {},
  release: () => {},
});

interface Held {
  root: Element;
  text: string;
}

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

/** The selection as the message that holds it: the one that contains the range's start, else
 *  the one that contains its end. A range inside it is `sel.toString()` as the browser lays it
 *  out; one that leaves it is clipped to the message. */
function held(scope: HTMLElement | null): Held | null {
  const sel = scope?.ownerDocument.getSelection();
  if (!scope || !sel || sel.isCollapsed || sel.rangeCount === 0) return null;
  const range = sel.getRangeAt(0);
  const root = rootOf(range.startContainer) ?? rootOf(range.endContainer);
  if (!root || !scope.contains(root)) return null;
  const startsIn = root.contains(range.startContainer);
  const endsIn = root.contains(range.endContainer);
  let text: string;
  if (startsIn && endsIn) text = sel.toString();
  else {
    const clip = range.cloneRange();
    if (!startsIn) clip.setStart(root, 0);
    // A folded reasoning shows only its line: its hidden text is not part of the selection.
    const line = root instanceof HTMLDetailsElement && !root.open ? root.querySelector('.thinking-peek') : null;
    if (line) {
      const shown = root.ownerDocument.createRange();
      shown.selectNodeContents(line);
      if (!endsIn || clip.compareBoundaryPoints(Range.END_TO_END, shown) > 0) clip.setEnd(shown.endContainer, shown.endOffset);
      text = clip.toString().replace(/^Thinking · /, '');
    } else {
      if (!endsIn) clip.setEnd(root, root.childNodes.length);
      text = flat(clip.cloneContents()).replace(/\n{3,}/g, '\n\n');
    }
  }
  text = text.trim();
  return text ? { root, text } : null;
}

/** The whole selection inside `scope`, for a menu about the selection and not about one message:
 *  inside one message, that message's text as `held` gives it; across several, the range's text
 *  by lines, without what `LEFT_OUT` names (times, heads, meta lines, buttons). Undefined when
 *  the selection is empty or not inside `scope`. */
export function selectedText(scope: HTMLElement | null): string | undefined {
  const sel = scope?.ownerDocument.getSelection();
  if (!scope || !sel || sel.isCollapsed || sel.rangeCount === 0) return undefined;
  const range = sel.getRangeAt(0);
  if (!scope.contains(range.commonAncestorContainer)) return undefined;
  const start = rootOf(range.startContainer);
  if (start && start === rootOf(range.endContainer)) return held(scope)?.text;
  return flat(range.cloneContents()).replace(/\n{3,}/g, '\n\n').trim() || undefined;
}

/** Keeps the selection inside `scope` (the chat) and publishes which message holds it; the state
 *  changes only when that message does, not on every `selectionchange` of a drag. */
export function useChatSelection(scope: RefObject<HTMLElement | null>): MessageSelection {
  const [holder, setHolder] = useState<Element | null>(null);
  const current = useRef<Held | null>(null);
  const frozen = useRef<Held | null>(null);
  const api = useMemo(() => {
    const sync = () => {
      if (frozen.current) return;
      current.current = held(scope.current);
      setHolder(current.current?.root ?? null);
    };
    const text = (root: Element | null) => {
      const h = frozen.current ?? current.current;
      return h && h.root === root ? h.text : undefined;
    };
    return {
      sync,
      text,
      // Read anew while the press has not cleared it; else the last one seen (iOS clears it first).
      press: () => {
        frozen.current = null;
        const now = held(scope.current);
        if (now) {
          current.current = now;
          setHolder(now.root);
        }
        frozen.current = current.current;
      },
      release: () => {
        frozen.current = null;
        sync();
      },
    };
  }, [scope]);
  useEffect(() => {
    const away = (e: Event) => {
      // A press elsewhere ends one on the buttons.
      if (!frozen.current || (e.target as Element | null)?.closest?.('.msg-buttons')) return;
      api.release();
    };
    document.addEventListener('selectionchange', api.sync);
    document.addEventListener('pointerdown', away, true);
    // A press the browser took over (a swipe that began on the buttons) has no click.
    document.addEventListener('pointercancel', api.release, true);
    return () => {
      document.removeEventListener('selectionchange', api.sync);
      document.removeEventListener('pointerdown', away, true);
      document.removeEventListener('pointercancel', api.release, true);
    };
  }, [api]);
  return useMemo(() => ({ holder, text: api.text, press: api.press, release: api.release }), [holder, api]);
}

/** The first two sentences of `text` (a blank line between paragraphs is no sentence), with
 *  "…" when more follows: after a space when the head ends in a sentence mark. */
export function firstSentences(text: string): string {
  // A third sentence only says that more follows: the rest of a long text is not segmented.
  const sentences: Intl.SegmentData[] = [];
  for (const s of new Intl.Segmenter(undefined, { granularity: 'sentence' }).segment(text)) {
    if (!s.segment.trim()) continue;
    sentences.push(s);
    if (sentences.length === 3) break;
  }
  if (!sentences.length) return '';
  const last = sentences[Math.min(2, sentences.length) - 1];
  const head = text.slice(sentences[0].index, last.index + last.segment.length).trim();
  if (sentences.length <= 2) return head;
  return /[.!?…]$/.test(head) ? `${head} …` : `${head}…`;
}
