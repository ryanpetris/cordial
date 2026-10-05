// Inline symbolic icons drawn with currentColor.
import type { DeviceKind } from "../../shared/state.ts";

type Props = { size?: number };
const svg = (size: number, body: React.ReactNode) => (
  <svg width={size} height={size} viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth={1.6} strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
    {body}
  </svg>
);

export const KeyboardIcon = ({ size = 24 }: Props) =>
  svg(
    size,
    <>
      <rect x="2.5" y="6" width="19" height="12" rx="2.5" />
      <path d="M6 9.5h.01M9 9.5h.01M12 9.5h.01M15 9.5h.01M18 9.5h.01M6 12.5h.01M9 12.5h.01M12 12.5h.01M15 12.5h.01M18 12.5h.01M8 15h8" strokeWidth={2} />
    </>,
  );

export const MouseIcon = ({ size = 24 }: Props) =>
  svg(
    size,
    <>
      <rect x="6.5" y="3" width="11" height="18" rx="5.5" />
      <path d="M12 3v6M12 6.5v1.5" />
    </>,
  );

export const ComboIcon = ({ size = 24 }: Props) =>
  svg(
    size,
    <>
      <rect x="1.5" y="8" width="14" height="9.5" rx="2" />
      <path d="M4.5 11h.01M7.5 11h.01M10.5 11h.01M6 14.5h5" strokeWidth={1.8} />
      <rect x="16.5" y="6.5" width="6" height="11" rx="3" />
    </>,
  );

/** Media and volume keys. */
export const MediaIcon = ({ size = 24 }: Props) =>
  svg(size, <path d="M4 9.5h3.5L12 6v12l-4.5-3.5H4zM15.5 9.5a3.5 3.5 0 0 1 0 5M18 7a7 7 0 0 1 0 10" />);

/** Power, sleep and wake keys. */
export const PowerIcon = ({ size = 24 }: Props) => svg(size, <path d="M12 3.5v8M7.2 6.5a7 7 0 1 0 9.6 0" />);

export const UnknownDeviceIcon = ({ size = 24 }: Props) =>
  svg(
    size,
    <>
      <rect x="4" y="4" width="16" height="16" rx="4" />
      <path d="M9.8 9.7a2.3 2.3 0 1 1 3.3 2c-.7.4-1.1.9-1.1 1.6v.2" />
      <path d="M12 16.3h.01" strokeWidth={2.2} />
    </>,
  );

export const AdapterIcon = ({ size = 24 }: Props) =>
  svg(
    size,
    <>
      <rect x="7" y="8" width="10" height="13" rx="2.5" />
      <path d="M9 8V3.5h6V8M10.5 5.5h.01M13.5 5.5h.01M12 12v5" />
    </>,
  );

/** The application icon: the mark on its tile, as in assets/icons/app.svg. */
export const AppIcon = ({ size = 22 }: Props) => (
  <svg width={size} height={size} viewBox="0 0 64 64" aria-hidden="true">
    <defs>
      <linearGradient id="app-tile" x1="0" y1="0" x2="0" y2="1">
        <stop offset="0" stopColor="#62a0ea" />
        <stop offset="1" stopColor="#1c71d8" />
      </linearGradient>
    </defs>
    <rect x="3" y="3" width="58" height="58" rx="14" fill="url(#app-tile)" />
    <g transform="translate(-2.4 2.4)">
      <path
        d="M40.5 23.5A15 15 0 1 0 40.5 44.5M41.9 15.6A8 8 0 0 1 48.4 22.1M43 9.2A14.5 14.5 0 0 1 54.8 21"
        fill="none"
        stroke="#fff"
        strokeWidth={4.5}
        strokeLinecap="round"
      />
      <circle cx="40.5" cy="23.5" r="3.4" fill="#fff" />
    </g>
  </svg>
);

export const HomeIcon = ({ size = 24 }: Props) => svg(size, <path d="M4 10.5 12 4l8 6.5V20h-5v-5.5H9V20H4z" />);

export const RefreshIcon = ({ size = 16 }: Props) => svg(size, <path d="M20 12a8 8 0 1 1-2.3-5.6M20 4v4.5h-4.5" />);
export const GearIcon = ({ size = 16 }: Props) =>
  svg(
    size,
    <>
      <circle cx="12" cy="12" r="3" />
      <path d="M19.4 15a1.7 1.7 0 0 0 .3 1.8l.1.1a2 2 0 1 1-2.8 2.8l-.1-.1a1.7 1.7 0 0 0-1.8-.3 1.7 1.7 0 0 0-1 1.5V21a2 2 0 1 1-4 0v-.1a1.7 1.7 0 0 0-1.1-1.5 1.7 1.7 0 0 0-1.8.3l-.1.1a2 2 0 1 1-2.8-2.8l.1-.1a1.7 1.7 0 0 0 .3-1.8 1.7 1.7 0 0 0-1.5-1H3a2 2 0 1 1 0-4h.1a1.7 1.7 0 0 0 1.5-1.1 1.7 1.7 0 0 0-.3-1.8l-.1-.1a2 2 0 1 1 2.8-2.8l.1.1a1.7 1.7 0 0 0 1.8.3H9a1.7 1.7 0 0 0 1-1.5V3a2 2 0 1 1 4 0v.1a1.7 1.7 0 0 0 1 1.5 1.7 1.7 0 0 0 1.8-.3l.1-.1a2 2 0 1 1 2.8 2.8l-.1.1a1.7 1.7 0 0 0-.3 1.8V9a1.7 1.7 0 0 0 1.5 1H21a2 2 0 1 1 0 4h-.1a1.7 1.7 0 0 0-1.5 1z" />
    </>,
  );
