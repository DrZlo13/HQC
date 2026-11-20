// @vitest-environment happy-dom
// The context menu's second level (a model family's versions): on a computer a flyout beside its
// item, opened by hover or →, closed by ← or Esc alone and a moment after the mouse left; on a
// phone unfolded under the item by a chevron of its own. And what stands over and under the
// items (the mode pill's switch, the model pill's limits).

import { afterEach, describe, expect, it, vi } from 'vitest';
import { render } from 'preact';
import { useState } from 'preact/hooks';
import { act } from 'preact/test-utils';
import { ContextMenu, type MenuEntry } from '../src/ui/ContextMenu';

let root: HTMLElement;
afterEach(() => {
  if (root) render(null, root);
  root?.remove();
  vi.useRealTimers();
  vi.restoreAllMocks();
});

const picked: string[] = [];
afterEach(() => void (picked.length = 0));

/** Opus with two versions, Sonnet with one, then an item with none. */
const ITEMS: MenuEntry[] = [
  {
    label: 'Opus',
    cls: 'btn-opus',
    detail: '4.8',
    onSelect: () => picked.push('opus'),
    submenu: [
      { label: 'Opus 5.5', cls: 'btn-opus-55', onSelect: () => picked.push('claude-opus-5-5') },
      { label: 'Opus 4.8', cls: 'btn-opus-48', checked: true, onSelect: () => picked.push('claude-opus-4-8') },
    ],
  },
  { label: 'Sonnet', cls: 'btn-sonnet', onSelect: () => picked.push('sonnet'), submenu: [{ label: 'Sonnet 5', cls: 'btn-sonnet-5', onSelect: () => picked.push('claude-sonnet-5') }] },
  'sep',
  { label: 'Another model…', cls: 'btn-other', onSelect: () => picked.push('other') },
];

/** The menu until it closes, with `onClose` counting. */
function Host({ phone, onClose, top, bottom }: { phone?: boolean; onClose?: () => void; top?: preact.ComponentChildren; bottom?: preact.ComponentChildren }) {
  const [open, setOpen] = useState(true);
  return open ? (
    <ContextMenu
      x={100}
      y={50}
      label="Model"
      items={ITEMS}
      backdrop={phone}
      top={top}
      bottom={bottom}
      onClose={() => {
        onClose?.();
        setOpen(false);
      }}
    />
  ) : null;
}

async function mount(node: preact.ComponentChild) {
  root = document.createElement('div');
  document.body.appendChild(root);
  await act(async () => render(node, root));
}

const $ = <T extends HTMLElement = HTMLElement>(sel: string) => root.querySelector<T>(sel);
const flyout = () => $('.ctx-flyout');
const flyoutItems = () => [...root.querySelectorAll('.ctx-flyout .menu-item')].map((b) => b.textContent);
const fire = (el: Element, type: string) => act(async () => void el.dispatchEvent(new MouseEvent(type)));
const key = (k: string) =>
  act(async () => {
    (document.activeElement ?? document).dispatchEvent(new KeyboardEvent('keydown', { key: k, bubbles: true, cancelable: true }));
  });

