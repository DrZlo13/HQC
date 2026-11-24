// On an iPhone the first tap on a project after a scroll on a session row only hovered it: every
// touch has to reach the page (src/taps.ts). Whether iOS then clicks was checked in the iOS
// simulator; this checks the listener and that the client installs it.

import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { describe, expect, it, vi } from 'vitest';
import { listenForTaps } from '../src/taps';

describe('listenForTaps', () => {
  it('listens to every touch on the page, passively', () => {
    const add = vi.fn();
    listenForTaps({ addEventListener: add } as unknown as Document);
    expect(add).toHaveBeenCalledWith('touchstart', expect.any(Function), { passive: true });
  });

  it('is installed when the client starts', () => {
    // main.tsx renders the app on import: its text is what is checked.
    const main = readFileSync(join(import.meta.dirname, '../src/main.tsx'), 'utf8');
    expect(main).toMatch(/^listenForTaps\(\);$/m);
  });
});
