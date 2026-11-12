// HEIC decoded by libheif's pure-JavaScript build, for browsers that do not decode it.

import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { expect, it } from 'vitest';
import { decodeHeic } from '../src/heic-decode';

it('the first image of a HEIC file becomes RGBA pixels', async () => {
  // 64×48, red (200, 40, 40) in every pixel, saved by macOS `sips`.
  const { width, height, data } = await decodeHeic(readFileSync(join(import.meta.dirname, 'fixtures/red.heic')));
  expect([width, height, data.length]).toEqual([64, 48, 64 * 48 * 4]);
  // HEVC is lossy: within a few levels.
  const [r, g, b] = data;
  for (const [got, want] of [
    [r, 200],
    [g, 40],
    [b, 40],
  ]) {
    expect(Math.abs(got - want)).toBeLessThanOrEqual(4);
  }
});