export const MenuIcon = ({ size = 16 }: Props) => svg(size, <path d="M4 7h16M4 12h16M4 17h16" />);
export const ChevronIcon = ({ size = 16 }: Props) => svg(size, <path d="M9 6l6 6-6 6" />);
export const PlusIcon = ({ size = 16 }: Props) => svg(size, <path d="M12 5v14M5 12h14" />);
export const CopyIcon = ({ size = 16 }: Props) => svg(size, <path d="M9 9h11v11H9zM5 15H4V4h11v1" />);
export const ArrowUpIcon = ({ size = 16 }: Props) => svg(size, <path d="M12 19V5M6 11l6-6 6 6" />);
export const ArrowDownIcon = ({ size = 16 }: Props) => svg(size, <path d="M12 5v14M6 13l6 6 6-6" />);
export const CheckIcon = ({ size = 16 }: Props) => svg(size, <path d="M5 12.5l4.5 4.5L19 7.5" />);
export const CloseIcon = ({ size = 16 }: Props) => svg(size, <path d="M7 7l10 10M17 7 7 17" />);
export const UndoIcon = ({ size = 16 }: Props) => svg(size, <path d="M9 14 4 9l5-5M4 9h10.5a5.5 5.5 0 0 1 0 11H11" />);
export const TrashIcon = ({ size = 16 }: Props) => svg(size, <path d="M4 7h16M10 11v6M14 11v6M6 7l1 13h10l1-13M9 7V4h6v3" />);
export const PencilIcon = ({ size = 16 }: Props) => svg(size, <path d="M4 20l1-4.5L15.5 5a2.1 2.1 0 0 1 3 3L8 18.5zM13.5 7l3 3" />);
export const PlugIcon = ({ size = 16 }: Props) => svg(size, <path d="M9 3v4M15 3v4M7 7h10v3a5 5 0 0 1-10 0zM12 15v6" />);
export const UnplugIcon = ({ size = 16 }: Props) => svg(size, <path d="M9 3v4M15 3v4M7 7h10v3a5 5 0 0 1-10 0zM12 15v6M3 3l18 18" />);
export const LinkIcon = ({ size = 16 }: Props) =>
  svg(size, <path d="M10 14a4.5 4.5 0 0 0 6.4 0l3-3a4.5 4.5 0 0 0-6.4-6.4l-1.2 1.2M14 10a4.5 4.5 0 0 0-6.4 0l-3 3a4.5 4.5 0 0 0 6.4 6.4l1.2-1.2" />);
export const MoreIcon = ({ size = 16 }: Props) => svg(size, <path d="M5.5 12h.01M12 12h.01M18.5 12h.01" strokeWidth={3} />);
export const SwapIcon = ({ size = 16 }: Props) => svg(size, <path d="M4 8h15M15 4l4 4-4 4M20 16H5M9 12l-4 4 4 4" />);
export const WarningIcon = ({ size = 16 }: Props) =>
  svg(size, <path d="M12 3.5l9.5 16.5h-19zM12 10v4.5M12 17.2h.01" />);

/** A setting's state marker. Each state has its own shape, so none depends on color alone. */
export type MarkShape = "outline" | "filled" | "differs" | "draft" | "problem";

export function StateMark({ shape, size = 14 }: { shape: MarkShape; size?: number }) {
  switch (shape) {
    case "outline":
      return svg(size, <circle cx="12" cy="12" r="6" />);
    case "filled":
      return svg(size, <circle cx="12" cy="12" r="6" fill="currentColor" />);
    case "differs":
      return svg(size, <path d="M12 5l7 7-7 7-7-7z" fill="currentColor" />);
    case "draft":
      return svg(size, <path d="M5 19l1-4L16 5l3 3L9 18zM14 7l3 3" />);
    case "problem":
      return svg(size, <path d="M12 4l9 15.5H3zM12 10v4M12 16.8h.01" />);
  }
}

export function DeviceIcon({ kind, size = 24 }: { kind: DeviceKind; size?: number }) {
  switch (kind) {
    case "keyboard":
      return <KeyboardIcon size={size} />;
    case "mouse":
      return <MouseIcon size={size} />;
    case "keyboard_mouse":
      return <ComboIcon size={size} />;
    default:
      return <UnknownDeviceIcon size={size} />;
  }
}

/** A battery outline filled to the percentage, or a bolt while charging. */
export function BatteryGlyph({ percent, charging, low }: { percent: number | null; charging: boolean | null; low: boolean }) {
  const width = percent == null ? 0 : Math.max(1.5, (13 * percent) / 100);
  return (
    <svg className={low ? "battery low" : "battery"} width="22" height="12" viewBox="0 0 22 12" aria-hidden="true">
      <rect x="0.75" y="0.75" width="17.5" height="10.5" rx="2.5" fill="none" stroke="currentColor" strokeWidth="1.5" />
      <rect x="19.3" y="3.5" width="2" height="5" rx="1" fill="currentColor" />
      <rect x="3" y="3" width={width} height="6" rx="1" fill="currentColor" />
      {charging ? <path d="M10.5 1.8 7 6.6h3l-1.5 3.6 3.9-5h-3z" fill="var(--bolt)" stroke="var(--card)" strokeWidth="0.6" /> : null}
    </svg>
  );
}
