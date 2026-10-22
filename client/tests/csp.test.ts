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

it('the build (if present) has no inline scripts', () => {
  const dist = join(ROOT, 'dist');
  if (!existsSync(join(dist, 'index.html'))) return;
  for (const f of readdirSync(dist).filter((f) => f.endsWith('.html'))) {
    expect(inlineScripts(readFileSync(join(dist, f), 'utf8')), f).toEqual([]);
  }
});
