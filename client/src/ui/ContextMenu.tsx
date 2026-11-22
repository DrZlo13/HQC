// One context menu for the session column, the project bar and the chat header: a session row's
// right click and a phone's long press, the column header's order and project menus, a project's
// right click on the bar, the header's mode and model pills. It stands at the point it was opened
// from, kept inside the window. An item may hold a second level (a model family's versions): on a
// computer a flyout beside the item, on a phone unfolded under it.

import type { ComponentChildren } from 'preact';
import { useEffect, useLayoutEffect, useRef, useState } from 'preact/hooks';
import { IconCheck, IconChevron, IconChevronDown } from './icons';

/** An item: a button (`onSelect`) or a link (`href`). */
export interface MenuItem {
  label: string;
  /** A second, muted part after the label, in the code font (a workspace's worker and path; on
   *  an item with a submenu, the one picked there). */
  detail?: string;
  icon?: ComponentChildren;
  /** A tooltip: what a short label does (the browser's switch: the agent restarts). */
  title?: string;
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
  /** A second level, one deep (its entries' own submenus are not shown). A click on the item
   *  still runs its `onSelect`: on a computer the submenu opens on hover or →, on a phone by
   *  the chevron beside the item. */
  submenu?: MenuEntry[];
  onSelect?: () => void;
}

export type MenuEntry = MenuItem | 'sep';

/** The space kept between a menu and the window's edge, px. */
const MARGIN = 8;
/** How long a flyout stays after the mouse left its item and itself, ms: a path from the item
 *  to the flyout that crosses a corner of the next item does not lose it. */
const FLYOUT_LEAVE_MS = 250;

/** Moves fixed box `box`, placed at (`x`, `y`), left and up as far as it overflows the window;
 *  with `above` (its bottom stands at `y`, see below) left only. */
function fitInView(box: HTMLElement, x: number, y: number, above: boolean): void {
  const view = box.ownerDocument.defaultView;
  if (!view) return;
  const r = box.getBoundingClientRect();
  const dx = Math.max(0, r.right - (view.innerWidth - MARGIN));
  box.style.left = `${Math.max(MARGIN, x - dx)}px`;
  if (above) return;
  const dy = Math.max(0, r.bottom - (view.innerHeight - MARGIN));
  box.style.top = `${Math.max(MARGIN, y - dy)}px`;
}

