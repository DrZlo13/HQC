import { describe, expect, it } from 'vitest';
import { eventPageOf } from '../src/api';
import { eventText, findQuoteSource, quoteKey, stripMarkdown } from '../src/ui/quotejump';

const ev = (seq: number, type: string, payload: object) =>
  eventPageOf(
    [{ session_id: 's', event_id: `0199f3a2-7c10-7000-8000-${String(seq).padStart(12, '0')}`, seq, ts: '2026-10-24T18:00:00Z', author: { kind: type === 'user.message' ? 'user' : 'agent', id: 's' }, type, payload }],
    0,
  ).events[0];
const none = { callFor: () => undefined };

describe('the key a quote is found by', () => {
  it('keeps letters and digits only, in lower case', () => {
    expect(quoteKey('Hello,\n\tWORLD — 42!')).toBe('helloworld42');
    expect(quoteKey('Привет, **Мир**: ёж')).toBe('приветмирёж');
  });

  it('is the same for the Markdown source and its rendering', () => {
    const raw = 'A **bold** and `code` and [link text](https://x.test/a_b) here\n\n1. first\n2) second\n\n```ts\nlet x = 1;\n```\n~~~\nplain\n~~~';
    const shown = 'A bold and code and link text here first second let x = 1; plain';
    expect(quoteKey(stripMarkdown(raw))).toBe(quoteKey(shown));
  });

  it('a fence line\'s info string and a link target do not count', () => {
    expect(quoteKey(stripMarkdown('```rust\nfn\n```'))).toBe('fn');
    expect(quoteKey(stripMarkdown('[a](https://b.c/d9)'))).toBe('a');
  });
});

describe('the source of a quote', () => {
  it('an own message counts without its quote lines', () => {
    const e = ev(1, 'user.message', { text: '> quoted words\n\nand mine', artifact_ids: [] });
    expect(quoteKey(eventText(e, none))).toBe('andmine');
  });

  it('a call is its string values, not its raw JSON', () => {
    const e = ev(1, 'tool.call', { call_id: 'c', tool: 'Bash', args_preview: '{"command":"echo a\\nb","timeout":5}' });
    expect(eventText(e, none)).toBe('echo a\nb');
  });

  it('is the newest event before the quoting message, and the first paragraph is the second try', () => {
    const events = [
      ev(1, 'agent.message', { text: 'Some words here.', final: true }),
      ev(2, 'agent.message', { text: 'Some words here. More.', final: true }),
      ev(3, 'agent.message', { text: 'Some words here.', final: true }),
    ];
    const keys = { full: quoteKey('Some words here'), first: undefined };
    expect(findQuoteSource(events, none, keys, 3)).toBe(2);
    expect(findQuoteSource(events, none, keys)).toBe(3);
    expect(findQuoteSource(events, none, { full: quoteKey('Some words here. Gone.'), first: quoteKey('Some words here.') }, 3)).toBe(2);
    expect(findQuoteSource(events, none, { full: 'nothing' })).toBeUndefined();
  });
});
