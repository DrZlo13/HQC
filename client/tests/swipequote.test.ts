// @vitest-environment happy-dom
// On a phone a message dragged to the left is quoted (src/ui/swipequote.ts). This checks which
// touches quote and what the block does while the finger is on it; the feel of it on a real
// screen is not checked here.

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { listenForSwipeQuote } from '../src/ui/swipequote';

let feed: HTMLElement;
let quoted: number[];
let stop: () => void;
let now = 0;

beforeEach(() => {
  document.body.innerHTML = `<div id="feed">
    <div class="feed-block feed-swipe" data-seq="7"><p id="text">A message.</p><pre id="code">code</pre><span class="feed-swipe-icon"></span></div>
    <div class="feed-block" data-seq="8"><p id="plain">Not swipeable.</p></div>
  </div>`;
  feed = document.getElementById('feed')!;
  quoted = [];
  now = 1000;
  vi.spyOn(performance, 'now').mockImplementation(() => now);
  stop = listenForSwipeQuote(feed, (seq) => quoted.push(seq));
});
afterEach(() => {
  stop();
  vi.restoreAllMocks();
});

const block = () => feed.querySelector<HTMLElement>('[data-seq="7"]')!;
/** The icon, which carries the variables. */
const icon = () => block().querySelector<HTMLElement>('.feed-swipe-icon')!;

/** A touch event on `target` with the fingers at `at` (none: all lifted). */
function touch(target: EventTarget, type: string, ...at: { x: number; y: number }[]) {
  const fingers = at.map((a) => ({ clientX: a.x, clientY: a.y }));
  const lift = type === 'touchend' || type === 'touchcancel';
  target.dispatchEvent(Object.assign(new Event(type, { bubbles: true }), { touches: lift ? [] : fingers, changedTouches: fingers }));
}

/** A finger down at (200, 50) on `target`, then moved to each of `moves`. */
function drag(target: EventTarget, ...moves: [number, number][]) {
  touch(target, 'touchstart', { x: 200, y: 50 });
  for (const [x, y] of moves) touch(target, 'touchmove', { x, y });
}

