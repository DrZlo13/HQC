// Markdown of message texts (T11): marked's lexer gives tokens, this file turns them into
// Preact elements. Nothing is ever parsed as HTML: raw HTML in the source is shown as
// text, links are made only for http, https and mailto, images are not loaded.
// Paths of files in the working copy (a relative link, or inline code like
// `src/main.rs:42`) are buttons that copy the path (T16; opening the file comes later).
// Code blocks marked `diff` or `patch` colour added and removed lines.
// TeX between `$$…$$`, `\[…\]` (display) or `$…$`, `\(…\)` (inline) becomes MathML by
// Temml, loaded on the first formula; until then, and if it fails, the source is shown.

import { Marked, type MarkedToken, type Token, type TokenizerExtension, type Tokens } from 'marked';
import type { ComponentChildren } from 'preact';
import { useEffect, useMemo, useRef, useState } from 'preact/hooks';
import { copyText } from '../clipboard';
import { IconCopy } from './icons';

interface MathToken {
  type: 'math';
  raw: string;
  tex: string;
  display: boolean;
}

/** `$…$` stays on one line, opens on a non-space and closes on a non-space not followed by
 *  a letter or digit, so "$5 and $10" and "$PATH/$HOME" stay text. Inside, `\$` is a dollar. */
const MATH_RULES: [RegExp, boolean][] = [
  [/^\$\$([\s\S]+?)\$\$/, true],
  [/^\\\[([\s\S]+?)\\\]/, true],
  [/^\\\(([\s\S]+?)\\\)/, false],
  [/^\$(?=[^\s$])((?:\\.|[^\\\n$])*?[^\s\\$])\$(?!\w)/, false],
];

function mathToken(src: string): MathToken | undefined {
  for (const [re, display] of MATH_RULES) {
    const m = re.exec(src);
    if (m && m[1].trim()) return { type: 'math', raw: m[0], tex: m[1].trim(), display };
  }
  return undefined;
}

/** A formula on lines of its own; one inside a paragraph is found by `mathInline`. */
const mathBlock: TokenizerExtension = {
  name: 'math',
  level: 'block',
  tokenizer(src) {
    if (!src.startsWith('$$') && !src.startsWith('\\[')) return undefined;
    const t = mathToken(src);
    const rest = t && /^[ \t]*(?:\n|$)/.exec(src.slice(t.raw.length));
    return t && rest ? { ...t, raw: t.raw + rest[0] } : undefined;
  },
};

