// One context menu for the session column, the project bar and the feed: a session row's right
// click and a phone's long press, the column header's order and project menus, a project's right
// click on the bar, a right click on text selected in the feed. It stands at the point it was
// opened from, kept inside the window.

import type { ComponentChildren } from 'preact';
import { useEffect, useLayoutEffect, useRef } from 'preact/hooks';
import { IconCheck } from './icons';

/** An item: a button (`onSelect`) or a link (`href`). */
export interface MenuItem {
  label: string;
  /** A second, muted part after the label, in the code font (a workspace's worker and path). */
  detail?: string;
  icon?: ComponentChildren;
  /** A class of its own (tests, styles). */
  cls?: string;
  danger?: boolean;
  /** One of a choice (State / Recent / Custom): a check mark while it is the one on. */
  checked?: boolean;
  disabled?: boolean;
  href?: string;
  /** The link opens in a new tab. */
  newTab?: boolean;
  /** The menu stays open: the item shows a next step in it. */
  keepOpen?: boolean;
  onSelect?: () => void;
}

export type MenuEntry = MenuItem | 'sep';

/** The space kept between a menu or a card and the window's edge, px. */
const MARGIN = 8;

/** Moves fixed box `box`, placed at (`x`, `y`), left and up as far as it overflows the window. */
export function fitInView(box: HTMLElement, x: number, y: number): void {
  const view = box.ownerDocument.defaultView;
  if (!view) return;
  const r = box.getBoundingClientRect();
  const dx = Math.max(0, r.right - (view.innerWidth - MARGIN));
  const dy = Math.max(0, r.bottom - (view.innerHeight - MARGIN));
  box.style.left = `${Math.max(MARGIN, x - dx)}px`;
  box.style.top = `${Math.max(MARGIN, y - dy)}px`;
}

/** The menu at (`x`, `y`) — with `above`, its bottom left corner there: `items`, or
 *  `children` in their place (a confirmation step). A press outside it (but on `anchor`, the
 *  button that toggles it) or Esc closes it; ↑/↓ move between its items. Closed, it gives the
 *  focus back to what had it before it opened (else to `anchor`). */
