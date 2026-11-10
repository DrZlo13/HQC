// Line icons of the redesigns (docs/design/redesign-2026-10-24, redesign-2026-11-01), 20×20 viewBox, drawn in
// `currentColor`. Decorative: the button around them carries the label.

import type { ComponentChildren } from 'preact';

function Svg({ size = 16, fill, children }: { size?: number; fill?: boolean; children: ComponentChildren }) {
  return (
    <svg
      class="icon"
      aria-hidden="true"
      width={size}
      height={size}
      viewBox="0 0 20 20"
      fill={fill ? 'currentColor' : 'none'}
      stroke={fill ? undefined : 'currentColor'}
      stroke-width="1.8"
      stroke-linecap="round"
      stroke-linejoin="round"
    >
      {children}
    </svg>
  );
}

export const IconPlus = ({ size }: { size?: number }) => (
  <Svg size={size}>
    <path d="M10 4v12M4 10h12" />
  </Svg>
);

export const IconSettings = ({ size }: { size?: number }) => (
  <Svg size={size}>
    <path d="M3 6h14M3 14h14" />
    <circle cx="7" cy="6" r="2" fill="var(--rail)" />
    <circle cx="13" cy="14" r="2" fill="var(--rail)" />
  </Svg>
);

/** Project settings (T49), the gear of the sidebar header. */
export const IconGear = ({ size = 17 }: { size?: number }) => (
  <Svg size={size}>
    <circle cx="10" cy="10" r="2.6" />
    <path d="M10 2.5v2M10 15.5v2M2.5 10h2M15.5 10h2M4.7 4.7l1.4 1.4M13.9 13.9l1.4 1.4M4.7 15.3l1.4-1.4M13.9 6.1l1.4-1.4" />
  </Svg>
);

export const IconSearch = ({ size }: { size?: number }) => (
  <Svg size={size}>
    <circle cx="9" cy="9" r="5" />
    <path d="M13 13l4 4" />
  </Svg>
);

export const IconCopy = ({ size = 13 }: { size?: number }) => (
  <Svg size={size}>
    <rect x="7" y="7" width="9" height="9" rx="2" />
    <path d="M4 13V5a1 1 0 0 1 1-1h8" />
  </Svg>
);

export const IconFork = ({ size = 13 }: { size?: number }) => (
  <Svg size={size}>
    <circle cx="6" cy="4.5" r="1.8" />
    <circle cx="14" cy="4.5" r="1.8" />
    <circle cx="10" cy="15.5" r="1.8" />
    <path d="M6 6.3v1.2a3 3 0 0 0 3 3h2a3 3 0 0 0 3-3V6.3M10 10.5v3.2" />
  </Svg>
);

export const IconChevron = ({ size = 14 }: { size?: number }) => (
  <Svg size={size}>
    <path d="M8 5l5 5-5 5" />
  </Svg>
);

export const IconBack = ({ size = 22 }: { size?: number }) => (
  <Svg size={size}>
    <path d="M12.5 4.5L7 10l5.5 5.5" />
  </Svg>
);

export const IconCheck = ({ size = 14 }: { size?: number }) => (
  <Svg size={size}>
    <path d="M4 10l4 4 8-8" />
  </Svg>
);

export const IconCross = ({ size = 14 }: { size?: number }) => (
  <Svg size={size}>
    <path d="M5 5l10 10M15 5L5 15" />
  </Svg>
);

export const IconShield = ({ size = 15 }: { size?: number }) => (
  <Svg size={size}>
    <path d="M10 2.5l6 2.5v4.5c0 4-2.6 6.8-6 8-3.4-1.2-6-4-6-8V5z" />
  </Svg>
);

export const IconInfo = ({ size = 14 }: { size?: number }) => (
  <Svg size={size}>
    <circle cx="10" cy="10" r="7.5" />
    <path d="M10 9v5M10 6.2v.1" />
  </Svg>
);

export const IconWarn = ({ size = 14 }: { size?: number }) => (
  <Svg size={size}>
    <path d="M10 3l7.5 13h-15z" />
    <path d="M10 8v4M10 14.2v.1" />
  </Svg>
);

export const IconMore = ({ size = 18 }: { size?: number }) => (
  <Svg size={size} fill>
    <circle cx="4.5" cy="10" r="1.6" />
    <circle cx="10" cy="10" r="1.6" />
    <circle cx="15.5" cy="10" r="1.6" />
  </Svg>
);

export const IconStop = ({ size = 12 }: { size?: number }) => (
  <svg class="icon" aria-hidden="true" width={size} height={size} viewBox="0 0 12 12" fill="currentColor">
    <rect x="1" y="1" width="10" height="10" rx="2" />
  </svg>
);

export const IconSend = ({ size = 16 }: { size?: number }) => (
  <Svg size={size}>
    <path d="M10 16V4M5 9l5-5 5 5" />
  </Svg>
);

export const IconAttach = ({ size = 17 }: { size?: number }) => (
  <Svg size={size}>
    <path d="M15.5 9.5l-5.8 5.8a3.5 3.5 0 0 1-5-5l6.4-6.4a2.3 2.3 0 0 1 3.3 3.3l-6.4 6.4a1.2 1.2 0 0 1-1.7-1.7l5.8-5.8" />
  </Svg>
);

