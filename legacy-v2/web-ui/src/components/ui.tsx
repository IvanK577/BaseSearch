// Small presentational primitives shared across pages.

import type { ReactNode } from "react";
import { Icon, type IconName } from "./Icon";
import { useStore } from "../state/store";
import { useI18n } from "../lib/i18n";

export function Spinner() {
  const { t } = useI18n();
  return <div className="spinner" role="status" aria-label={t("common_loading")} />;
}

export function Loading({ label }: { label?: string }) {
  return (
    <div className="center">
      <div className="row">
        <Spinner />
        {label ? <span className="muted">{label}</span> : null}
      </div>
    </div>
  );
}

export function EmptyState({
  icon = "database",
  title,
  hint,
  action,
  compact = false,
}: {
  icon?: IconName;
  title: string;
  hint?: string;
  action?: ReactNode;
  compact?: boolean;
}) {
  return (
    <div className={`empty-state ${compact ? "compact" : ""}`}>
      <Icon name={icon} className="empty-icon" size={compact ? 22 : 30} />
      <div className="empty-copy">
        <strong>{title}</strong>
        {hint ? <span>{hint}</span> : null}
      </div>
      {action}
    </div>
  );
}

export function Banner({
  children,
  variant = "error",
}: {
  children: ReactNode;
  variant?: "error" | "warn";
}) {
  return (
    <div
      className={variant === "warn" ? "banner banner-warn" : "banner"}
      role={variant === "error" ? "alert" : "status"}
    >
      <Icon name="alert" size={16} />
      {children}
    </div>
  );
}

export function Progress({ percent }: { percent: number | null }) {
  if (percent === null) {
    return (
      <div className="progress" role="progressbar">
        <div className="progress-fill progress-indeterminate" />
      </div>
    );
  }
  return (
    <div
      className="progress"
      role="progressbar"
      aria-valuemin={0}
      aria-valuemax={100}
      aria-valuenow={Math.round(Math.max(0, Math.min(100, percent)))}
    >
      <div
        className="progress-fill"
        style={{ width: `${Math.max(2, Math.min(100, percent))}%` }}
      />
    </div>
  );
}

export function ToastHost() {
  const { t } = useI18n();
  const { toasts, dismissToast } = useStore();
  if (toasts.length === 0) return null;
  return (
    <div className="toast-wrap">
      {toasts.map((toast) => (
        <button
          key={toast.id}
          className={`toast ${toast.kind}`}
          onClick={() => dismissToast(toast.id)}
          type="button"
          aria-label={`${toast.message}. ${t("common_close")}`}
          title={t("common_close")}
        >
          {toast.message}
          <Icon name="close" size={14} />
        </button>
      ))}
    </div>
  );
}
