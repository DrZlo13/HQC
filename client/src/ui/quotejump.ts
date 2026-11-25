// A click on a quote in the owner's message takes the chat back to the text it quotes, as in
// Telegram: the source is looked for in the session's events (the feed renders only part of
// them), then in the DOM of its block.

import type { HqcEvent } from '../proto';
import type { Timeline } from '../timeline';
import { askResult, callArgs, hqcTool, peerArgs } from './feed';
import { LEFT_OUT } from './selection';

/** The keys a quote is looked for by: its whole text, and its first paragraph when it has more
 *  than one (a quote taken across several messages is not found whole). */
export interface QuoteKeys {
  full: string;
  first?: string;
}

/** A click on a quote: what it quotes and the event of the message that quotes (the source is
 *  before it; none for a message not stored yet). */
export interface QuoteClick {
  keys: QuoteKeys;
  before?: number;
}

/** The letters and digits of `text`, lowercased and NFKC-normalized, each with where in `text` it
 *  stands, so Markdown marks, line breaks, table tabs and list layout do not matter. Per code
 *  point, so a position maps back to the text. */
function* keyChars(text: string): Generator<{ ch: string; at: number; len: number }> {
  let at = 0;
  for (const c of text) {
    for (const ch of c.normalize('NFKC').toLowerCase()) if (/[\p{L}\p{N}]/u.test(ch)) yield { ch, at, len: c.length };
    at += c.length;
  }
}

/** `text` as the key a quote is compared by. */
export function quoteKey(text: string): string {
  let key = '';
  for (const { ch } of keyChars(text)) key += ch;
  return key;
}

/** The raw Markdown of a message without what its rendering does not show as text and the quote
 *  does not carry: the targets of links and images, the numbers of an ordered list, the fence
 *  lines (their info string with them). */
