// @vitest-environment happy-dom
// Markdown of message texts (T11): parser, rendering of every supported element, HTML and
// unsafe links shown as text, copying the source, the delta stream.

import { afterEach, describe, expect, it, vi } from 'vitest';
import { render } from 'preact';
import { act } from 'preact/test-utils';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { parseEvent, type HqcEvent } from '../src/proto';
import { Timeline } from '../src/timeline';
import { ChatItems, StreamingMessage, type ChatActions } from '../src/ui/cards';
import { Markdown, decodeEntities, parseMarkdown, safeHref } from '../src/ui/Markdown';
import { copyText } from '../src/clipboard';

const WIRE = join(import.meta.dirname, '../../crates/hqc-proto/examples/wire');
const ev = (name: string, patch: (e: any) => void = () => {}): HqcEvent => {
  const e = JSON.parse(readFileSync(join(WIRE, `${name}.json`), 'utf8'));
  patch(e);
  return parseEvent(e);
};

let root: HTMLElement | undefined;
afterEach(() => {
  if (root) render(null, root);
  root = undefined;
  document.body.innerHTML = '';
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

function md(text: string): HTMLElement {
  root = document.createElement('div');
  document.body.appendChild(root);
  act(() => render(<Markdown text={text} />, root!));
  return root.querySelector('.md')!;
}

function chat(events: HqcEvent[]): HTMLElement {
  const t = new Timeline();
  events.forEach((e, i) => t.apply({ ...e, seq: i + 1 } as HqcEvent));
  const actions: ChatActions = {
    timeline: t,
    approve: async () => {},
    answer: async () => {},
    resend: () => {},
    discard: () => {},
    openArtifact: () => {},
  };
  root = document.createElement('div');
  document.body.appendChild(root);
  act(() => render(<ChatItems items={t.items()} actions={actions} />, root!));
  return root;
}

const tags = (el: Element) => [...el.querySelectorAll('*')].map((e) => e.tagName.toLowerCase());

describe('parser', () => {
  it('gives GFM tokens with single newlines as breaks', () => {
    const types = parseMarkdown('# T\n\na\nb\n\n| x |\n|---|\n| 1 |\n\n- [ ] t').map((t) => t.type);
    expect(types.filter((t) => t !== 'space')).toEqual(['heading', 'paragraph', 'table', 'list']);
    const p = parseMarkdown('a\nb')[0] as { tokens: { type: string }[] };
    expect(p.tokens.map((t) => t.type)).toEqual(['text', 'br', 'text']);
  });

  it('accepts only absolute http, https and mailto URLs', () => {
    expect(safeHref('https://example.com/a?b=1')).toBe('https://example.com/a?b=1');
    expect(safeHref('http://example.com')).toBe('http://example.com/');
    expect(safeHref('mailto:a@example.com')).toBe('mailto:a@example.com');
    for (const bad of ['javascript:alert(1)', 'JaVaScRiPt:alert(1)', ' javascript:alert(1)', 'java\tscript:x',
      'data:text/html,<b>', 'vbscript:x', 'file:///etc/passwd', '/relative', '#top', '//example.com']) {
      expect(safeHref(bad), bad).toBeNull();
    }
  });

  it('decodes character references', () => {
    expect(decodeEntities('a &amp; b &lt;c&gt; &#65;&#x42; &bogus; &#0;')).toBe('a & b <c> AB &bogus; �');
  });
});

describe('elements', () => {
  it('headings', () => {
    const el = md('# One\n## Two\n### Three\n#### Four\n##### Five\n###### Six');
    expect(tags(el)).toEqual(['h1', 'h2', 'h3', 'h4', 'h5', 'h6']);
    expect(el.querySelector('h2')!.textContent).toBe('Two');
  });

  it('paragraphs, and line breaks inside a paragraph as <br>', () => {
    const el = md('first line\nsecond line\n\nnext paragraph');
    const ps = el.querySelectorAll('p');
    expect(ps.length).toBe(2);
    expect(ps[0].querySelectorAll('br').length).toBe(1);
    expect(ps[0].textContent).toBe('first linesecond line');
    expect(ps[1].textContent).toBe('next paragraph');
  });

  it('lists: unordered, ordered with start, nested', () => {
    const el = md('- a\n- b\n  - b1\n  - b2\n    1. deep\n\n3. three\n4. four');
    const ul = el.querySelector(':scope > ul')!;
    expect(ul.children.length).toBe(2);
    expect(ul.querySelector('li > ul')!.children.length).toBe(2);
    expect(ul.querySelector('li > ul li > ol')!.textContent).toBe('deep');
    const ol = el.querySelector(':scope > ol')!;
    expect(ol.getAttribute('start')).toBe('3');
    expect([...ol.children].map((li) => li.textContent)).toEqual(['three', 'four']);
  });

  it('task lists with checkboxes, also nested', () => {
    const el = md('- [x] done\n- [ ] todo\n  - [x] sub');
    const boxes = [...el.querySelectorAll<HTMLInputElement>('input[type=checkbox]')];
    expect(boxes.map((b) => b.checked)).toEqual([true, false, true]);
    expect(boxes.every((b) => b.disabled)).toBe(true);
    expect(el.querySelectorAll('li.md-task').length).toBe(3);
    expect(el.querySelector('li')!.textContent).toBe('done');
  });

  it('bold, italic, strikethrough and inline code', () => {
    const el = md('**bold** and *it* and _it2_ and ***both*** ~~gone~~ `a <b> & c`');
    expect(el.querySelector('strong')!.textContent).toBe('bold');
    expect([...el.querySelectorAll('em')].map((e) => e.textContent)).toContain('it2');
    expect(el.querySelector('del')!.textContent).toBe('gone');
    const code = el.querySelector('code')!;
    expect(code.textContent).toBe('a <b> & c');
    expect(code.children.length).toBe(0);
  });

  it('code blocks with the language and their own copy button', () => {
    const el = md('```rust title="x"\nfn main() {\n    println!("<hi>");\n}\n```\n\n```\nplain\n```');
    const blocks = el.querySelectorAll('.md-code');
    expect(blocks.length).toBe(2);
    expect(blocks[0].querySelector('.md-code-lang')!.textContent).toBe('rust');
    expect(blocks[0].querySelector('pre code')!.textContent).toBe('fn main() {\n    println!("<hi>");\n}');
    expect(blocks[0].querySelector('button.btn-copy')).not.toBeNull();
    expect(blocks[1].querySelector('.md-code-lang')!.textContent).toBe('');
  });

  it('links: http, https and mailto open in a new tab without opener', () => {
    const el = md('[site](https://example.com "T") <http://example.org> mail [me](mailto:a@example.com) www.example.net');
    const links = [...el.querySelectorAll('a')];
    expect(links.map((a) => a.getAttribute('href'))).toEqual([
      'https://example.com/',
      'http://example.org/',
      'mailto:a@example.com',
      'http://www.example.net/',
    ]);
    for (const a of links) {
      expect(a.getAttribute('target')).toBe('_blank');
      expect(a.getAttribute('rel')).toBe('noopener noreferrer');
    }
    expect(links[0].getAttribute('title')).toBe('T');
    expect(links[0].textContent).toBe('site');
  });

  it('javascript:, data: and relative links render as text', () => {
    const el = md('[click](javascript:alert(1)) [d](data:text/html,x) [r](/api/me) [JS](JAVASCRIPT:alert(2))');
    expect(el.querySelector('a')).toBeNull();
    expect(el.textContent).toContain('[click](javascript:alert(1))');
    expect(el.textContent).toContain('[JS](JAVASCRIPT:alert(2))');
    expect(el.textContent).toContain('[r](/api/me)');
  });

  it('blockquotes, also nested and with markdown inside', () => {
    const el = md('> quoted **text**\n>\n> > inner');
    const q = el.querySelector(':scope > blockquote')!;
    expect(q.querySelector('strong')!.textContent).toBe('text');
    expect(q.querySelector('blockquote')!.textContent).toBe('inner');
  });

  it('tables with alignment', () => {
    const el = md('| name | n | c |\n|:-----|--:|:-:|\n| a | 1 | x |\n| `b` | 22 | y |');
    const table = el.querySelector('.md-table > table')!;
    expect([...table.querySelectorAll('th')].map((th) => th.textContent)).toEqual(['name', 'n', 'c']);
    expect(table.querySelectorAll('tbody tr').length).toBe(2);
    expect(table.querySelector('tbody tr:nth-child(2) td code')!.textContent).toBe('b');
    expect([...table.querySelectorAll('th')].map((th) => th.className)).toEqual([
      'align-left',
      'align-right',
      'align-center',
    ]);
  });

  it('horizontal rules', () => {
    const el = md('above\n\n---\n\nbelow\n\n***');
    expect(el.querySelectorAll('hr').length).toBe(2);
  });

  it('images are not loaded: alt text and a link', () => {
    const el = md('![a diagram](https://example.com/d.png) ![](javascript:alert(1))');
    expect(el.querySelector('img')).toBeNull();
    const imgs = el.querySelectorAll('.md-image');
    expect(imgs[0].querySelector('.md-image-alt')!.textContent).toBe('a diagram');
    expect(imgs[0].querySelector('a')!.getAttribute('href')).toBe('https://example.com/d.png');
    expect(imgs[1].querySelector('a')).toBeNull();
    expect(imgs[1].textContent).toContain('javascript:alert(1)');
  });

  it('backslash escapes and character references', () => {
    const el = md('\\*not em\\* &amp; &lt;b&gt;');
    expect(el.querySelector('em')).toBeNull();
    expect(el.textContent).toBe('*not em* & <b>');
  });
});

describe('HTML is text', () => {
  it('inline HTML creates no elements', () => {
    const el = md('before <img src=x onerror=alert(1)> and <script>alert(1)</script> after <b>bold</b>');
    expect(el.querySelector('img, script, b')).toBeNull();
    expect(tags(el)).toEqual(['p']);
    expect(el.textContent).toBe('before <img src=x onerror=alert(1)> and <script>alert(1)</script> after <b>bold</b>');
  });

  it('block HTML creates no elements', () => {
    const src = '<script>alert(1)</script>\n\n<img src=x onerror=alert(1)>\n\n<div onclick="x()">\n*hi*\n</div>';
    const el = md(src);
    expect(el.querySelector('img, script, div[onclick]')).toBeNull();
    expect(el.textContent).toContain('<script>alert(1)</script>');
    expect(el.textContent).toContain('<img src=x onerror=alert(1)>');
    expect(el.textContent).toContain('<div onclick="x()">');
    for (const e of el.querySelectorAll('*')) {
      expect([...e.attributes].map((a) => a.name).filter((n) => n.startsWith('on'))).toEqual([]);
    }
  });

  it('HTML inside link text, table cells and code stays text', () => {
    const el = md('[<img src=x onerror=alert(1)>](https://e.com)\n\n| <script>x</script> |\n|---|\n| <b>c</b> |');
    expect(el.querySelector('img, script, b')).toBeNull();
    expect(el.querySelector('a')!.textContent).toBe('<img src=x onerror=alert(1)>');
  });
});

describe('messages', () => {
  const SRC = '## Plan\n\n- **one**\n- `two`\n\n```sh\nls -la\n```';

  it('agent, user, peer and system texts are markdown', () => {
    const root = chat([
      ev('event.user.message', (e) => (e.payload.text = 'please **run**')),
      ev('event.agent.message', (e) => ((e.payload.text = SRC), (e.payload.final = false))),
      ev('event.agent.message', (e) => (e.payload.text = SRC)),
      ev('event.peer.message', (e) => (e.payload.text = '_from_ a peer')),
      // `session_created` is the divider «Сессия начата …»; other codes show their text.
      ev('event.system', (e) => (e.payload = { text: 'worker `home-mac` is **offline**', code: 'agent_exited' })),
    ]);
    expect(root.querySelector('.msg-user strong')!.textContent).toBe('run');
    expect(root.querySelector('.msg-interim h2')!.textContent).toBe('Plan');
    expect(root.querySelector('.msg-final .md-code code')!.textContent).toBe('ls -la');
    expect(root.querySelector('.card-peer em')!.textContent).toBe('from');
    expect(root.querySelector('.line-system code')!.textContent).toBe('home-mac');
  });

  it('every message has a copy button that copies the source text', async () => {
    const writeText = vi.fn(async (_: string) => {});
    vi.stubGlobal('isSecureContext', true);
    Object.defineProperty(window.navigator, 'clipboard', { value: { writeText }, configurable: true });
    const root = chat([
      ev('event.user.message', (e) => (e.payload.text = 'user *md*')),
      ev('event.agent.message', (e) => (e.payload.text = SRC)),
      ev('event.peer.message', (e) => (e.payload.text = 'peer **md**')),
    ]);
    const copy = (sel: string) => root.querySelector<HTMLButtonElement>(`${sel} > .meta .btn-copy`)!;
    for (const [sel, text] of [
      ['.msg-user', 'user *md*'],
      ['.msg-agent', SRC],
      ['.card-peer', 'peer **md**'],
    ]) {
      await act(async () => copy(sel).click());
      expect(writeText).toHaveBeenLastCalledWith(text);
    }
    expect(copy('.msg-agent').textContent).toBe('Скопировано');
    // The code block's button copies only the block.
    await act(async () => root.querySelector<HTMLButtonElement>('.msg-agent .md-code .btn-copy')!.click());
    expect(writeText).toHaveBeenLastCalledWith('ls -la');
  });

  it('copy falls back to a hidden textarea without the Clipboard API', async () => {
    Object.defineProperty(window.navigator, 'clipboard', { value: undefined, configurable: true });
    let copied: string | null = null;
    const exec = vi.fn((cmd: string) => {
      const area = document.activeElement as HTMLTextAreaElement | null;
      copied = cmd === 'copy' ? (document.querySelector<HTMLTextAreaElement>('textarea.copy-buffer') ?? area)!.value : null;
      return true;
    });
    Object.defineProperty(document, 'execCommand', { value: exec, configurable: true });
    await copyText('# raw *source*', document);
    expect(exec).toHaveBeenCalledWith('copy');
    expect(copied).toBe('# raw *source*');
    expect(document.querySelector('textarea')).toBeNull();
  });

  it('copy also falls back when the Clipboard API refuses', async () => {
    Object.defineProperty(window.navigator, 'clipboard', {
      value: { writeText: async () => Promise.reject(new Error('NotAllowedError')) },
      configurable: true,
    });
    const exec = vi.fn(() => true);
    Object.defineProperty(document, 'execCommand', { value: exec, configurable: true });
    await copyText('x', document);
    expect(exec).toHaveBeenCalledWith('copy');
  });

  it('copy reports a failure when nothing works', async () => {
    Object.defineProperty(window.navigator, 'clipboard', { value: undefined, configurable: true });
    Object.defineProperty(document, 'execCommand', { value: () => false, configurable: true });
    await expect(copyText('x', document)).rejects.toThrow();
  });

  it('renders a long message from examples/wire', () => {
    const section = (i: number) =>
      `## Section ${i}\n\nText with **bold**, *em*, \`code\` and a [link](https://example.com/${i}).\n` +
      `Second line.\n\n- item\n  - nested [ ]\n- [x] task\n\n| a | b |\n|---|---|\n| ${i} | <b>${i}</b> |\n\n` +
      '```ts\nconst x = 1;\n```\n\n> quote\n\n---\n\n';
    const text = Array.from({ length: 500 }, (_, i) => section(i)).join('');
    expect(text.length).toBeGreaterThan(100_000);
    const root = chat([ev('event.agent.message'), ev('event.agent.message', (e) => (e.payload.text = text))]);
    const bubbles = root.querySelectorAll('.msg-agent');
    expect(bubbles[0].querySelector('.md')!.textContent).toBe('All tests pass.');
    const long = bubbles[1];
    expect(long.querySelectorAll('h2').length).toBe(500);
    expect(long.querySelectorAll('table').length).toBe(500);
    expect(long.querySelectorAll('.md-code').length).toBe(500);
    expect(long.querySelector('b')).toBeNull();
  });
});

describe('delta stream', () => {
  function stream(chunks: string[]) {
    const t = new Timeline();
    root = document.createElement('div');
    document.body.appendChild(root);
    const draw = () => act(() => render(t.streaming ? <StreamingMessage text={t.streaming} /> : null, root!));
    for (const c of chunks) {
      t.apply(ev('event.agent.delta', (e) => (e.payload.text = c)));
      draw();
    }
    return { t, root: root!, draw };
  }

  it('re-renders markdown as deltas arrive', () => {
    const { root } = stream(['# Ti', 'tle\n\n- one', '\n- two\n\n| a |\n|---|\n| 1 |']);
    expect(root.querySelector('h1')!.textContent).toBe('Title');
    expect(root.querySelectorAll('li').length).toBe(2);
    expect(root.querySelector('td')!.textContent).toBe('1');
  });

  it('an unclosed code block or emphasis at the end does not break', () => {
    const { root } = stream(['Text **bold', ' still\n\n```py\nprint(1)\n', 'x = [', '']);
    const md = root.querySelector('.msg-streaming .md')!;
    expect(md.querySelector('.md-code code')!.textContent).toBe('print(1)\nx = [');
    expect(md.querySelector('.md-code-lang')!.textContent).toBe('py');
    expect(md.querySelector('p')!.textContent).toBe('Text **bold still');
  });

  it('survives every prefix of a document', () => {
    const doc =
      '# H\n\n**b** _e_ `c` [l](https://e.com) ![i](https://e.com/i.png)\n\n> q\n\n1. a\n   - [x] b\n\n' +
      '| a | b |\n|:-|-:|\n| 1 | 2 |\n\n```js\nlet a = "<script>";\n```\n\n<div>html</div>\n\n---\n';
    for (let i = 1; i <= doc.length; i++) {
      expect(() => parseMarkdown(doc.slice(0, i))).not.toThrow();
    }
    const { root } = stream([...doc]);
    expect(root.querySelector('script, div:not([class])')).toBeNull();
    expect(root.querySelector('.md-code code')!.textContent).toBe('let a = "<script>";');
  });
});
