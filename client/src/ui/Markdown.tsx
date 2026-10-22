// Markdown of message texts (T11): marked's lexer gives tokens, this file turns them into
// Preact elements. Nothing is ever parsed as HTML: raw HTML in the source is shown as
// text, links are made only for http, https and mailto, images are not loaded.

import { Lexer, getDefaults, type MarkedToken, type Token, type Tokens } from 'marked';
import type { ComponentChildren } from 'preact';
import { useEffect, useMemo, useState } from 'preact/hooks';
import { copyText } from '../clipboard';

/** GFM with single newlines as <br>, as chat messages are written. */
export function parseMarkdown(src: string): Token[] {
  return Lexer.lex(src, { ...getDefaults(), gfm: true, breaks: true });
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
  return tokens.map((t, i) => block(t as MarkedToken, i));
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
  return (tokens ?? []).map((t, i) => inlineToken(t as MarkedToken, i));
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
    case 'codespan':
      return <code key={key}>{t.text}</code>;
    case 'br':
      return <br key={key} />;
    case 'checkbox':
      return <input key={key} type="checkbox" class="md-checkbox" checked={t.checked} disabled />;
    case 'link': {
      const href = safeHref(t.href);
      if (!href) return t.raw;
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
          <span class="md-image-alt">{decodeEntities(t.text) || 'картинка'}</span>{' '}
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

function CodeBlock({ code, lang }: { code: string; lang: string }) {
  return (
    <div class="md-code">
      <div class="md-code-bar">
        <span class="md-code-lang">{lang}</span>
        <CopyButton text={code} />
      </div>
      <pre>
        <code>{code}</code>
      </pre>
    </div>
  );
}

/** Puts `text` into the clipboard as is and says so for a moment. */
export function CopyButton({ text, label = 'Копировать' }: { text: string; label?: string }) {
  const [state, setState] = useState<'idle' | 'done' | 'failed'>('idle');
  useEffect(() => {
    if (state === 'idle') return;
    const t = setTimeout(() => setState('idle'), 1500);
    return () => clearTimeout(t);
  }, [state]);
  return (
    <button
      type="button"
      class={`btn-copy ${state === 'idle' ? 'btn-copy-idle' : ''}`}
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
        'Скопировано'
      ) : state === 'failed' ? (
        'Не скопировалось'
      ) : (
        <svg viewBox="0 0 24 24" width="14" height="14" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
          <rect x="9" y="9" width="11" height="11" rx="2" />
          <path d="M5 15V5a2 2 0 0 1 2-2h10" />
        </svg>
      )}
    </button>
  );
}
