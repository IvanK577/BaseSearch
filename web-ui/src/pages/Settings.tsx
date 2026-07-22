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
  const { status, theme, toggleTheme, toast, refreshJobs, isAdmin, auth } = useStore();
  const [stats, setStats] = useState<DatabaseStats | null>(null);
  const [engines, setEngines] = useState<EngineStatus | null>(null);
  const [busy, setBusy] = useState(false);
  const isLanWorkspace = Boolean(auth?.required || status?.lan_exposed);

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
    <div className="stack content-narrow settings-page">
      <div className="settings-overview-grid">
        <section className="panel panel-pad workspace-settings">
          <div className={`settings-mode-icon ${isLanWorkspace ? "lan" : "personal"}`}>
            <Icon name={isLanWorkspace ? "users" : "user"} size={20} />
          </div>
          <div>
            <div className="section-title">
              {isLanWorkspace ? t("shell_lan_active") : t("shell_personal_workspace")}
            </div>
            <strong>
              {isLanWorkspace
                ? t("settings_team_account_required")
                : t("settings_personal_no_account")}
            </strong>
            <p className="faint">
              {isLanWorkspace
                ? t("settings_lan_warning")
                : t("settings_personal_local_only")}
            </p>
          </div>
        </section>

        <section className="panel panel-pad settings-section">
          <div className="settings-row">
            <div className="settings-row-label">
              <Icon name="language" size={16} />
              <span>{t("settings_language")}</span>
            </div>
            <select
              className="select input-compact"
              value={lang}
              onChange={(event) => setLang(event.target.value as typeof lang)}
              aria-label={t("settings_language")}
            >
              {LANGUAGES.map((language) => (
                <option key={language.code} value={language.code}>
                  {language.label}
                </option>
              ))}
            </select>
          </div>
          <label className="settings-row" htmlFor="theme-switch">
            <div className="settings-row-label">
              <Icon name={theme === "dark" ? "moon" : "sun"} size={16} />
              <span>{t("settings_theme")}</span>
            </div>
            <span className="switch-control">
              <input
                id="theme-switch"
                type="checkbox"
                checked={theme === "light"}
                onChange={toggleTheme}
                aria-label={theme === "dark" ? t("theme_light") : t("theme_dark")}
              />
              <span aria-hidden="true" />
            </span>
          </label>
        </section>

        <section className="panel panel-pad stack database-settings" style={{ gap: 0 }}>
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
        </section>
      </div>

      {isAdmin && isLanWorkspace ? <UsersPanel /> : null}

      {isAdmin ? (
      <section className="panel panel-pad stack maintenance-settings" style={{ gap: 12 }}>
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
      </section>
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
  const requestedUsername = username.trim();
  const existingAccount = accounts.find(
    (account) =>
      requestedUsername.length > 0 &&
      account.username.localeCompare(requestedUsername, undefined, {
        sensitivity: "accent",
      }) === 0,
  );

  const load = () => {
    api.accounts().then(setAccounts).catch(() => {});
  };
  useEffect(load, []);

  const add = async () => {
    if (existingAccount) {
      toast(t("settings_account_exists", { name: existingAccount.username }), "error");
      return;
    }
    setBusy(true);
    try {
      await api.createAccount(requestedUsername, password, role);
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

      <div className="row wrap account-create-form" style={{ gap: 10, alignItems: "flex-end" }}>
        <div>
          <label className="field-label" htmlFor="account-username">
            {t("settings_username")}
          </label>
          <input
            id="account-username"
            className="input"
            style={{ width: 160 }}
            value={username}
            onChange={(e) => setUsername(e.target.value)}
          />
        </div>
        <div>
          <label className="field-label" htmlFor="account-password">
            {t("settings_password")}
          </label>
          <input
            id="account-password"
            className="input"
            style={{ width: 160 }}
            type="password"
            value={password}
            onChange={(e) => setPassword(e.target.value)}
          />
        </div>
        <div>
          <label className="field-label" htmlFor="account-role">
            {t("settings_role")}
          </label>
          <select
            id="account-role"
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
          disabled={busy || !requestedUsername || password.length < 8 || Boolean(existingAccount)}
          onClick={add}
        >
          <Icon name="plus" size={15} /> {t("settings_add_account")}
        </button>
        {existingAccount ? (
          <div className="account-name-error" role="alert">
            {t("settings_account_exists", { name: existingAccount.username })}
          </div>
        ) : null}
      </div>
    </div>
  );
}
