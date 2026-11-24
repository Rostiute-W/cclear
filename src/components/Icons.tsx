// 界面内使用的统一线框 SVG 图标，不依赖外部图标库或网络资源。
import type { SVGProps } from "react";

type IconProps = SVGProps<SVGSVGElement>;

const BASE: IconProps = {
  width: 16,
  height: 16,
  viewBox: "0 0 24 24",
  fill: "none",
  stroke: "currentColor",
  strokeWidth: 1.85,
  strokeLinecap: "round",
  strokeLinejoin: "round",
  "aria-hidden": true,
};

export function BrandMark({ className }: { className?: string }) {
  return (
    <svg
      className={className}
      width={28}
      height={28}
      viewBox="0 0 1024 1024"
      aria-hidden="true"
    >
      <defs>
        <linearGradient id="cc-brand-bg" x1="0%" y1="0%" x2="100%" y2="100%">
          <stop offset="0%" stopColor="#0f2942" />
          <stop offset="100%" stopColor="#0d6e6e" />
        </linearGradient>
      </defs>
      <rect x="44" y="44" width="936" height="936" rx="220" fill="url(#cc-brand-bg)" />
      <path
        d="M 694 316 A 266 266 0 1 0 694 708"
        fill="none"
        stroke="#ffffff"
        strokeWidth="128"
        strokeLinecap="round"
      />
      <path d="M 594 512 L 788 372 A 298 298 0 0 1 788 652 Z" fill="#5eead4" />
      <circle cx="512" cy="512" r="76" fill="#ffffff" />
    </svg>
  );
}

export function IconClean(props: IconProps) {
  return (
    <svg {...BASE} {...props}>
      <path d="M12 3l1.9 4.5L18.5 9l-4.6 1.5L12 15l-1.9-4.5L5.5 9l4.6-1.5L12 3z" />
      <path d="M5 17l1 2.2L8.2 20 6 20.8 5 23l-1-2.2L1.8 20 4 19.2 5 17z" />
      <path d="M18.5 14l.8 1.7 1.7.8-1.7.8-.8 1.7-.8-1.7-1.7-.8 1.7-.8.8-1.7z" />
    </svg>
  );
}

export function IconSpace(props: IconProps) {
  return (
    <svg {...BASE} {...props}>
      <rect x="3" y="4" width="18" height="16" rx="2" />
      <path d="M11 4v16" />
      <path d="M11 12h10" />
    </svg>
  );
}

export function IconHistory(props: IconProps) {
  return (
    <svg {...BASE} {...props}>
      <circle cx="12" cy="12" r="9" />
      <path d="M12 7v5l3 2" />
    </svg>
  );
}

export function IconShield(props: IconProps) {
  return (
    <svg {...BASE} {...props}>
      <path d="M12 3l7 3v6c0 4.2-3 7.8-7 9-4-1.2-7-4.8-7-9V6l7-3z" />
    </svg>
  );
}

export function IconWarning(props: IconProps) {
  return (
    <svg {...BASE} {...props}>
      <path d="M10.3 3.9L2.8 17a2 2 0 0 0 1.7 3h15a2 2 0 0 0 1.7-3L13.7 3.9a2 2 0 0 0-3.4 0z" />
      <path d="M12 9v4" />
      <path d="M12 17h.01" />
    </svg>
  );
}

export function IconLock(props: IconProps) {
  return (
    <svg {...BASE} {...props}>
      <rect x="4" y="11" width="16" height="9" rx="2" />
      <path d="M8 11V7a4 4 0 0 1 8 0v4" />
    </svg>
  );
}

export function IconTerminal(props: IconProps) {
  return (
    <svg {...BASE} {...props}>
      <rect x="3" y="4" width="18" height="16" rx="2" />
      <path d="M7 9l3 3-3 3" />
      <path d="M13 15h4" />
    </svg>
  );
}

export function IconFolder(props: IconProps) {
  return (
    <svg {...BASE} {...props}>
      <path d="M3 6a2 2 0 0 1 2-2h4l2 2h8a2 2 0 0 1 2 2v10a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V6z" />
    </svg>
  );
}

export function IconRefresh(props: IconProps) {
  return (
    <svg {...BASE} {...props}>
      <path d="M20 11a8 8 0 0 0-14-4.5L4 9" />
      <path d="M4 4v5h5" />
      <path d="M4 13a8 8 0 0 0 14 4.5l2-2.5" />
      <path d="M20 20v-5h-5" />
    </svg>
  );
}