/** The menu at (`x`, `y`) — with `above`, its bottom left corner there: `items`, or
 *  `children` in their place (a confirmation step); `top` and `bottom` stand over and under the
 *  items (a segmented control, the usage limits). A press outside it (but on `anchor`, the
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
  top,
  bottom,
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
   *  menu does nothing else (opens no session, presses no button); a submenu unfolds under its
   *  item instead of a flyout. */
  backdrop?: boolean;
  onClose: () => void;
  /** Over the items (the header's mode pill: Ask | Auto | Allow all). */
  top?: ComponentChildren;
  /** Under the items (the model pill: the usage limits). */
  bottom?: ComponentChildren;
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
  // The item whose submenu is open, by its index in `items`: a flyout on a computer, unfolded
  // under the item on a phone.
  const [sub, setSub] = useState<number | null>(null);
  const flyout = useRef<HTMLDivElement>(null);
  // The item the flyout stands beside, and gives the focus back to.
  const subFrom = useRef<HTMLElement | null>(null);
  const subTimer = useRef<ReturnType<typeof setTimeout> | undefined>(undefined);
  // Opened by →: the flyout's first item takes the focus once it is there.
  const subFocus = useRef(false);
  useEffect(() => () => clearTimeout(subTimer.current), []);
  useLayoutEffect(() => {
    const el = box.current;
    if (!el) return;
    // Upwards: its bottom stands at `y`, so a menu that grows (a text box that follows its text)
    // grows upwards, and it is no taller than the space above `y`: past that it scrolls.
    if (above) {
      const view = el.ownerDocument.defaultView;
      el.style.top = 'auto';
      el.style.bottom = `${(view?.innerHeight ?? 0) - y}px`;
      el.style.maxHeight = `${y - MARGIN}px`;
      el.style.overflowY = 'auto';
    }
    fitInView(el, x, y, !!above);
  }, [x, y, step]);
  // The flyout beside its item: to the right where it fits, else to the left; its top level with
  // the item's, moved up as far as it would overflow the window's bottom.
  useLayoutEffect(() => {
    const el = flyout.current;
    const from = subFrom.current;
    if (!el || !from) return;
    const view = el.ownerDocument.defaultView;
    if (!view) return;
    const r = el.getBoundingClientRect();
    const f = from.getBoundingClientRect();
    const left = f.right + r.width <= view.innerWidth - MARGIN ? f.right : Math.max(MARGIN, f.left - r.width);
    el.style.left = `${left}px`;
    el.style.top = `${Math.max(MARGIN, Math.min(f.top - 6, view.innerHeight - MARGIN - r.height))}px`;
    if (subFocus.current) {
      subFocus.current = false;
      el.querySelector<HTMLElement>('.menu-item:not(:disabled)')?.focus();
    }
  }, [sub]);
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
  /** Esc closes the menu, ↑/↓ move between its items: the flyout's while the focus is in it,
   *  else the menu's own. */
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
    const active = el.ownerDocument.activeElement as HTMLElement | null;
    const fly = flyout.current;
    const inFly = !!fly && !!active && fly.contains(active);
    const scope = inFly ? fly : el;
    const list = [...scope.querySelectorAll<HTMLElement>('.menu-item:not(:disabled)')].filter((b) => inFly || !fly?.contains(b));
    if (!list.length) return;
    e.preventDefault();
    const i = list.indexOf(active as HTMLElement);
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

  // A computer's flyout: up beside item `i` (`from`), down a moment after the mouse left both.
  // `focus` (→): its first entry takes the focus, also when the mouse opened it already and no
  // render follows.
  const openFlyout = (i: number, from: HTMLElement, focus = false) => {
    clearTimeout(subTimer.current);
    subFrom.current = from;
    if (sub === i && flyout.current) {
      if (focus) flyout.current.querySelector<HTMLElement>('.menu-item:not(:disabled)')?.focus();
      return;
    }
    subFocus.current = focus;
    setSub(i);
  };
  const closeFlyout = () => {
    clearTimeout(subTimer.current);
    setSub(null);
  };
  const leaveFlyout = () => {
    clearTimeout(subTimer.current);
    subTimer.current = setTimeout(() => setSub(null), FLYOUT_LEAVE_MS);
  };
  const flyoutOpen = !backdrop && !step && sub !== null;

  // Inside the menu its keys stay its own: a permission card in the open chat answers Enter
  // and Esc on the document (Esc would deny the request, Enter allow it and cancel the item's
  // click).
  const ownKeys = (e: KeyboardEvent) => {
    // An open flyout takes Esc and ← first: it closes alone, the focus back on its item.
    if (flyoutOpen && (e.key === 'Escape' || e.key === 'ArrowLeft')) {
      e.stopPropagation();
      e.preventDefault();
      closeFlyout();
      subFrom.current?.focus({ preventScroll: true });
      return;
    }
    if (e.key !== 'Escape' && e.key !== 'Enter' && e.key !== 'ArrowDown' && e.key !== 'ArrowUp') return;
    e.stopPropagation();
    keys(e);
  };

  const pick = (it: MenuItem) => {
    if (!it.keepOpen) close.current();
    it.onSelect?.();
  };
  const face = (it: MenuItem) => (
    <>
      {it.checked === undefined ? it.icon : <span class="menu-check">{it.checked && <IconCheck />}</span>}
      <span class="menu-label">{it.label}</span>
      {it.detail && <span class="menu-detail mono">{it.detail}</span>}
    </>
  );
  // `i` is the item's index in `items`; `nested`: an entry of a submenu, which opens none.
  const entry = (it: MenuEntry, i: number, nested = false) => {
    if (it === 'sep') return <div key={`sep${i}`} class="menu-sep" role="separator" />;
    const subItems = !nested && it.submenu?.length ? it.submenu : undefined;
    // An item with a submenu shows the pick there beside its label, not a line under it.
    const cls = `menu-item ${it.cls ?? ''} ${it.danger ? 'menu-danger' : ''} ${it.detail && !subItems ? 'has-detail' : ''}`;
    if (it.href) {
      return (
        <a
          key={it.label}
          role="menuitem"
          class={cls}
          title={it.title}
          href={it.href}
          target={it.newTab ? '_blank' : undefined}
          rel={it.newTab ? 'noopener' : undefined}
          onClick={() => close.current()}
        >
          {face(it)}
        </a>
      );
    }
    const role = it.checked === undefined ? 'menuitem' : 'menuitemradio';
    if (subItems && backdrop) {
      // A phone: the chevron beside the item unfolds its submenu under it.
      const open = sub === i;
      return (
        <div key={it.label} class={`menu-split ${open ? 'is-open' : ''}`}>
          <div class="menu-split-row">
            <button type="button" role={role} aria-checked={it.checked} class={cls} title={it.title} disabled={it.disabled} onClick={() => pick(it)}>
              {face(it)}
            </button>
            <button
              type="button"
              class="menu-sub-toggle"
              aria-label={`${it.label}: more`}
              aria-expanded={open}
              disabled={it.disabled}
              onClick={() => setSub(open ? null : i)}
            >
              <IconChevronDown size={14} />
            </button>
          </div>
          {open && (
            <div class="menu-inline" role="group" aria-label={it.label}>
              {subItems.map((m, j) => entry(m, j, true))}
            </div>
          )}
        </div>
      );
    }
    // A computer: in a menu with submenus every item moves the flyout — up beside its own item,
    // down a moment after the mouse left it for an item without one.
    const hover =
      !backdrop && !nested && items.some((o) => o !== 'sep' && o.submenu?.length)
        ? {
            onMouseEnter: (e: MouseEvent) => (subItems ? openFlyout(i, e.currentTarget as HTMLElement) : leaveFlyout()),
            onMouseLeave: leaveFlyout,
          }
        : {};
    return (
      <button
        key={it.label}
        type="button"
        role={role}
        aria-checked={it.checked}
        aria-haspopup={subItems ? 'menu' : undefined}
        aria-expanded={subItems ? sub === i : undefined}
        class={`${cls} ${subItems ? 'menu-has-sub' : ''}`}
        title={it.title}
        disabled={it.disabled}
        onClick={() => pick(it)}
        onKeyDown={
          subItems
            ? (e: KeyboardEvent) => {
                if (e.key !== 'ArrowRight') return;
                e.preventDefault();
                openFlyout(i, e.currentTarget as HTMLElement, true);
              }
            : undefined
        }
        {...hover}
      >
        {face(it)}
        {subItems && (
          <span class="menu-sub-arrow" aria-hidden="true">
            <IconChevron size={12} />
          </span>
        )}
      </button>
    );
  };
  const flyItem = flyoutOpen && sub !== null ? items[sub] : undefined;
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
        {top && !step && <div class="menu-top">{top}</div>}
        {step ? children : items.map((it, i) => entry(it, i))}
        {bottom && !step && <div class="menu-bottom">{bottom}</div>}
        {/* Inside the menu's box, fixed beside its item: a press on it is no press away, and the
            focus in it is the menu's. Placed out of sight until the layout effect moves it. */}
        {flyItem && flyItem !== 'sep' && flyItem.submenu && flyItem.submenu.length > 0 && (
          <div
            class="menu ctx-menu ctx-flyout"
            role="menu"
            aria-label={flyItem.label}
            ref={flyout}
            style={{ left: '-9999px', top: '0px' }}
            onMouseEnter={() => clearTimeout(subTimer.current)}
            onMouseLeave={leaveFlyout}
          >
            {flyItem.submenu.map((m, j) => entry(m, j, true))}
          </div>
        )}
      </div>
    </>
  );
}
