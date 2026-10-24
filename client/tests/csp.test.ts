// The hub serves the client with `Content-Security-Policy: default-src 'self'; ...` and no
// `script-src` exception (SPEC §20): every script must be a same-origin file, never inline.

import { existsSync, readdirSync, readFileSync } from 'node:fs';
import { join } from 'node:path';
import { expect, it } from 'vitest';

const ROOT = join(import.meta.dirname, '..');

function inlineScripts(html: string): string[] {
  return [...html.matchAll(/<script\b([^>]*)>([\s\S]*?)<\/script>/gi)]
    .filter(([, attrs, body]) => !/\bsrc=/.test(attrs) || body.trim() !== '')
    .map(([s]) => s);
}

it('index.html has no inline scripts, handlers or remote assets', () => {
  const html = readFileSync(join(ROOT, 'index.html'), 'utf8');
  expect(inlineScripts(html)).toEqual([]);
  expect(html).not.toMatch(/\son[a-z]+\s*=/i);
  expect(html).not.toMatch(/(src|href)="(https?:)?\/\//i);
});

// T26: Inter and JetBrains Mono come from the hub itself (`font-src 'self'`), not Google Fonts.
it('fonts are same-origin files that exist, with their licenses', () => {
  const css = readFileSync(join(ROOT, 'src/styles.css'), 'utf8');
  const urls = [...css.matchAll(/url\(([^)]+)\)/g)].map((m) => m[1].replace(/['"]/g, ''));
  expect(urls.length).toBeGreaterThan(0);
  expect(css).not.toMatch(/@import|googleapis|gstatic/);
  const fonts = join(ROOT, 'public/fonts');
  for (const u of urls) {
    expect(u, u).toMatch(/^\/fonts\/[\w.-]+\.woff2$/);
    expect(existsSync(join(fonts, u.slice('/fonts/'.length))), u).toBe(true);
  }
  const files = readdirSync(fonts);
  expect(files.filter((f) => f.endsWith('.woff2')).sort()).toEqual(
    [...new Set(urls.map((u) => u.slice('/fonts/'.length)))].sort(),
  );
  expect(files).toContain('OFL-Inter.txt');
  expect(files).toContain('OFL-JetBrainsMono.txt');
});

it('the build (if present) has no inline scripts', () => {
  const dist = join(ROOT, 'dist');
  if (!existsSync(join(dist, 'index.html'))) return;
  for (const f of readdirSync(dist).filter((f) => f.endsWith('.html'))) {
    expect(inlineScripts(readFileSync(join(dist, f), 'utf8')), f).toEqual([]);
  }
});
