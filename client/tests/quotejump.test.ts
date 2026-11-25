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

  it('a link target is cut at the first `)` after `](`, and a `](` with none is left as it is', () => {
    expect(stripMarkdown('[a](https://b.c) and [d](https://e.f/g_h) end')).toBe('[a] and [d] end');
    expect(stripMarkdown('see [a](https://b.c and more')).toBe('see [a](https://b.c and more');
    expect(stripMarkdown('[a](x) then [b](y')).toBe('[a] then [b](y');
    // Linear: a long text of `](` with no `)` after them is no slower than a short one.
    const long = '](x'.repeat(50000);
    const at = performance.now();
    expect(stripMarkdown(long)).toBe(long);
    expect(performance.now() - at).toBeLessThan(500);
  });

  it('a line start scan does not run across the blank lines after it', () => {
    const long = `${' \n'.repeat(30000)}- a`;
    const at = performance.now();
    stripMarkdown(long);
    expect(performance.now() - at).toBeLessThan(500);
  });

  it('a fence line\'s info string and a link target do not count', () => {
    expect(quoteKey(stripMarkdown('```rust\nfn\n```'))).toBe('fn');
    expect(quoteKey(stripMarkdown('[a](https://b.c/d9)'))).toBe('a');
  });
});

describe('the source of a quote', () => {
  it('an own message counts without its quote lines', () => {
    const e = ev(1, 'user.message', { text: '> quoted words\n\nand mine', artifact_ids: [] });
    expect(quoteKey(eventText(e, none).text)).toBe('andmine');
  });

  it('a call is its string values, not its raw JSON', () => {
    const e = ev(1, 'tool.call', { call_id: 'c', tool: 'Bash', args_preview: '{"command":"echo a\\nb","timeout":5}' });
    expect(eventText(e, none).text).toBe('echo a\nb');
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

describe('what the feed shows of an event, and how', () => {
  const find = (events: ReturnType<typeof ev>[], quote: string) => findQuoteSource(events, none, { full: quoteKey(quote) });

  it('a thinking is text: its list numbers are in the quote; a message is Markdown: they are not', () => {
    const thinking = ev(1, 'agent.thinking', { text: '1. Read the file\n2. Edit it' });
    const message = ev(2, 'agent.message', { text: '1. Read the file\n2. Edit it', final: true });
    expect(find([thinking], '1. Read the file')).toBe(1);
    expect(find([thinking], 'Read the file')).toBe(1);
    expect(find([message], 'Read the file')).toBe(2);
    // The number does not stand in the rendered message.
    expect(find([message], '1. Read the file')).toBeUndefined();
  });

  it('the report of a subagent on its card, a request\'s command and a question\'s option are found', () => {
    const report = ev(1, 'system', { code: 'task_notification', text: 'The **report** body of the subagent' });
    const approval = ev(2, 'approval.request', { request_id: 'r', tool: 'Bash', description: 'Run a script', reason: 'An ask rule requires approval', args_preview: '{"command":"echo hello world"}', options: [] });
    const clarify = ev(3, 'clarify.request', {
      request_id: 'q',
      message: 'The server wants a choice',
      questions: [{ id: 'a', text: 'Which flavour?', header: 'Flavour', options: ['vanilla bean', 'dark chocolate'], option_descriptions: ['plain', 'the bitter one'], option_previews: ['', 'a dark mockup'] }],
    });
    const all = [report, approval, clarify];
    expect(find(all, 'report body of the subagent')).toBe(1);
    expect(find(all, 'echo hello world')).toBe(2);
    expect(find(all, 'Run a script')).toBe(2);
    expect(find(all, 'dark chocolate')).toBe(3);
    expect(find(all, 'the bitter one')).toBe(3);
    expect(find(all, 'Which flavour')).toBe(3);
    expect(find(all, 'Flavour')).toBe(3);
    expect(find(all, 'ask rule requires approval')).toBe(2);
    expect(find(all, 'server wants a choice')).toBe(3);
    expect(find(all, 'a dark mockup')).toBe(3);
  });

  it('a worker log line is text, not Markdown', () => {
    expect(eventText(ev(1, 'system', { code: 'worker_log', text: '1. a' }), none)).toEqual({ text: '1. a', markdown: false });
    expect(eventText(ev(1, 'system', { code: 'context_compacted', text: '1. a' }), none).markdown).toBe(true);
  });

  it('only what `Quote` writes is a quote line of the owner: `> text` and a bare `>`', () => {
    const e = ev(1, 'user.message', { text: '> quoted\n>\n>>> x\n  > y\nmine', artifact_ids: [] });
    expect(eventText(e, none).text).toBe('>>> x\n  > y\nmine');
  });
});
