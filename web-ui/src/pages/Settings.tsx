import { useEffect, useState } from "react";

import { api, ApiError } from "../api/client";
import type { AccountInfo, DatabaseStats, EngineStatus, UserRole } from "../api/types";
import { Icon } from "../components/Icon";
import { useI18n, LANGUAGES } from "../lib/i18n";
import { formatBytes, formatInt } from "../lib/format";
import { roleLabel } from "../lib/roles";
import { useStore } from "../state/store";

export function SettingsPage() {
  const { t, lang, setLang } = useI18n();
  const { status, theme, toggleTheme, toast, refreshJobs, isAdmin } = useStore();
  const [stats, setStats] = useState<DatabaseStats | null>(null);
  const [engines, setEngines] = useState<EngineStatus | null>(null);
  const [busy, setBusy] = useState(false);

  const loadStats = () => {
    api.stats().then(setStats).catch(() => {});
  };
  useEffect(loadStats, []);
  useEffect(() => {
    api.engines().then(setEngines).catch(() => {});
  }, []);

  const runMaintenance = async (
    action: () => Promise<unknown>,
    label: string,
    confirmText?: string,
  ) => {
    if (confirmText && !window.confirm(confirmText)) return;
    setBusy(true);
    try {
      await action();
      toast(t("common_action_started", { action: label }), "info");
      refreshJobs();
    } catch (err) {
      toast(
        (err as ApiError)?.message ?? t("common_action_failed", { action: label }),
        "error",
      );
    } finally {
      setBusy(false);
      setTimeout(loadStats, 800);
    }
  };

  return (
    <div className="stack content-narrow">
      <div className="grid-2">
        <div className="panel panel-pad stack" style={{ gap: 14 }}>
          <div className="section-title" style={{ margin: 0 }}>
            {t("settings_language")}
          </div>
          <select
            className="select"
            value={lang}
            onChange={(e) => setLang(e.target.value as typeof lang)}
          >
            {LANGUAGES.map((l) => (
              <option key={l.code} value={l.code}>
                {l.label}
              </option>
            ))}
          </select>

          <div className="section-title" style={{ margin: "6px 0 0" }}>
            {t("settings_theme")}
          </div>
          <button className="btn" onClick={toggleTheme}>
            {theme === "dark" ? t("theme_light") : t("theme_dark")}
          </button>
        </div>

        <div className="panel panel-pad stack" style={{ gap: 10 }}>
          <div className="section-title" style={{ margin: 0 }}>
            {t("settings_database")}
          </div>
          <div className="kv">
            <div className="kv-label">{t("settings_path")}</div>
            <div className="kv-value faint" style={{ fontSize: 12 }}>
              {status?.db_path ?? "—"}
            </div>
          </div>
          <div className="kv">
            <div className="kv-label">{t("common_rows")}</div>
            <div className="kv-value">{formatInt(stats?.total_rows ?? status?.total_rows ?? 0)}</div>
          </div>
          <div className="kv">
            <div className="kv-label">{t("settings_unindexed")}</div>
            <div className="kv-value">{formatInt(stats?.unindexed_rows ?? 0)}</div>
          </div>
          <div className="kv">
            <div className="kv-label">{t("settings_size_on_disk")}</div>
            <div className="kv-value">
              {stats ? formatBytes(stats.storage.total_file_bytes) : "—"}
            </div>
          </div>
          <div className="kv" style={{ borderBottom: "none" }}>
            <div className="kv-label">{t("settings_last_import")}</div>
            <div className="kv-value faint">{stats?.last_import ?? "—"}</div>
          </div>
        </div>
      </div>

      {isAdmin ? <UsersPanel /> : null}

      {isAdmin ? (
      <div className="panel panel-pad stack" style={{ gap: 14 }}>
        <div className="section-title" style={{ margin: 0 }}>
          {t("settings_maintenance")}
        </div>
        <div className="toolbar">
          <button
            className="btn"
            disabled={busy}
            onClick={() => runMaintenance(api.optimize, t("settings_optimize"))}
          >
            <Icon name="database" size={16} /> {t("settings_optimize")}
          </button>
          <button
            className="btn"
            disabled={busy}
            onClick={() => runMaintenance(api.reindex, t("settings_reindex"))}
          >
            <Icon name="jobs" size={16} /> {t("settings_reindex")}
          </button>
          <button
            className="btn"
            disabled={busy}
            onClick={() => runMaintenance(api.compact, t("settings_compact"))}
            title={t("settings_compact_hint")}
          >
            <Icon name="database" size={16} /> {t("settings_compact")}
          </button>
          {engines?.duckdb_available ? (
            <button
              className="btn"
              disabled={busy}
              onClick={() => runMaintenance(api.buildOlap, t("settings_rebuild_cache"))}
            >
              <Icon name="jobs" size={16} /> {t("settings_rebuild_cache")}
            </button>
          ) : null}
          <div className="grow" />
          <button
            className="btn btn-danger"
            disabled={busy}
            onClick={() =>
              runMaintenance(api.clear, t("settings_clear"), t("settings_clear_confirm"))
            }
          >
            <Icon name="trash" size={16} /> {t("settings_clear")}
          </button>
        </div>
        {status?.lan_exposed ? (
          <div className="banner banner-warn">
            {t("settings_lan_warning")}
          </div>
        ) : null}
      </div>
      ) : null}
    </div>
  );
}

