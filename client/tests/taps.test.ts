// @vitest-environment happy-dom
// On an iPhone a tap lost its click when a button or a link appeared anywhere in the page during
// it, and the first tap on a project after a scroll on a session row only hovered it (src/taps.ts).
// That iOS makes no click of its own after the press, and that a press survives the page's
// changes, was checked in the iOS simulator; this checks which touches press and which do not, and
// that the client installs the listeners.

import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { listenForTaps } from '../src/taps';

let now = 0;
let doc: Document;

beforeEach(() => {
  now = 1000;
  vi.spyOn(performance, 'now').mockImplementation(() => now);
  // A document of its own per test: the listeners and what they remember do not carry over.
  doc = document.implementation.createHTMLDocument('taps');
  listenForTaps(doc);
});
afterEach(() => vi.restoreAllMocks());

/** Lays `el` out 100 × 40 px at the top left. */
function box(el: Element) {
  el.getBoundingClientRect = () => ({ left: 0, top: 0, right: 100, bottom: 40, width: 100, height: 40, x: 0, y: 0, toJSON: () => ({}) });
}

/** `html`'s first element, 100 × 40 px at the top left, and how many clicks it got. */
function make(html: string): { el: HTMLElement; clicks: () => number } {
  doc.body.innerHTML = html;
  const el = doc.body.firstElementChild as HTMLElement;
  box(el);
  let n = 0;
  el.addEventListener('click', (e) => {
    n++;
    e.preventDefault();
  });
  return { el, clicks: () => n };
}

/** A touch event on `target` with one finger at `at` (none: no finger left on the screen). */
function touch(target: EventTarget, type: string, at?: { x: number; y: number }, cancelable = true): Event {
  const finger = at ? [{ clientX: at.x, clientY: at.y }] : [];
  const lift = type === 'touchend' || type === 'touchcancel';
  const e = Object.assign(new Event(type, { bubbles: true, cancelable }), {
    touches: lift ? [] : finger,
    changedTouches: finger,
  });
  target.dispatchEvent(e);
  return e;
}

/** A tap at (x, y) held `held` ms: its touchend. */
function tap(target: EventTarget, x = 50, y = 20, held = 80): Event {
  touch(target, 'touchstart', { x, y });
  now += held;
  return touch(target, 'touchend', { x, y });
}

/** Scroll events every 16 ms, `n` of them. */
function scrolls(n: number) {
  for (let i = 0; i < n; i++) {
    now += 16;
    doc.body.dispatchEvent(new Event('scroll'));
  }
}