export function IconInfo(props: IconProps) {
  return (
    <svg {...BASE} {...props}>
      <circle cx="12" cy="12" r="9" />
      <path d="M12 11v5" />
      <path d="M12 8h.01" />
    </svg>
  );
}

export function IconSearch(props: IconProps) {
  return (
    <svg {...BASE} {...props}>
      <circle cx="11" cy="11" r="7" />
      <path d="M20 20l-3.5-3.5" />
    </svg>
  );
}

export function IconSettings(props: IconProps) {
  return (
    <svg {...BASE} {...props}>
      <path d="M12.22 2h-.44a2 2 0 0 0-2 2v.18a2 2 0 0 1-1 1.73l-.43.25a2 2 0 0 1-2 0l-.15-.08a2 2 0 0 0-2.73.73l-.22.38a2 2 0 0 0 .73 2.73l.15.1a2 2 0 0 1 1 1.72v.51a2 2 0 0 1-1 1.74l-.15.09a2 2 0 0 0-.73 2.73l.22.38a2 2 0 0 0 2.73.73l.15-.08a2 2 0 0 1 2 0l.43.25a2 2 0 0 1 1 1.73V20a2 2 0 0 0 2 2h.44a2 2 0 0 0 2-2v-.18a2 2 0 0 1 1-1.73l.43-.25a2 2 0 0 1 2 0l.15.08a2 2 0 0 0 2.73-.73l.22-.39a2 2 0 0 0-.73-2.73l-.15-.08a2 2 0 0 1-1-1.74v-.5a2 2 0 0 1 1-1.74l.15-.09a2 2 0 0 0 .73-2.73l-.22-.38a2 2 0 0 0-2.73-.73l-.15.08a2 2 0 0 1-2 0l-.43-.25a2 2 0 0 1-1-1.73V4a2 2 0 0 0-2-2z" />
      <circle cx="12" cy="12" r="3" />
    </svg>
  );
}

export function IconZap(props: IconProps) {
  return (
    <svg {...BASE} {...props}>
      <polygon points="13 2 3 14 12 14 11 22 21 10 12 10 13 2" />
    </svg>
  );
}

export function IconGlobe(props: IconProps) {
  return (
    <svg {...BASE} {...props}>
      <circle cx="12" cy="12" r="9" />
      <path d="M3 12h18" />
      <path d="M12 3a13.5 13.5 0 0 1 0 18" />
      <path d="M12 3a13.5 13.5 0 0 0 0 18" />
    </svg>
  );
}

export function IconGrid(props: IconProps) {
  return (
    <svg {...BASE} {...props}>
      <rect x="3" y="3" width="7" height="7" rx="1.5" />
      <rect x="14" y="3" width="7" height="7" rx="1.5" />
      <rect x="3" y="14" width="7" height="7" rx="1.5" />
      <rect x="14" y="14" width="7" height="7" rx="1.5" />
    </svg>
  );
}

export function IconTrash(props: IconProps) {
  return (
    <svg {...BASE} {...props}>
      <path d="M3 6h18" />
      <path d="M8 6V4a1 1 0 0 1 1-1h6a1 1 0 0 1 1 1v2" />
      <path d="M6 6l1 14a2 2 0 0 0 2 2h6a2 2 0 0 0 2-2l1-14" />
      <path d="M10 11v6" />
      <path d="M14 11v6" />
    </svg>
  );
}

export function IconCpu(props: IconProps) {
  return (
    <svg {...BASE} {...props}>
      <rect x="5" y="5" width="14" height="14" rx="2" />
      <rect x="9" y="9" width="6" height="6" />
      <path d="M9 2v3M15 2v3M9 19v3M15 19v3M2 9h3M2 15h3M19 9h3M19 15h3" />
    </svg>
  );
}

export function IconChevron({ open, ...props }: IconProps & { open?: boolean }) {
  return (
    <svg
      {...BASE}
      {...props}
      style={{
        transform: open ? "rotate(90deg)" : "rotate(0deg)",
        transition: "transform 150ms ease",
        ...(props.style ?? {}),
      }}
    >
      <path d="M9 6l6 6-6 6" />
    </svg>
  );
}

export function IconUpload(props: IconProps) {
  return (
    <svg {...BASE} {...props}>
      <path d="M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4" />
      <polyline points="17 8 12 3 7 8" />
      <line x1="12" y1="3" x2="12" y2="15" />
    </svg>
  );
}

export function IconCheck(props: IconProps) {
  return (
    <svg {...BASE} {...props}>
      <polyline points="20 6 9 17 4 12" />
    </svg>
  );
}