describe('a swipe to the left', () => {
  it('moves the block with the finger and quotes it on a lift past 60 px, once', () => {
    const text = document.getElementById('text')!;
    drag(text, [190, 52], [150, 55]);
    expect(block().classList.contains('is-swiping')).toBe(true);
    expect(block().classList.contains('is-armed')).toBe(false);
    expect(block().style.transform).toBe('translateX(-50px)');
    touch(text, 'touchmove', { x: 120, y: 60 });
    expect(icon().style.getPropertyValue('--swipe')).toBe('1');
    expect(block().classList.contains('is-armed')).toBe(true);
    // Past the threshold the block follows a quarter of the pull: 60 + 20 × 0.25.
    expect(icon().style.getPropertyValue('--swipe-x')).toBe('65');
    expect(block().style.transform).toBe('translateX(-65px)');
    expect(icon().style.getPropertyValue('--swipe-y')).toBe('60');
    touch(text, 'touchend');
    expect(quoted).toEqual([7]);
    expect(block().classList.contains('is-swiping')).toBe(false);
    expect(block().classList.contains('is-armed')).toBe(false);
    expect(block().style.transform).toBe('');
    expect(icon().style.getPropertyValue('--swipe-x')).toBe('');
    expect(icon().style.getPropertyValue('--swipe')).toBe('');
    expect(icon().style.getPropertyValue('--swipe-y')).toBe('');
  });

  it('quotes nothing on a lift before 60 px, and the block goes back', () => {
    const text = document.getElementById('text')!;
    drag(text, [160, 50]);
    expect(block().classList.contains('is-swiping')).toBe(true);
    touch(text, 'touchend');
    expect(quoted).toEqual([]);
    expect(block().style.transform).toBe('');
  });

  it('is no swipe when the finger first goes more up than left, even if it then goes far left', () => {
    const text = document.getElementById('text')!;
    drag(text, [198, 20], [100, 20]);
    expect(block().classList.contains('is-swiping')).toBe(false);
    touch(text, 'touchend');
    expect(quoted).toEqual([]);
  });

  it('is none to the right', () => {
    const text = document.getElementById('text')!;
    drag(text, [300, 50]);
    expect(block().classList.contains('is-swiping')).toBe(false);
    touch(text, 'touchend');
    expect(quoted).toEqual([]);
  });

  it('is cancelled by touchcancel', () => {
    const text = document.getElementById('text')!;
    drag(text, [100, 50]);
    touch(text, 'touchcancel');
    expect(quoted).toEqual([]);
    expect(block().style.transform).toBe('');
  });

  it('is cancelled by a second finger', () => {
    const text = document.getElementById('text')!;
    drag(text, [150, 50]);
    touch(text, 'touchmove', { x: 100, y: 50 }, { x: 300, y: 50 });
    expect(block().classList.contains('is-swiping')).toBe(false);
    expect(block().style.transform).toBe('');
    touch(text, 'touchmove', { x: 90, y: 50 });
    touch(text, 'touchend');
    expect(quoted).toEqual([]);
  });

  it('starts only on a block that is swipeable', () => {
    const plain = document.getElementById('plain')!;
    drag(plain, [100, 50]);
    touch(plain, 'touchend');
    expect(quoted).toEqual([]);
    expect(feed.querySelector('.is-swiping')).toBeNull();
  });

  it('starts nowhere on what scrolls sideways (a code block)', () => {
    const code = document.getElementById('code')!;
    Object.defineProperty(code, 'scrollWidth', { value: 500 });
    Object.defineProperty(code, 'clientWidth', { value: 300 });
    code.style.overflowX = 'auto';
    drag(code, [100, 50]);
    expect(block().classList.contains('is-swiping')).toBe(false);
    touch(code, 'touchend');
    expect(quoted).toEqual([]);
  });

  it('forgets a touch whose end never came when the next one begins', () => {
    const text = document.getElementById('text')!;
    drag(text, [150, 50]);
    touch(text, 'touchstart', { x: 200, y: 50 });
    expect(block().classList.contains('is-swiping')).toBe(false);
    expect(block().style.transform).toBe('');
  });

  it('quotes once and puts the block back when its node was removed before the lift', () => {
    const text = document.getElementById('text')!;
    drag(text, [100, 50]);
    expect(block().classList.contains('is-armed')).toBe(true);
    const b = block();
    // The touch goes on at the node it went down on, which no longer bubbles to the page.
    b.remove();
    touch(text, 'touchend');
    expect(quoted).toEqual([7]);
    expect(b.style.transform).toBe('');
    expect(b.classList.contains('is-swiping')).toBe(false);
  });

  it('is no swipe when the finger is held before it moves (a long press selects text)', () => {
    const text = document.getElementById('text')!;
    touch(text, 'touchstart', { x: 200, y: 50 });
    now += 600;
    touch(text, 'touchmove', { x: 100, y: 50 });
    expect(block().classList.contains('is-swiping')).toBe(false);
    touch(text, 'touchend');
    expect(quoted).toEqual([]);
  });

  it('is a swipe when the finger moves within the hold time', () => {
    const text = document.getElementById('text')!;
    touch(text, 'touchstart', { x: 200, y: 50 });
    now += 400;
    touch(text, 'touchmove', { x: 100, y: 50 });
    touch(text, 'touchend');
    expect(quoted).toEqual([7]);
  });

  it('puts the block back and quotes nothing when its finger is lifted while one outside the feed stays', () => {
    const text = document.getElementById('text')!;
    const outside = document.createElement('button');
    document.body.append(outside);
    drag(text, [100, 50]);
    // A thumb comes down by the input, outside the feed: its touchstart goes to its own node.
    outside.dispatchEvent(Object.assign(new Event('touchstart', { bubbles: true }), { touches: [{ clientX: 100, clientY: 50 }, { clientX: 300, clientY: 700 }], changedTouches: [{ clientX: 300, clientY: 700 }] }));
    // The swiping finger is lifted; the thumb stays.
    text.dispatchEvent(Object.assign(new Event('touchend', { bubbles: true }), { touches: [{ clientX: 300, clientY: 700 }], changedTouches: [{ clientX: 100, clientY: 50 }] }));
    expect(quoted).toEqual([]);
    expect(block().classList.contains('is-swiping')).toBe(false);
    expect(block().style.transform).toBe('');
    // The thumb's end goes to its own node and does nothing here.
    outside.dispatchEvent(Object.assign(new Event('touchend', { bubbles: true }), { touches: [], changedTouches: [{ clientX: 300, clientY: 700 }] }));
    expect(quoted).toEqual([]);
  });

  it('puts the block back even when the quote throws', () => {
    stop();
    stop = listenForSwipeQuote(feed, () => {
      throw new Error('no field');
    });
    const text = document.getElementById('text')!;
    drag(text, [100, 50]);
    expect(() => touch(text, 'touchend')).toThrow('no field');
    expect(block().style.transform).toBe('');
  });

  it('does not look at what scrolls sideways before the touch turns into a swipe', () => {
    const code = document.getElementById('code')!;
    const read = vi.fn(() => 500);
    Object.defineProperty(code, 'scrollWidth', { get: read });
    touch(code, 'touchstart', { x: 200, y: 50 });
    expect(read).not.toHaveBeenCalled();
  });

  it('stops listening when cleaned up', () => {
    stop();
    const text = document.getElementById('text')!;
    drag(text, [100, 50]);
    touch(text, 'touchend');
    expect(quoted).toEqual([]);
    expect(block().classList.contains('is-swiping')).toBe(false);
  });
});
