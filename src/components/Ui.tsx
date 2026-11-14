// 纯展示型通用界面组件，不持有业务状态，也不发起后端调用。
import type { ReactNode } from "react";
import { IconInfo, IconLock, IconShield, IconWarning } from "./Icons";

export type SemanticTone = "safe" | "caution" | "quarantine" | "danger" | "info" | "neutral";

export function StatusBadge({
  tone = "neutral",
  icon,
  children,
}: {
  tone?: SemanticTone;
  icon?: ReactNode;
  children: ReactNode;
}) {
  return (
    <span className={`badge badge-${tone}`}>
      {icon && <span className="badge-icon">{icon}</span>}
      <span>{children}</span>
    </span>
  );
}

export function AlertBanner({
  tone = "warn",
  children,
  actions,
}: {
  tone?: "warn" | "danger" | "info" | "success";
  children: ReactNode;
  actions?: ReactNode;
}) {
  const icon =
    tone === "danger" || tone === "warn" ? (
      <IconWarning />
    ) : tone === "success" ? (
      <IconShield />
    ) : (
      <IconInfo />
    );

  return (
    <div className={`alert-banner alert-${tone}`} role={tone === "danger" ? "alert" : "status"}>
      <span className="alert-icon">{icon}</span>
      <div className="alert-content">{children}</div>
      {actions && <div className="alert-actions">{actions}</div>}
    </div>
  );
}

export function AdminTag() {
  return (
    <StatusBadge tone="caution" icon={<IconLock width={12} height={12} />}>
      需要管理员
    </StatusBadge>
  );
}

export function EmptyState({
  icon,
  title,
  description,
}: {
  icon?: ReactNode;
  title: string;
  description?: string;
}) {
  return (
    <div className="empty-state">
      {icon && <div className="empty-state-icon">{icon}</div>}
      <p className="empty-state-title">{title}</p>
      {description && <p className="empty-state-desc">{description}</p>}
    </div>
  );
}