describe('a submenu on a computer', () => {
  it('opens beside its item on hover, which marks it; a click on the item picks it, a click in the flyout picks the entry; both close the menu', async () => {
    const onClose = vi.fn();
    await mount(<Host onClose={onClose} />);
    const opus = $('.btn-opus')!;
    expect(opus.getAttribute('aria-haspopup')).toBe('menu');
    expect(opus.getAttribute('aria-expanded')).toBe('false');
    // The chevron at its end, the pick there after its label (not a line under it).
    expect(opus.querySelector('.menu-sub-arrow svg')).not.toBeNull();
    expect(opus.querySelector('.menu-detail')!.textContent).toBe('4.8');
    expect(opus.classList.contains('has-detail')).toBe(false);
    expect(flyout()).toBeNull();
    await fire(opus, 'mouseenter');
    expect(flyoutItems()).toEqual(['Opus 5.5', 'Opus 4.8']);
    expect(flyout()!.getAttribute('role')).toBe('menu');
    expect(flyout()!.getAttribute('aria-label')).toBe('Opus');
    expect(opus.getAttribute('aria-expanded')).toBe('true');
    expect($('.ctx-flyout .btn-opus-48')!.getAttribute('aria-checked')).toBe('true');
    // Another family's item moves it there at once.
    await fire($('.btn-sonnet')!, 'mouseenter');
    expect(flyoutItems()).toEqual(['Sonnet 5']);
    await act(async () => $('.ctx-flyout .btn-sonnet-5')!.click());
    expect(picked).toEqual(['claude-sonnet-5']);
    expect(onClose).toHaveBeenCalledTimes(1);
    expect($('.ctx-menu')).toBeNull();
    render(null, root);

    await mount(<Host />);
    await fire($('.btn-opus')!, 'mouseenter');
    await act(async () => $('.btn-opus')!.click());
    expect(picked).toEqual(['claude-sonnet-5', 'opus']);
    expect($('.ctx-menu')).toBeNull();
  });

  it('→ opens it with its first entry focused, ↑/↓ stay in it, ← and Esc close it alone with the focus back on its item; Esc again closes the menu', async () => {
    const onClose = vi.fn();
    await mount(<Host onClose={onClose} />);
    const opus = $('.btn-opus')!;
    // The first item has the focus as the menu opens.
    expect(document.activeElement).toBe(opus);
    await key('ArrowRight');
    expect(document.activeElement).toBe($('.ctx-flyout .btn-opus-55'));
    await key('ArrowDown');
    expect(document.activeElement).toBe($('.ctx-flyout .btn-opus-48'));
    // Round within the flyout, not on to Sonnet.
    await key('ArrowDown');
    expect(document.activeElement).toBe($('.ctx-flyout .btn-opus-55'));
    await key('ArrowLeft');
    expect(flyout()).toBeNull();
    expect(document.activeElement).toBe(opus);
    expect(onClose).not.toHaveBeenCalled();
    // The menu's own items, the flyout's left out.
    await key('ArrowDown');
    expect(document.activeElement).toBe($('.btn-sonnet'));
    await key('ArrowUp');
    await key('ArrowRight');
    expect(flyout()).not.toBeNull();
    await key('Escape');
    expect(flyout()).toBeNull();
    expect($('.ctx-menu')).not.toBeNull();
    expect(document.activeElement).toBe(opus);
    await key('Escape');
    expect(onClose).toHaveBeenCalledTimes(1);
    expect($('.ctx-menu')).toBeNull();
  });

  it('→ on an item whose flyout the mouse opened already moves the focus to its first entry', async () => {
    await mount(<Host />);
    const opus = $('.btn-opus')!;
    expect(document.activeElement).toBe(opus);
    await fire(opus, 'mouseenter');
    expect(flyout()).not.toBeNull();
    // The mouse does not take the focus.
    expect(document.activeElement).toBe(opus);
    await key('ArrowRight');
    expect(document.activeElement).toBe($('.ctx-flyout .btn-opus-55'));
    expect(flyoutItems()).toEqual(['Opus 5.5', 'Opus 4.8']);
  });

  it('stays while the mouse is on it or its item and goes 250 ms after it left both, or for an item without one', async () => {
    vi.useFakeTimers({ toFake: ['setTimeout', 'clearTimeout'] });
    await mount(<Host />);
    const opus = $('.btn-opus')!;
    const wait = (ms: number) => act(async () => void vi.advanceTimersByTime(ms));
    await fire(opus, 'mouseenter');
    // On the way from the item to the flyout.
    await fire(opus, 'mouseleave');
    await wait(200);
    expect(flyout()).not.toBeNull();
    await fire(flyout()!, 'mouseenter');
    await wait(1000);
    expect(flyout()).not.toBeNull();
    await fire(flyout()!, 'mouseleave');
    await wait(240);
    expect(flyout()).not.toBeNull();
    await wait(20);
    expect(flyout()).toBeNull();
    // The menu itself stays.
    expect($('.ctx-menu')).not.toBeNull();

    await fire(opus, 'mouseenter');
    await fire(opus, 'mouseleave');
    await fire($('.btn-other')!, 'mouseenter');
    await wait(260);
    expect(flyout()).toBeNull();
  });

  it('stands to the right of its item where it fits, else to the left; its top level with the item, kept in the window', async () => {
    // No layout in happy-dom: the item's and the flyout's boxes as a 1024×768 window has them.
    let itemBox = { left: 100, right: 320, top: 60, bottom: 94 };
    vi.spyOn(HTMLElement.prototype, 'getBoundingClientRect').mockImplementation(function (this: HTMLElement) {
      const r = this.classList.contains('ctx-flyout')
        ? { left: 0, top: 0, right: 150, bottom: 200, width: 150, height: 200 }
        : this.classList.contains('btn-opus')
          ? { ...itemBox, width: itemBox.right - itemBox.left, height: itemBox.bottom - itemBox.top }
          : { left: 0, top: 0, right: 0, bottom: 0, width: 0, height: 0 };
      return { ...r, x: r.left, y: r.top, toJSON: () => r } as DOMRect;
    });
    await mount(<Host />);
    await fire($('.btn-opus')!, 'mouseenter');
    expect([flyout()!.style.left, flyout()!.style.top]).toEqual(['320px', '54px']);
    render(null, root);

    // Near the right and the bottom edge: to the left, moved up to end 8 px over the bottom.
    itemBox = { left: 800, right: 1000, top: 700, bottom: 734 };
    await mount(<Host />);
    await fire($('.btn-opus')!, 'mouseenter');
    expect([flyout()!.style.left, flyout()!.style.top]).toEqual(['650px', '560px']);
  });
});

