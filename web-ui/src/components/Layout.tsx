import { useEffect, useState, type ReactNode } from "react";

import { api } from "../api/client";
import { useI18n, type MessageKey } from "../lib/i18n";
import { roleLabel } from "../lib/roles";
import { navigate, useRoute, type Route } from "../lib/router";
import { useStore } from "../state/store";
import { Icon, type IconName } from "./Icon";

interface SecondaryNavDef {
  route: Route;
  key: MessageKey;
}

interface PrimaryNavDef {
  route: Route;
  icon: IconName;
  key: MessageKey;
  activeRoutes: Route[];
  secondary?: SecondaryNavDef[];
}

export function Layout({ title, children }: { title: string; children: ReactNode }) {
  const route = useRoute();
  const { activeJobs, theme, toggleTheme, status, auth, refreshAuth } = useStore();
  const { t } = useI18n();
  const [menuOpen, setMenuOpen] = useState(false);
  const isLan = Boolean(auth?.required || status?.lan_exposed);
  const navigation: PrimaryNavDef[] = [
    {
      route: "search",
      icon: "search",
      key: "nav_search",
      activeRoutes: ["search", "company"],
    },
    {
      route: "analytics",
      icon: "analytics",
      key: "nav_analyze",
      activeRoutes: ["analytics", "risk"],
      secondary: [{ route: "risk", key: "nav_risk" }],
    },
    {
      route: "imports",
      icon: "database",
      key: "nav_data",
      activeRoutes: ["imports", "exports", "columns"],
      secondary: [
        { route: "exports", key: "nav_exports" },
        { route: "columns", key: "nav_columns" },
      ],
    },
    {
      route: "settings",
      icon: "settings",
      key: "nav_settings",
      activeRoutes: ["settings"],
    },
  ];

  useEffect(() => {
    if (!menuOpen) return;
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") setMenuOpen(false);
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [menuOpen]);

  return (
    <div className="app-shell">
      <a className="skip-link" href="#main-content">
        {t("shell_skip_to_content")}
      </a>

      <aside id="primary-navigation" className={`sidebar ${menuOpen ? "open" : ""}`}>
        <a className="brand" href="#/search" onClick={() => setMenuOpen(false)}>
          <img
            className="brand-mark"
            src="/base-search-icon.png"
            alt={t("appName")}
            width="32"
            height="32"
          />
          <div className="brand-copy">
            <div className="brand-name">
              {t("appName")} <span>{status?.version ?? "2.0"}</span>
            </div>
            <div className="brand-sub">{t("tagline")}</div>
          </div>
        </a>

        <nav className="sidebar-nav" aria-label={t("common_menu")}>
          {navigation.map((item) => {
            const areaActive = item.activeRoutes.includes(route);
            return (
              <div className={`nav-section ${areaActive ? "active" : ""}`} key={item.route}>
                <a
                  className={`nav-item nav-primary ${areaActive ? "active" : ""}`}
                  href={`#/${item.route}`}
                  aria-current={route === item.route ? "page" : undefined}
                  onClick={() => setMenuOpen(false)}
                >
                  <Icon name={item.icon} className="nav-icon" />
                  <span>{t(item.key)}</span>
                  {item.secondary && areaActive ? (
                    <Icon name="chevron-down" className="nav-disclosure" size={14} />
                  ) : null}
                </a>
                {item.secondary && areaActive ? (
                  <div className="nav-secondary">
                    {item.secondary.map((secondary) => (
                      <a
                        key={secondary.route}
                        className={route === secondary.route ? "active" : ""}
                        href={`#/${secondary.route}`}
                        aria-current={route === secondary.route ? "page" : undefined}
                        onClick={() => setMenuOpen(false)}
                      >
                        {t(secondary.key)}
                      </a>
                    ))}
                  </div>
                ) : null}
              </div>
            );
          })}
        </nav>

        <div className="sidebar-foot">
          <div className={`workspace-mode ${isLan ? "lan" : "personal"}`}>
            <Icon name={isLan ? "users" : "user"} size={18} />
            <div className="workspace-mode-copy">
              <strong>{isLan ? t("shell_lan_active") : t("shell_personal_workspace")}</strong>
              <span>
                {isLan && auth?.user
                  ? `${auth.user.username} · ${roleLabel(t, auth.user.role)}`
                  : t("shell_personal_no_sign_in")}
              </span>
            </div>
            {isLan && auth?.authenticated ? (
              <button
                className="icon-button"
                onClick={async () => {
                  await api.logout().catch(() => {});
                  refreshAuth();
                }}
                aria-label={t("common_sign_out")}
                title={t("common_sign_out")}
              >
                <Icon name="logout" size={16} />
              </button>
            ) : null}
          </div>
        </div>
      </aside>

      <div className="main">
        <header className="topbar">
          <button
            className="icon-button menu-toggle"
            onClick={() => setMenuOpen((value) => !value)}
            aria-label={t("common_menu")}
            title={t("common_menu")}
            aria-controls="primary-navigation"
            aria-expanded={menuOpen}
          >
            <Icon name="menu" />
          </button>
          <div className="topbar-title">
            <h1>{title}</h1>
            <span className={`mode-label ${isLan ? "lan" : "personal"}`}>
              {isLan ? "LAN" : t("shell_personal_short")}
            </span>
          </div>
          <div className="spacer" />
          <a
            className={`icon-button topbar-jobs ${route === "jobs" ? "active" : ""}`}
            href="#/jobs"
            aria-label={t("nav_jobs")}
            title={t("nav_jobs")}
          >
            <Icon name="jobs" size={17} />
            {activeJobs > 0 ? <span className="button-badge">{activeJobs}</span> : null}
          </a>
          <button
            className="icon-button"
            onClick={toggleTheme}
            aria-label={theme === "dark" ? t("theme_light") : t("theme_dark")}
            title={theme === "dark" ? t("theme_light") : t("theme_dark")}
            aria-pressed={theme === "light"}
          >
            <Icon name={theme === "dark" ? "sun" : "moon"} size={17} />
          </button>
        </header>

        <main id="main-content" className="content" tabIndex={-1}>
          {children}
        </main>
      </div>

      {menuOpen ? (
        <button
          className="backdrop navigation-backdrop"
          onClick={() => setMenuOpen(false)}
          aria-label={t("common_close")}
        />
      ) : null}
    </div>
  );
}

export { navigate };