export const IconArchive = ({ size = 16 }: { size?: number }) => (
  <Svg size={size}>
    <rect x="3" y="4" width="14" height="4" rx="1" />
    <path d="M4.5 8v7a1 1 0 0 0 1 1h9a1 1 0 0 0 1-1V8M8 11.5h4" />
  </Svg>
);

export const IconRestore = ({ size = 15 }: { size?: number }) => (
  <Svg size={size}>
    <path d="M4 9.5a6 6 0 1 1 1.8 4.3" />
    <path d="M4 4.5V9.5h5" />
  </Svg>
);

export const IconTrash = ({ size = 15 }: { size?: number }) => (
  <Svg size={size}>
    <path d="M4 6h12M8 6V4h4v2M5.5 6l.8 10a1 1 0 0 0 1 .9h5.4a1 1 0 0 0 1-.9l.8-10" />
  </Svg>
);

/** Rename: a pencil. */
export const IconRename = ({ size = 15 }: { size?: number }) => (
  <Svg size={size}>
    <path d="M4 16l1-4 8-8 3 3-8 8zM11 6l3 3" />
  </Svg>
);

/** Favorites: a five-pointed star. */
export const IconStar = ({ size = 16 }: { size?: number }) => (
  <Svg size={size}>
    <path d="M10 2.8l1.8 5.1 5.4.2-4.2 3.3 1.5 5.2-4.5-3.1-4.5 3.1 1.5-5.2-4.2-3.3 5.4-.2z" />
  </Svg>
);

/** "Remove from favorites": the star struck through. */
export const IconStarOff = ({ size = 16 }: { size?: number }) => (
  <Svg size={size}>
    <path d="M10 3l2.2 4.5 4.8.7-3.5 3.4.8 4.9L10 14.2 5.7 16.5l.8-4.9L3 8.2l4.8-.7z" />
    <path d="M3 17L17 3" />
  </Svg>
);

/** "Open in new tab": a box with an arrow out of its corner. */
export const IconOpenNew = ({ size = 16 }: { size?: number }) => (
  <Svg size={size}>
    <path d="M11 4h5v5M16 4l-7 7M14 12v3a1 1 0 0 1-1 1H5a1 1 0 0 1-1-1V7a1 1 0 0 1 1-1h3" />
  </Svg>
);

/** "Move to project…": a folder with an arrow. */
export const IconMove = ({ size = 16 }: { size?: number }) => (
  <Svg size={size}>
    <path d="M3 6a1 1 0 0 1 1-1h4l2 2h6a1 1 0 0 1 1 1v7a1 1 0 0 1-1 1H4a1 1 0 0 1-1-1z" />
    <path d="M9 11h5M12 9l2 2-2 2" />
  </Svg>
);

/** A pinned block (the favorites over the session list). */
export const IconPin = ({ size = 12 }: { size?: number }) => (
  <Svg size={size}>
    <path d="M7 3h6M8 3v6l-3 3h10l-3-3V3M10 12v5" />
  </Svg>
);

/** ▾ of a dropdown, ▴ of a line that opens upwards. */
export const IconChevronDown = ({ size = 12 }: { size?: number }) => (
  <Svg size={size}>
    <path d="M5 8l5 5 5-5" />
  </Svg>
);

export const IconChevronUp = ({ size = 12 }: { size?: number }) => (
  <Svg size={size}>
    <path d="M5 12l5-5 5 5" />
  </Svg>
);

/** Reorder: two arrows, up and down. */
export const IconReorder = ({ size = 16 }: { size?: number }) => (
  <Svg size={size}>
    <path d="M7 3v14M3.5 6.5L7 3l3.5 3.5M13 17V3M9.5 13.5L13 17l3.5-3.5" />
  </Svg>
);

/** The drag handle: six dots. */
export const IconGrip = ({ size = 16 }: { size?: number }) => (
  <Svg size={size} fill>
    <circle cx="7.5" cy="4" r="1.4" />
    <circle cx="12.5" cy="4" r="1.4" />
    <circle cx="7.5" cy="10" r="1.4" />
    <circle cx="12.5" cy="10" r="1.4" />
    <circle cx="7.5" cy="16" r="1.4" />
    <circle cx="12.5" cy="16" r="1.4" />
  </Svg>
);

export const IconCode = ({ size = 18 }: { size?: number }) => (
  <Svg size={size}>
    <path d="M7 6l-4 4 4 4M13 6l4 4-4 4" />
  </Svg>
);

/** ↗ / ↙ of the blocks between sessions (T51): left this session, came into it. */
export const IconArrowOut = ({ size = 14 }: { size?: number }) => (
  <Svg size={size}>
    <path d="M5 15L15 5M8 5h7v7" />
  </Svg>
);

export const IconArrowIn = ({ size = 14 }: { size?: number }) => (
  <Svg size={size}>
    <path d="M15 5L5 15M12 15H5V8" />
  </Svg>
);
