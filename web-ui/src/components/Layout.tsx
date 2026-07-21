// App shell: fire-branded sidebar navigation + topbar, with a responsive
// drawer on narrow screens.

import { useState, type ReactNode } from "react";

import { api } from "../api/client";
import { Icon, type IconName } from "./Icon";
import { useStore } from "../state/store";
import { LANGUAGES, useI18n, type MessageKey } from "../lib/i18n";
import { navigate, useRoute, type Route } from "../lib/router";
import { roleLabel } from "../lib/roles";

interface NavDef {
  route: Route;
  icon: IconName;
  key: MessageKey;
}

const NAV: NavDef[] = [
  { route: "search", icon: "search", key: "nav_search" },
  { route: "analytics", icon: "analytics", key: "nav_analytics" },
  { route: "risk", icon: "alert", key: "nav_risk" },
  { route: "imports", icon: "import", key: "nav_imports" },
  { route: "exports", icon: "export", key: "nav_exports" },
  { route: "columns", icon: "columns", key: "nav_columns" },
  { route: "jobs", icon: "jobs", key: "nav_jobs" },
  { route: "settings", icon: "settings", key: "nav_settings" },
];

export function Layout({ title, children }: { title: string; children: ReactNode }) {
  const route = useRoute();
  const { activeJobs, theme, toggleTheme, status, auth, refreshAuth } = useStore();
  const { t, lang, setLang } = useI18n();
  const [menuOpen, setMenuOpen] = useState(false);

  return (
    <div className="app-shell">
      <aside className={`sidebar ${menuOpen ? "open" : ""}`}>
        <a className="brand" href="#/search" onClick={() => setMenuOpen(false)}>
          <div className="brand-mark">
            <Icon name="flame" size={20} className="" />
          </div>
          <div>
            <div className="brand-name">{t("appName")}</div>
            <div className="brand-sub">{t("tagline")}</div>
          </div>
        </a>

        <nav>
          {NAV.map((item) => (
            <a
              key={item.route}
              className={`nav-item ${route === item.route ? "active" : ""}`}
              href={`#/${item.route}`}
              onClick={() => setMenuOpen(false)}
            >
              <Icon name={item.icon} className="nav-icon" />
              <span>{t(item.key)}</span>
              {item.route === "jobs" && activeJobs > 0 ? (
                <span className="nav-badge">{activeJobs}</span>
              ) : null}
            </a>
          ))}
        </nav>

        <div className="sidebar-foot">
          {auth?.authenticated && auth.user ? (
            <div style={{ marginBottom: 10 }}>
              <div className="faint" style={{ overflowWrap: "anywhere" }}>
                {auth.user.username} · {roleLabel(t, auth.user.role)}
              </div>
              <button
                className="btn btn-ghost btn-sm"
                style={{ marginTop: 6 }}
                onClick={async () => {
                  await api.logout().catch(() => {});
                  refreshAuth();
                }}
              >
                {t("common_sign_out")}
              </button>
            </div>
          ) : null}
          {t("appName")} v{status?.version ?? "2.0.0"}
          {status?.lan_exposed ? (
            <div style={{ color: "var(--flame-amber)", marginTop: 4 }}>
              {t("shell_lan_active")}
            </div>
          ) : null}
        </div>
      </aside>

      <div className="main">
        <header className="topbar">
          <button
            className="btn btn-ghost btn-sm menu-toggle"
            onClick={() => setMenuOpen((v) => !v)}
            aria-label={t("common_menu")}
            title={t("common_menu")}
          >
            <Icon name="menu" />
          </button>
          <h1>{title}</h1>
          <div className="spacer" />
          <select
            className="select"
            style={{ width: 130 }}
            value={lang}
            onChange={(e) => setLang(e.target.value as typeof lang)}
            aria-label={t("settings_language")}
          >
            {LANGUAGES.map((l) => (
              <option key={l.code} value={l.code}>
                {l.label}
              </option>
            ))}
          </select>
          <button className="btn btn-ghost btn-sm" onClick={toggleTheme}>
            {theme === "dark" ? t("theme_light") : t("theme_dark")}
          </button>
        </header>

        <main className="content">{children}</main>
      </div>

      {menuOpen ? (
        <div
          className="backdrop"
          style={{ zIndex: 45 }}
          onClick={() => setMenuOpen(false)}
        />
      ) : null}
    </div>
  );
}

export { navigate };