const mathInline: TokenizerExtension = {
  name: 'math',
  level: 'inline',
  start: (src) => src.match(/\$|\\[([]/)?.index,
  tokenizer: (src) => mathToken(src),
};

const marked = new Marked({ gfm: true, breaks: true, extensions: [mathBlock, mathInline] });

/** GFM with single newlines as <br>, as chat messages are written, and formulas. */
export function parseMarkdown(src: string): Token[] {
  return marked.lexer(src);
}

function isMath(t: Token): t is Token & MathToken {
  return t.type === 'math';
}

export function Markdown({ text, class: cls }: { text: string; class?: string }) {
  const tokens = useMemo(() => {
    try {
      return parseMarkdown(text);
    } catch {
      return null;
    }
  }, [text]);
  const name = cls ? `md ${cls}` : 'md';
  // The lexer never failed on any input tried, but a chat must not break on one message.
  if (!tokens) return <div class={`${name} md-plain`}>{text}</div>;
  return <div class={name}>{blocks(tokens)}</div>;
}

const SAFE_PROTOCOLS = new Set(['http:', 'https:', 'mailto:']);

/** The URL to put into `href`, or null if it is not an absolute http, https or mailto URL. */
export function safeHref(href: string): string | null {
  try {
    const url = new URL(href);
    return SAFE_PROTOCOLS.has(url.protocol) ? url.href : null;
  } catch {
    return null;
  }
}

function blocks(tokens: Token[]): ComponentChildren[] {
  return tokens.map((t, i) => (isMath(t) ? <TeX key={i} token={t} /> : block(t as MarkedToken, i)));
}

function block(t: MarkedToken, key: number): ComponentChildren {
  switch (t.type) {
    case 'space':
    case 'def':
      return null;
    case 'heading': {
      const H = `h${Math.min(Math.max(t.depth, 1), 6)}` as 'h1';
      return <H key={key}>{inline(t.tokens)}</H>;
    }
    case 'paragraph':
      return <p key={key}>{inline(t.tokens)}</p>;
    case 'text':
      // Block text of a tight list item.
      return <span key={key}>{t.tokens ? inline(t.tokens) : decodeEntities(t.text)}</span>;
    case 'code':
      return <CodeBlock key={key} code={t.text} lang={t.lang?.trim().split(/\s+/)[0] ?? ''} />;
    case 'blockquote':
      return <blockquote key={key}>{blocks(t.tokens)}</blockquote>;
    case 'list': {
      const items = t.items.map((it, i) => (
        <li key={i} class={it.task ? 'md-task' : undefined}>
          {blocks(it.tokens)}
        </li>
      ));
      if (!t.ordered) return <ul key={key}>{items}</ul>;
      return (
        <ol key={key} start={typeof t.start === 'number' && t.start !== 1 ? t.start : undefined}>
          {items}
        </ol>
      );
    }
    case 'table':
      return (
        <div key={key} class="md-table">
          <table>
            <thead>
              <tr>
                {t.header.map((c, i) => (
                  <th key={i} class={alignClass(c.align)}>
                    {inline(c.tokens)}
                  </th>
                ))}
              </tr>
            </thead>
            <tbody>
              {t.rows.map((row, r) => (
                <tr key={r}>
                  {row.map((c, i) => (
                    <td key={i} class={alignClass(c.align)}>
                      {inline(c.tokens)}
                    </td>
                  ))}
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      );
    case 'hr':
      return <hr key={key} />;
    case 'html':
      // Raw HTML stays the text it was written as.
      return (
        <p key={key} class="md-html">
          {t.text}
        </p>
      );
    default:
      return inlineToken(t, key);
  }
}

function inline(tokens: Token[] | undefined): ComponentChildren[] {
  return (tokens ?? []).map((t, i) =>
    isMath(t) ? <TeX key={i} token={t} /> : inlineToken(t as MarkedToken, i),
  );
}

let temml: Promise<typeof import('temml').default> | undefined;

/** A formula: MathML once Temml has rendered it, else its source. Temml builds the nodes
 *  itself (no HTML is parsed) into an element Preact leaves empty; `trust` stays off, so
 *  `\href` and the like are errors. */
function TeX({ token }: { token: MathToken }) {
  const out = useRef<HTMLSpanElement>(null);
  const [state, setState] = useState<'source' | 'math'>('source');
  useEffect(() => {
    let live = true;
    setState('source');
    (temml ??= import('temml').then((m) => m.default)).then(
      (t) => {
        if (!live || !out.current) return;
        try {
          t.render(token.tex, out.current, { displayMode: token.display, throwOnError: true });
          setState('math');
        } catch {
          out.current.textContent = '';
        }
      },
      () => {},
    );
    return () => {
      live = false;
    };
  }, [token.tex, token.display]);
  const cls = token.display ? 'md-math md-math-display' : 'md-math';
  return (
    <span class={cls}>
      {state === 'source' && <code class="md-math-src">{token.raw.trim()}</code>}
      <span ref={out} />
    </span>
  );
}

function inlineToken(t: MarkedToken, key: number): ComponentChildren {
  switch (t.type) {
    case 'text':
      return t.tokens ? <span key={key}>{inline(t.tokens)}</span> : decodeEntities(t.text);
    case 'escape':
      return decodeEntities(t.text);
    case 'strong':
      return <strong key={key}>{inline(t.tokens)}</strong>;
    case 'em':
      return <em key={key}>{inline(t.tokens)}</em>;
    case 'del':
      return <del key={key}>{inline(t.tokens)}</del>;
    case 'codespan': {
      const path = filePath(t.text);
      if (path) {
        return (
          <FilePath key={key} path={path}>
            <code>{t.text}</code>
          </FilePath>
        );
      }
      return <code key={key}>{t.text}</code>;
    }
    case 'br':
      return <br key={key} />;
    case 'checkbox':
      return <input key={key} type="checkbox" class="md-checkbox" checked={t.checked} disabled />;
    case 'link': {
      const href = safeHref(t.href);
      if (!href) {
        const path = filePath(t.href, true);
        if (!path) return t.raw;
        return (
          <FilePath key={key} path={path}>
            {inline(t.tokens)}
          </FilePath>
        );
      }
      return (
        <a key={key} href={href} title={t.title ?? undefined} target="_blank" rel="noopener noreferrer">
          {inline(t.tokens)}
        </a>
      );
    }
    case 'image': {
      // Not loaded: the alt text and a link to the picture.
      const href = safeHref(t.href);
      return (
        <span key={key} class="md-image">
          <span class="md-image-alt">{decodeEntities(t.text) || 'image'}</span>{' '}
          {href ? (
            <a href={href} target="_blank" rel="noopener noreferrer">
              {t.href}
            </a>
          ) : (
            t.href
          )}
        </span>
      );
    }
    case 'html':
      return t.raw;
    default:
      // Block tokens never appear inline; anything unexpected is shown as its source.
      return (t as { raw?: string }).raw ?? '';
  }
}

function alignClass(align: Tokens.TableCell['align']): string | undefined {
  return align ? `align-${align}` : undefined;
}

const NAMED: Record<string, string> = { amp: '&', lt: '<', gt: '>', quot: '"', apos: "'", nbsp: ' ' };

/** Character references in text: numeric ones and the few named ones people type. */
export function decodeEntities(s: string): string {
  if (!s.includes('&')) return s;
  return s.replace(/&(?:#(\d{1,7})|#[xX]([0-9a-fA-F]{1,6})|([a-zA-Z]+));/g, (m, dec, hex, name) => {
    if (name) return NAMED[name] ?? m;
    const cp = dec ? Number(dec) : parseInt(hex, 16);
    const bad = cp === 0 || cp > 0x10ffff || (cp >= 0xd800 && cp <= 0xdfff);
    return String.fromCodePoint(bad ? 0xfffd : cp);
  });
}

/** A code block; `diff`/`patch` get coloured lines. `title` replaces the language label
 *  (an attached diff shows its file name), `extra` goes into the bar. */
export function CodeBlock({
  code,
  lang,
  title,
  extra,
}: {
  code: string;
  lang: string;
  title?: string;
  extra?: ComponentChildren;
}) {
  const diff = lang === 'diff' || lang === 'patch';
  return (
    <div class={diff ? 'md-code md-code-diff' : 'md-code'}>
      <div class="md-code-bar">
        <span class="md-code-lang">{title ?? lang}</span>
        {extra}
        <CopyButton text={code} withText />
      </div>
      <pre>
        <code>{diff ? diffLines(code) : code}</code>
      </pre>
    </div>
  );
}

/** Class of a line of a unified diff, if it is marked. */
export function diffClass(line: string): string | undefined {
  if (line.startsWith('+++') || line.startsWith('---')) return 'diff-file';
  if (line.startsWith('@@')) return 'diff-hunk';
  if (line.startsWith('+')) return 'diff-add';
  if (line.startsWith('-')) return 'diff-del';
  return undefined;
}

function diffLines(code: string): ComponentChildren[] {
  const lines = code.split('\n');
  return lines.map((l, i) => {
    const text = i < lines.length - 1 ? `${l}\n` : l;
    const cls = diffClass(l);
    return cls ? (
      <span key={i} class={cls}>
        {text}
      </span>
    ) : (
      text
    );
  });
}

/** Extensions that make a bare name (no slash) look like a file. */
const FILE_EXT =
  /\.(rs|ts|tsx|js|jsx|mjs|cjs|json|jsonl|toml|ya?ml|md|txt|py|go|rb|java|kt|swift|c|h|cc|cpp|hpp|cs|sh|zsh|bash|sql|html?|css|scss|vue|svelte|lock|xml|proto|ini|cfg|conf|env|gradle|dart|lua|php|exs?|zig|nix|tf|csv|log|diff|patch|png|jpe?g|gif|svg|webp|pdf)$/i;

/** The path as written if `s` looks like a file of the working copy: `src/main.rs`,
 *  `./a/b/`, `/abs/x.ts`, `main.rs:42`, `src/a.ts:3:7`, `docs/x.md#L10`; otherwise null.
 *  URLs, text with spaces and bare words with an unknown extension are not paths.
 *  `relative`: absolute paths are not accepted either (link targets: `/api/...` is the hub). */
export function filePath(s: string, relative = false): string | null {
  const t = s.trim();
  if (!t || t.length > 300 || /\s/.test(t)) return null;
  const m = /^(.+?)(?::\d+(?::\d+)?|#L\d+(?:-L?\d+)?)?$/.exec(t);
  const path = m ? m[1] : t;
  if (/^[a-z][a-z0-9+.-]*:/i.test(path) || path.startsWith('//')) return null;
  if (relative && (path.startsWith('/') || path.startsWith('~'))) return null;
  if (!/^[\w.@+~/-]+$/.test(path) || !/[a-z]/i.test(path)) return null;
  const parts = path.replace(/^\.{0,2}\//, '').replace(/\/$/, '').split('/');
  if (parts.some((p) => !p || p === '.')) return null;
  if (path.includes('/')) return t;
  return FILE_EXT.test(path) && !path.startsWith('.') ? t : null;
}

/** A path in a message: a click copies it and says so. */
function FilePath({ path, children }: { path: string; children: ComponentChildren }) {
  const [state, setState] = useState<'idle' | 'done' | 'failed'>('idle');
  useEffect(() => {
    if (state === 'idle') return;
    const t = setTimeout(() => setState('idle'), 1500);
    return () => clearTimeout(t);
  }, [state]);
  return (
    <button
      type="button"
      class="md-path"
      title={`Copy path ${path}`}
      onClick={(e) =>
        copyText(path, (e.currentTarget as HTMLElement).ownerDocument).then(
          () => setState('done'),
          () => setState('failed'),
        )
      }
    >
      {children}
      {state !== 'idle' && (
        <span class="md-path-note" role="status">
          {state === 'done' ? 'path copied' : 'copy failed'}
        </span>
      )}
    </button>
  );
}

/** Puts `text` into the clipboard as is and says so for a moment. `withText`: the caption
 *  next to the icon (code blocks), otherwise the icon alone. */
export function CopyButton({ text, label = 'Copy', withText }: { text: string; label?: string; withText?: boolean }) {
  const [state, setState] = useState<'idle' | 'done' | 'failed'>('idle');
  useEffect(() => {
    if (state === 'idle') return;
    const t = setTimeout(() => setState('idle'), 1500);
    return () => clearTimeout(t);
  }, [state]);
  return (
    <button
      type="button"
      class={`btn-copy ${state === 'idle' && !withText ? 'btn-copy-idle' : ''}`}
      aria-label={label}
      title={label}
      onClick={(e) =>
        copyText(text, (e.currentTarget as HTMLElement).ownerDocument).then(
          () => setState('done'),
          () => setState('failed'),
        )
      }
    >
      {state === 'done' ? (
        'Copied'
      ) : state === 'failed' ? (
        'Copy failed'
      ) : (
        <>
          <IconCopy />
          {withText && label}
        </>
      )}
    </button>
  );
}
