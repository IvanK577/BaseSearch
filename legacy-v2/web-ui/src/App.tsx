import { Layout } from "./components/Layout";
import { Icon } from "./components/Icon";
import { Banner, EmptyState, Loading, ToastHost } from "./components/ui";
import { useI18n, type MessageKey } from "./lib/i18n";
import { useRoute, type Route } from "./lib/router";
import { useStore } from "./state/store";

import { LoginScreen } from "./pages/Login";
import { SearchPage } from "./pages/Search";
import { AnalyticsPage } from "./pages/Analytics";
import { PriceRiskPage } from "./pages/PriceRisk";
import { CompanyPage } from "./pages/Company";
import { ImportsPage } from "./pages/Imports";
import { ExportsPage } from "./pages/Exports";
import { ColumnsPage } from "./pages/Columns";
import { SettingsPage } from "./pages/Settings";
import { JobsPage } from "./pages/Jobs";

const TITLE_KEYS: Record<Route, MessageKey> = {
  search: "nav_search",
  analytics: "nav_analytics",
  risk: "nav_risk",
  company: "nav_company",
  imports: "imports_title",
  exports: "exports_title",
  columns: "columns_title",
  jobs: "jobs_title",
  settings: "settings_title",
};

function renderPage(route: Route) {
  switch (route) {
    case "search":
      return <SearchPage />;
    case "analytics":
      return <AnalyticsPage />;
    case "risk":
      return <PriceRiskPage />;
    case "company":
      return <CompanyPage />;
    case "imports":
      return <ImportsPage />;
    case "exports":
      return <ExportsPage />;
    case "columns":
      return <ColumnsPage />;
    case "jobs":
      return <JobsPage />;
    case "settings":
      return <SettingsPage />;
  }
}

export function App() {
  const route = useRoute();
  const { t } = useI18n();
  const { statusError, auth, authReadiness, needsLogin, refreshAuth } = useStore();

  if (authReadiness === "unknown") {
    return (
      <div className="auth-gate">
        <Loading label={t("common_loading")} />
      </div>
    );
  }
  if (authReadiness === "error" || auth === null) {
    return (
      <div className="auth-gate">
        <section className="panel auth-error-panel">
          <EmptyState
            compact
            icon="alert"
            title={t("auth_check_failed_title")}
            hint={t("auth_check_failed_hint")}
            action={
              <button className="btn btn-primary" onClick={refreshAuth}>
                <Icon name="refresh" size={16} /> {t("common_retry")}
              </button>
            }
          />
        </section>
      </div>
    );
  }
  if (needsLogin) {
    return <LoginScreen />;
  }

  return (
    <>
      <Layout title={t(TITLE_KEYS[route])}>
        {statusError ? (
          <Banner>
            {statusError} — {t("shell_server_stopped")}
          </Banner>
        ) : null}
        <div style={statusError ? { marginTop: 16 } : undefined}>{renderPage(route)}</div>
      </Layout>
      <ToastHost />
    </>
  );
}