function UsersPanel() {
  const { t } = useI18n();
  const { toast, auth } = useStore();
  const [accounts, setAccounts] = useState<AccountInfo[]>([]);
  const [username, setUsername] = useState("");
  const [password, setPassword] = useState("");
  const [role, setRole] = useState<UserRole>("viewer");
  const [busy, setBusy] = useState(false);

  const load = () => {
    api.accounts().then(setAccounts).catch(() => {});
  };
  useEffect(load, []);

  const add = async () => {
    setBusy(true);
    try {
      await api.createAccount(username.trim(), password, role);
      toast(t("settings_account_created"), "success");
      setUsername("");
      setPassword("");
      load();
    } catch (err) {
      toast((err as ApiError)?.message ?? t("settings_account_create_failed"), "error");
    } finally {
      setBusy(false);
    }
  };

  const remove = async (name: string) => {
    if (!window.confirm(t("settings_account_remove_confirm", { name }))) return;
    try {
      await api.deleteAccount(name);
      load();
    } catch (err) {
      toast((err as ApiError)?.message ?? t("settings_account_remove_failed"), "error");
    }
  };

  return (
    <div className="panel panel-pad stack" style={{ gap: 14 }}>
      <div className="section-title" style={{ margin: 0 }}>
        {t("settings_accounts")}
      </div>
      <p className="muted" style={{ margin: 0 }}>
        {t("settings_accounts_desc")}
      </p>

      {accounts.length > 0 ? (
        <div className="table-wrap" style={{ maxHeight: "none" }}>
          <table className="grid" style={{ width: "100%" }}>
            <thead>
              <tr>
                <th>{t("settings_username")}</th>
                <th>{t("settings_role")}</th>
                <th>{t("settings_created")}</th>
                <th />
              </tr>
            </thead>
            <tbody>
              {accounts.map((account) => (
                <tr key={account.username} style={{ cursor: "default" }}>
                  <td>{account.username}</td>
                  <td>{roleLabel(t, account.role)}</td>
                  <td className="faint">{account.created_at}</td>
                  <td>
                    <button
                      className="btn btn-ghost btn-sm"
                      onClick={() => remove(account.username)}
                      aria-label={t("settings_remove_account", {
                        name: account.username,
                      })}
                      title={t("settings_remove_account", { name: account.username })}
                    >
                      <Icon name="trash" size={14} />
                    </button>
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      ) : (
        <div className="faint">{t("settings_no_accounts")}</div>
      )}

      <div className="row wrap" style={{ gap: 10, alignItems: "flex-end" }}>
        <div>
          <label className="field-label">{t("settings_username")}</label>
          <input
            className="input"
            style={{ width: 160 }}
            value={username}
            onChange={(e) => setUsername(e.target.value)}
          />
        </div>
        <div>
          <label className="field-label">{t("settings_password")}</label>
          <input
            className="input"
            style={{ width: 160 }}
            type="password"
            value={password}
            onChange={(e) => setPassword(e.target.value)}
          />
        </div>
        <div>
          <label className="field-label">{t("settings_role")}</label>
          <select
            className="select"
            style={{ width: 120 }}
            value={role}
            onChange={(e) => setRole(e.target.value as UserRole)}
          >
            <option value="viewer">{t("role_viewer")}</option>
            <option value="editor">{t("role_editor")}</option>
            <option value="admin">{t("role_admin")}</option>
            {auth?.user?.role === "owner" ? (
              <option value="owner">{t("role_owner")}</option>
            ) : null}
          </select>
        </div>
        <button
          className="btn btn-primary"
          disabled={busy || !username.trim() || password.length < 8}
          onClick={add}
        >
          <Icon name="plus" size={15} /> {t("settings_add_account")}
        </button>
      </div>
    </div>
  );
}
