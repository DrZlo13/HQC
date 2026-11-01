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

export const IconRename = ({ size = 15 }: { size?: number }) => (
  <Svg size={size}>
    <path d="M6 3h5l4 4v10H6z" />
    <path d="M11 3v4h4" />
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