export function stripMarkdown(text: string): string {
  return text
    .replace(/\]\([^)]*\)/g, ']')
    .replace(/^\s*\d+[.)]\s+/gm, '')
    .replace(/^\s*(?:```|~~~).*$/gm, '');
}

/** The text of an event the feed shows (a quoted text may come from any of them). The owner's
 *  own message without its `>` lines: an earlier message that quoted the same text is not its
 *  source. */
export function eventText(e: HqcEvent, t: Pick<Timeline, 'callFor'>): string {
  switch (e.type) {
    case 'agent.message':
    case 'agent.thinking':
    case 'peer.message':
      return e.payload.text;
    case 'user.message':
      return e.payload.text
        .split('\n')
        .filter((l) => !/^\s*>/.test(l))
        .join('\n');
    case 'side.answer':
      return e.payload.text ?? '';
    case 'tool.call': {
      const { tool, args_preview } = e.payload;
      if (hqcTool(tool) !== undefined) return peerArgs(args_preview).text;
      // The values, not the raw JSON: its escapes (`\n`) would add letters.
      return Object.values(callArgs(args_preview))
        .filter((v): v is string => typeof v === 'string')
        .join('\n');
    }
    case 'tool.result': {
      const call = t.callFor(e.payload.call_id);
      if (call && hqcTool(call.payload.tool) === 'ask') return askResult(e).reply;
      return [e.payload.error_text, e.payload.result_preview].filter(Boolean).join('\n');
    }
    default:
      return '';
  }
}

/** The `seq` of the event a quote comes from: the newest before `before` whose text holds the
 *  whole quote, else the newest that holds its first paragraph. */
export function findQuoteSource(events: readonly HqcEvent[], t: Pick<Timeline, 'callFor'>, keys: QuoteKeys, before?: number): number | undefined {
  let firstHit: number | undefined;
  for (let i = events.length - 1; i >= 0; i--) {
    const e = events[i];
    if (before !== undefined && e.seq! >= before) continue;
    const key = quoteKey(stripMarkdown(eventText(e, t)));
    if (key.includes(keys.full)) return e.seq;
    if (keys.first && firstHit === undefined && key.includes(keys.first)) firstHit = e.seq;
  }
  return firstHit;
}

/** The `seq` of the oldest event after `after` whose text holds the quote (whole or its first
 *  paragraph): the source of a quote taken from an answer still being written, which is stored
 *  after the message that quotes it. */
export function findQuoteAfter(events: readonly HqcEvent[], t: Pick<Timeline, 'callFor'>, keys: QuoteKeys, after: number): number | undefined {
  for (const e of events) {
    if (e.seq === undefined || e.seq <= after) continue;
    const key = quoteKey(stripMarkdown(eventText(e, t)));
    if (key.includes(keys.full) || (keys.first !== undefined && key.includes(keys.first))) return e.seq;
  }
  return undefined;
}

/** The quote under a click on the chat's feed, or undefined when the click is not a jump: not in
 *  a quote of the owner's message in this feed, ending a selection, or on a quote with no
 *  letters. */
export function quoteClick(e: MouseEvent, feed: HTMLElement): QuoteClick | undefined {
  const target = e.target instanceof Element ? e.target : null;
  const quote = target?.closest('.msg-user blockquote.msg-quote');
  if (!target || !quote || quote.closest('.feed') !== feed) return undefined;
  if (feed.ownerDocument.getSelection()?.isCollapsed === false) return undefined;
  const full = quoteKey(quote.textContent ?? '');
  if (!full) return undefined;
  const paragraphs = (quote.textContent ?? '').split(/\n\s*\n/);
  const lead = paragraphs.length > 1 ? quoteKey(paragraphs[0]) : '';
  const seq = quote.closest<HTMLElement>('.feed-block[data-seq]')?.dataset.seq;
  return { keys: { full, first: lead && lead !== full ? lead : undefined }, before: seq === undefined ? undefined : Number(seq) };
}

/** The range of the quoted text in the DOM of `block`: its whole key, else its first
 *  paragraph's, over the text that is there (not what `LEFT_OUT` names). */
function quoteRange(block: HTMLElement, keys: QuoteKeys): Range | undefined {
  const chars: { node: Text; at: number; len: number }[] = [];
  let all = '';
  const walk = block.ownerDocument.createTreeWalker(block, NodeFilter.SHOW_ELEMENT | NodeFilter.SHOW_TEXT, {
    acceptNode: (n) => (n instanceof Element && n.matches(LEFT_OUT) ? NodeFilter.FILTER_REJECT : NodeFilter.FILTER_ACCEPT),
  });
  for (let n = walk.nextNode(); n; n = walk.nextNode()) {
    if (!(n instanceof Text)) continue;
    for (const { ch, at, len } of keyChars(n.data)) {
      all += ch;
      chars.push({ node: n, at, len });
    }
  }
  for (const key of [keys.full, keys.first]) {
    const i = key ? all.indexOf(key) : -1;
    if (i < 0) continue;
    const last = chars[i + key!.length - 1];
    const range = block.ownerDocument.createRange();
    range.setStart(chars[i].node, chars[i].at);
    range.setEnd(last.node, last.at + last.len);
    return range;
  }
  return undefined;
}

interface HighlightRegistry {
  set(name: string, h: unknown): void;
  delete(name: string): void;
}
/** As long as the `found` animation of a search hit (styles.css). */
const HIGHLIGHT_MS = 2400;
let fading: ReturnType<typeof setTimeout> | undefined;

/** Shows the quoted text in `block`: opens the folds it is in, centres it in `scroller` (top
 *  aligned when it is taller) and highlights exactly it for a moment. False when it cannot: the
 *  text is not in the DOM (a fold that does not render its body), or the browser has no custom
 *  highlights. */
export function showQuote(block: HTMLElement, scroller: HTMLElement, keys: QuoteKeys): boolean {
  const registry = (globalThis as { CSS?: { highlights?: HighlightRegistry } }).CSS?.highlights;
  const Highlight = (globalThis as { Highlight?: new (...ranges: Range[]) => unknown }).Highlight;
  if (!registry || !Highlight) return false;
  const range = quoteRange(block, keys);
  if (!range) return false;
  for (const end of [range.startContainer, range.endContainer])
    for (let d = end.parentElement?.closest('details'); d; d = d.parentElement?.closest('details')) d.open = true;
  const at = range.getBoundingClientRect();
  const view = scroller.getBoundingClientRect();
  scroller.scrollTop += at.top - view.top - (at.height < view.height ? (view.height - at.height) / 2 : 0);
  registry.set('quote-jump', new Highlight(range));
  clearTimeout(fading);
  fading = setTimeout(() => registry.delete('quote-jump'), HIGHLIGHT_MS);
  return true;
}