export function ContextMenu({
  x,
  y,
  above,
  label,
  heading,
  items = [],
  anchor,
  backdrop,
  onClose,
  children,
}: {
  x: number;
  y: number;
  /** Opens upwards from `y`: the chips over a new session's input. */
  above?: boolean;
  label: string;
  /** A line over the items saying what the menu is for (a project's tile has no name). */
  heading?: string;
  items?: MenuEntry[];
  anchor?: HTMLElement | null;
  /** A phone: a clear layer under the menu over the whole screen, so the tap that closes the
   *  menu does nothing else (opens no session, presses no button). */
  backdrop?: boolean;
  onClose: () => void;
  children?: ComponentChildren;
}) {
  const box = useRef<HTMLDivElement>(null);
  const scrim = useRef<HTMLDivElement>(null);
  // A press that started on the backdrop: only its click closes the menu, not the click that
  // ends the long press which opened it.
  const scrimDown = useRef(false);
  // Closed by a press away from the menu: the focus is left where that press puts it.
  const pressedAway = useRef(false);
  const close = useRef(onClose);
  close.current = onClose;
  const step = !!children;
  useLayoutEffect(() => {
    const el = box.current;
    if (!el) return;
    // Upwards: placed by the height it has now, then kept in the window as any.
    if (above) el.style.top = `${y - el.offsetHeight}px`;
    fitInView(el, x, above ? y - el.offsetHeight : y);
  }, [x, y, step]);
  // Where the focus was before the menu; it goes back there when the menu closes and the focus
  // was in the menu or nowhere (Esc, an item picked), not when a press away moved it.
  useEffect(() => {
    const doc = box.current?.ownerDocument;
    const before = doc?.activeElement as HTMLElement | null | undefined;
    return () => {
      // The menu closes on pointerdown, before the press moves the focus: restoring it here
      // would scroll the list back and lose the click on another row.
      if (!doc || pressedAway.current) return;
      const now = doc.activeElement;
      if (now && now !== doc.body && !box.current?.contains(now)) return;
      const to = before && before !== doc.body && before.isConnected && !box.current?.contains(before) ? before : anchor;
      to?.focus({ preventScroll: true });
    };
  }, []);
  // An item takes the focus (the one marked `data-focus`, else the first), again when a next
  // step replaces the items.
  useEffect(() => {
    const el = box.current;
    (el?.querySelector<HTMLElement>('[data-focus]') ?? el?.querySelector<HTMLElement>('.menu-item:not(:disabled)'))?.focus();
  }, [step]);
  /** Esc closes the menu, ↑/↓ move between its items. */
  const keys = (e: KeyboardEvent) => {
    const el = box.current;
    if (!el) return;
    if (e.key === 'Escape') {
      // Marked handled: a permission card in the open chat skips a prevented Esc.
      e.preventDefault();
      close.current();
      return;
    }
    if (e.key !== 'ArrowDown' && e.key !== 'ArrowUp') return;
    const list = [...el.querySelectorAll<HTMLElement>('.menu-item:not(:disabled)')];
    if (!list.length) return;
    e.preventDefault();
    const i = list.indexOf(el.ownerDocument.activeElement as HTMLElement);
    const next = e.key === 'ArrowDown' ? i + 1 : (i < 0 ? 0 : i) - 1 + list.length;
    list[next % list.length].focus();
  };
  useEffect(() => {
    const el = box.current;
    const doc = el?.ownerDocument;
    if (!el || !doc) return;
    const away = (e: Event) => {
      const t = e.target as Node;
      // The backdrop closes the menu by its own click.
      if (t === scrim.current) return;
      if (!el.contains(t) && !anchor?.contains(t)) {
        pressedAway.current = true;
        close.current();
      }
    };
    // The keys while the focus is outside the menu (inside it, the menu's own handler).
    doc.addEventListener('pointerdown', away);
    doc.addEventListener('keydown', keys);
    return () => {
      doc.removeEventListener('pointerdown', away);
      doc.removeEventListener('keydown', keys);
    };
  }, [anchor]);
  // Inside the menu its keys stay its own: a permission card in the open chat answers Enter
  // and Esc on the document (Esc would deny the request, Enter allow it and cancel the item's
  // click).
  const ownKeys = (e: KeyboardEvent) => {
    if (e.key !== 'Escape' && e.key !== 'Enter' && e.key !== 'ArrowDown' && e.key !== 'ArrowUp') return;
    e.stopPropagation();
    keys(e);
  };

  const pick = (it: MenuItem) => {
    if (!it.keepOpen) close.current();
    it.onSelect?.();
  };
  const entry = (it: MenuEntry, i: number) => {
    if (it === 'sep') return <div key={`sep${i}`} class="menu-sep" role="separator" />;
    const cls = `menu-item ${it.cls ?? ''} ${it.danger ? 'menu-danger' : ''} ${it.detail ? 'has-detail' : ''}`;
    const face = (
      <>
        {it.checked === undefined ? it.icon : <span class="menu-check">{it.checked && <IconCheck />}</span>}
        <span class="menu-label">{it.label}</span>
        {it.detail && <span class="menu-detail mono">{it.detail}</span>}
      </>
    );
    return it.href ? (
      <a
        key={it.label}
        role="menuitem"
        class={cls}
        href={it.href}
        target={it.newTab ? '_blank' : undefined}
        rel={it.newTab ? 'noopener' : undefined}
        onClick={() => close.current()}
      >
        {face}
      </a>
    ) : (
      <button
        key={it.label}
        type="button"
        role={it.checked === undefined ? 'menuitem' : 'menuitemradio'}
        aria-checked={it.checked}
        class={cls}
        disabled={it.disabled}
        onClick={() => pick(it)}
      >
        {face}
      </button>
    );
  };
  return (
    <>
      {backdrop && (
        <div
          ref={scrim}
          class="menu-scrim"
          aria-hidden="true"
          onPointerDown={() => (scrimDown.current = true)}
          onClick={(e) => {
            e.preventDefault();
            if (scrimDown.current) close.current();
          }}
          onContextMenu={(e) => e.preventDefault()}
        />
      )}
      <div
        class="menu ctx-menu"
        role="menu"
        aria-label={label}
        ref={box}
        style={{ left: `${x}px`, top: `${y}px` }}
        onKeyDown={ownKeys}
      >
        {heading && !step && <div class="menu-head">{heading}</div>}
        {step ? children : items.map(entry)}
      </div>
    </>
  );
}