describe('listenForTaps', () => {
  it('presses a button on the lift of a tap and cancels the click of the browser', () => {
    const { el, clicks } = make('<button>Send</button>');
    const end = tap(el);
    expect(clicks()).toBe(1);
    expect(end.defaultPrevented).toBe(true);
  });

  it('presses the button a tap went down on inside it (the icon of Send)', () => {
    const { el, clicks } = make('<button><svg><path d="M0 0"/></svg></button>');
    tap(el.querySelector('path')!);
    expect(clicks()).toBe(1);
  });

  it('presses a link, a summary and an element in the role of a button, a menu item or an option', () => {
    for (const html of [
      '<a href="#/s/1">row</a>',
      '<summary>Thinking</summary>',
      '<div role="button">x</div>',
      '<div role="menuitem">Copy</div>',
      '<li role="option">/compact</li>',
    ]) {
      const { el, clicks } = make(html);
      tap(el);
      expect(clicks(), html).toBe(1);
    }
  });

  it('leaves to the browser a tap on what is no button or link', () => {
    const { el, clicks } = make('<p>text</p>');
    const end = tap(el);
    expect(clicks()).toBe(0);
    expect(end.defaultPrevented).toBe(false);
  });

  it('presses nothing for a finger that moved over 10 px, held over 500 ms, or was lifted off', () => {
    const { el, clicks } = make('<button>Send</button>');
    touch(el, 'touchstart', { x: 50, y: 20 });
    touch(el, 'touchmove', { x: 50, y: 31 });
    expect(touch(el, 'touchend', { x: 50, y: 31 }).defaultPrevented).toBe(false);
    // Past the momentum of that move, which holds back a press of its own.
    now += 200;
    tap(el, 50, 20, 501);
    tap(el, 150, 20);
    expect(clicks()).toBe(0);
    // Within those bounds it is a tap.
    touch(el, 'touchstart', { x: 50, y: 20 });
    touch(el, 'touchmove', { x: 56, y: 28 });
    now += 500;
    touch(el, 'touchend', { x: 56, y: 28 });
    expect(clicks()).toBe(1);
  });

  it('takes the focus from a field being typed in, as the press of the browser does', () => {
    doc.body.innerHTML = '<input class="title"><a href="#/p/1">‹</a>';
    const field = doc.querySelector<HTMLElement>('.title')!;
    const back = doc.querySelector('a')!;
    box(back);
    Object.defineProperty(doc, 'activeElement', { configurable: true, get: () => field });
    const blur = vi.spyOn(field, 'blur');
    const order: string[] = [];
    blur.mockImplementation(() => order.push('blur'));
    back.addEventListener('click', (e) => {
      order.push('click');
      e.preventDefault();
    });
    tap(back);
    // Blurred first: a renamed title is saved before the chat goes.
    expect(order).toEqual(['blur', 'click']);
  });

  it('leaves the focus in the field when the press keeps it (its mousedown or pointerdown prevented)', () => {
    doc.body.innerHTML = '<form><textarea></textarea><button>Send</button></form><span class="mb"><button>Copy</button></span>';
    const field = doc.querySelector<HTMLElement>('textarea')!;
    const send = doc.querySelector<HTMLElement>('form button')!;
    const copy = doc.querySelector<HTMLElement>('.mb button')!;
    box(send);
    box(copy);
    Object.defineProperty(doc, 'activeElement', { configurable: true, get: () => field });
    const blur = vi.spyOn(field, 'blur');
    let clicks = 0;
    for (const b of [send, copy]) b.addEventListener('click', () => clicks++);
    // The composer's form keeps the focus on the mousedown of its buttons.
    doc.querySelector('form')!.addEventListener('mousedown', (e) => e.preventDefault());
    doc.querySelector('form')!.addEventListener('submit', (e) => e.preventDefault());
    tap(send);
    // A message's buttons keep it on their pointerdown, before the touchstart or after it.
    doc.querySelector('.mb')!.addEventListener('pointerdown', (e) => e.preventDefault());
    copy.dispatchEvent(new Event('pointerdown', { bubbles: true, cancelable: true }));
    tap(copy);
    touch(copy, 'touchstart', { x: 50, y: 20 });
    copy.dispatchEvent(new Event('pointerdown', { bubbles: true, cancelable: true }));
    now += 80;
    touch(copy, 'touchend', { x: 50, y: 20 });
    expect(clicks).toBe(3);
    expect(blur).not.toHaveBeenCalled();
  });

  it('presses no disabled button', () => {
    const { el, clicks } = make('<button disabled>Send</button>');
    expect(tap(el).defaultPrevented).toBe(false);
    expect(clicks()).toBe(0);
  });

  it('leaves to iOS a touchend it no longer lets cancel, and a touch of two fingers', () => {
    const { el, clicks } = make('<button>Send</button>');
    touch(el, 'touchstart', { x: 50, y: 20 });
    touch(el, 'touchend', { x: 50, y: 20 }, false);
    const two = Object.assign(new Event('touchstart', { bubbles: true }), {
      touches: [{ clientX: 50, clientY: 20 }, { clientX: 60, clientY: 20 }],
      changedTouches: [{ clientX: 60, clientY: 20 }],
    });
    el.dispatchEvent(two);
    touch(el, 'touchend', { x: 50, y: 20 });
    expect(clicks()).toBe(0);
  });

  it('presses nothing with a tap that stops the momentum of a swipe, and presses after it', () => {
    const { el, clicks } = make('<button>Fork</button>');
    // A swipe up the feed, and the scroll that coasts after the finger is lifted.
    touch(doc.body, 'touchstart', { x: 50, y: 300 });
    touch(doc.body, 'touchmove', { x: 50, y: 200 });
    touch(doc.body, 'touchend', { x: 50, y: 200 });
    scrolls(5);
    expect(tap(el).defaultPrevented).toBe(false);
    expect(clicks()).toBe(0);
    // The scroll has stopped: the next tap is a tap.
    now += 200;
    tap(el);
    expect(clicks()).toBe(1);
  });

  it('presses a button outside what coasts (Send under a feed still coasting after a swipe)', () => {
    doc.body.innerHTML = '<div class="feed"><button class="fork">Fork</button></div><button class="send">Send</button>';
    const feed = doc.querySelector('.feed')!;
    const fork = doc.querySelector<HTMLElement>('.fork')!;
    const send = doc.querySelector<HTMLElement>('.send')!;
    let pressed = '';
    for (const b of [fork, send]) {
      b.getBoundingClientRect = () => ({ left: 0, top: 0, right: 100, bottom: 40, width: 100, height: 40, x: 0, y: 0, toJSON: () => ({}) });
      b.addEventListener('click', () => (pressed += b.className + ' '));
    }
    touch(feed, 'touchstart', { x: 50, y: 300 });
    touch(feed, 'touchmove', { x: 50, y: 200 });
    touch(feed, 'touchend', { x: 50, y: 200 });
    for (let i = 0; i < 5; i++) {
      now += 16;
      feed.dispatchEvent(new Event('scroll'));
    }
    tap(send);
    // The feed still coasts: a tap on it stops it.
    tap(fork);
    expect(pressed).toBe('send ');
  });

  it('presses while the page scrolls by itself, with no swipe before (the feed following its bottom)', () => {
    const { el, clicks } = make('<button>Send</button>');
    scrolls(5);
    touch(el, 'touchstart', { x: 50, y: 20 });
    scrolls(3);
    touch(el, 'touchend', { x: 50, y: 20 });
    expect(clicks()).toBe(1);
  });

  it('listens to touches passively but for touchend, whose preventDefault cancels the click', () => {
    const add = vi.fn();
    listenForTaps({ addEventListener: add } as unknown as Document);
    expect(add).toHaveBeenCalledWith('touchstart', expect.any(Function), { passive: true });
    expect(add).toHaveBeenCalledWith('touchmove', expect.any(Function), { passive: true });
    expect(add.mock.calls.find((c) => c[0] === 'touchend')).toHaveLength(2);
  });

  it('is installed when the client starts', () => {
    // main.tsx renders the app on import: its text is what is checked.
    const main = readFileSync(join(import.meta.dirname, '../src/main.tsx'), 'utf8');
    expect(main).toMatch(/^listenForTaps\(\);$/m);
  });
});