describe('a submenu on a phone', () => {
  it('the chevron beside the item unfolds it under the item, indented; a tap on the label picks the item', async () => {
    await mount(<Host phone />);
    const toggle = $('.menu-split .menu-sub-toggle')!;
    expect(toggle.getAttribute('aria-label')).toBe('Opus: more');
    expect(toggle.getAttribute('aria-expanded')).toBe('false');
    expect($('.menu-inline')).toBeNull();
    await act(async () => toggle.click());
    // No flyout: under the item, in the menu.
    expect(flyout()).toBeNull();
    expect(toggle.getAttribute('aria-expanded')).toBe('true');
    const inline = $('.menu-split.is-open .menu-inline')!;
    expect(inline.getAttribute('role')).toBe('group');
    expect([...inline.querySelectorAll('.menu-item')].map((b) => b.textContent)).toEqual(['Opus 5.5', 'Opus 4.8']);
    // A tap on the chevron again folds it.
    await act(async () => toggle.click());
    expect($('.menu-inline')).toBeNull();
    expect($('.ctx-menu')).not.toBeNull();
    // The label picks the family.
    await act(async () => $<HTMLButtonElement>('.menu-split-row .btn-opus')!.click());
    expect(picked).toEqual(['opus']);
    expect($('.ctx-menu')).toBeNull();
    render(null, root);

    await mount(<Host phone />);
    await act(async () => $('.menu-sub-toggle')!.click());
    await act(async () => $('.menu-inline .btn-opus-55')!.click());
    expect(picked).toEqual(['opus', 'claude-opus-5-5']);
  });
});

describe('over and under the items', () => {
  it('`top` stands over them, `bottom` under them', async () => {
    await mount(<Host top={<div class="switch">Ask | Auto</div>} bottom={<div class="limits">5h 40%</div>} />);
    const menu = $('.ctx-menu')!;
    expect(menu.firstElementChild!.matches('.menu-top')).toBe(true);
    expect(menu.querySelector('.menu-top .switch')!.textContent).toBe('Ask | Auto');
    expect(menu.lastElementChild!.matches('.menu-bottom')).toBe(true);
    expect(menu.querySelector('.menu-bottom .limits')!.textContent).toBe('5h 40%');
  });
});
