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

export function IconOverview(props: IconProps) {
  return (
    <svg {...BASE} {...props}>
      <circle cx="12" cy="12" r="9" />
      <path d="M12 12l4.5-4.5" />
      <path d="M8.5 16.5a5 5 0 0 1 0-9" />
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

export function IconQuarantine(props: IconProps) {
  return (
    <svg {...BASE} {...props}>
      <path d="M12 3l8 3.5v5.8c0 4.6-3.3 8.5-8 9.7-4.7-1.2-8-5.1-8-9.7V6.5L12 3z" />
      <path d="M9 12l2 2 4-4" />
    </svg>
  );
}

export function IconMigrate(props: IconProps) {
  return (
    <svg {...BASE} {...props}>
      <path d="M4 8h13l-3-3" />
      <path d="M20 16H7l3 3" />
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
