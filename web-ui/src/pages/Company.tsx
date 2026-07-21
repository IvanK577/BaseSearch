import { useEffect, useState } from "react";

import { api, ApiError } from "../api/client";
import type { CompanyProfile } from "../api/types";
import { GroupTable, MonthChart, PriceTable, StatCard } from "../components/analytics";
import { Icon } from "../components/Icon";
import { Banner, EmptyState, Loading } from "../components/ui";
import { useI18n } from "../lib/i18n";
import { formatCompact, formatInt, formatMoney, formatMonth } from "../lib/format";
import { navigate, useRouteSegment } from "../lib/router";
import { useQueryStore } from "../state/query";
import { useStore } from "../state/store";

export function CompanyPage() {
  const { t } = useI18n();
  const { companyEdrpou: storedCompanyEdrpou } = useStore();
  const routeCompanyEdrpou = useRouteSegment(0);
  const companyEdrpou = routeCompanyEdrpou || storedCompanyEdrpou;
  const { setQuery } = useQueryStore();
  const [profile, setProfile] = useState<CompanyProfile | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (!companyEdrpou) return;
    let alive = true;
    setLoading(true);
    setError(null);
    setProfile(null);
    api
      .company(companyEdrpou, 10)
      .then((p) => alive && setProfile(p))
      .catch((err: ApiError) => alive && setError(err.message))
      .finally(() => alive && setLoading(false));
    return () => {
      alive = false;
    };
  }, [companyEdrpou]);

  if (!companyEdrpou) {
    return (
      <EmptyState
        icon="database"
        title="No company selected"
        hint="Click an EDRPOU code in results or analytics to open a company dossier."
        action={
          <button className="btn" onClick={() => navigate("analytics")}>
            {t("nav_analytics")}
          </button>
        }
      />
    );
  }

  const searchCompany = () => {
    setQuery({
      text: "",
      filters: {
        year: "",
        product_code: "",
        trademark: "",
        description: "",
        sender: "",
        recipient: "",
        edrpou: companyEdrpou,
        trade_country: "",
        dispatch_country: "",
        origin_country: "",
      },
    });
    navigate("search");
  };

  if (loading && !profile) return <Loading label={t("common_loading")} />;
  if (error) return <Banner>{error}</Banner>;
  if (!profile) return null;

  const o = profile.overview;
  const empty = o.row_count === 0;

  return (
    <div className="stack">
      <div className="panel panel-pad">
        <div className="row" style={{ justifyContent: "space-between", alignItems: "flex-start" }}>
          <div>
            <div className="section-title" style={{ margin: 0 }}>
              Company dossier · {profile.edrpou}
            </div>
            <div style={{ fontSize: 20, fontWeight: 700, marginTop: 4 }}>
              {profile.names[0] ?? "Unknown importer"}
            </div>
            {profile.names.length > 1 ? (
              <div className="faint" style={{ marginTop: 4 }}>
                Also seen as: {profile.names.slice(1).join(", ")}
              </div>
            ) : null}
          </div>
          <button className="btn btn-primary" onClick={searchCompany}>
            <Icon name="search" size={15} /> Search rows
          </button>
        </div>
      </div>

      {empty ? (
        <EmptyState icon="search" title="No rows for this EDRPOU in the database." />
      ) : (
        <>
          <div className="stat-grid">
            <StatCard label={t("common_rows")} value={formatInt(o.row_count)} />
            <StatCard label={t("common_declarations")} value={formatInt(o.declaration_count)} />
            <StatCard
              label={`${t("common_value")} USD`}
              value={formatCompact(o.total_value_usd)}
              hint={formatMoney(o.total_value_usd)}
            />
            <StatCard label={t("common_net_kg")} value={formatCompact(o.total_net_kg)} />
            <StatCard label="Value / kg" value={formatMoney(o.avg_value_per_net_kg)} />
            <StatCard label="Suppliers" value={formatInt(o.distinct_senders)} />
            <StatCard label="Product codes" value={formatInt(o.distinct_product_codes)} />
            <StatCard label="Origin countries" value={formatInt(o.distinct_origin_countries)} />
          </div>

          {profile.months.length > 0 ? (
            <div className="panel panel-pad">
              <div className="row" style={{ justifyContent: "space-between" }}>
                <div className="section-title" style={{ margin: 0 }}>
                  {t("analytics_months")}
                </div>
                <div className="faint">
                  {formatMonth(profile.months[0].month)} –{" "}
                  {formatMonth(profile.months[profile.months.length - 1].month)}
                </div>
              </div>
              <MonthChart months={profile.months} metric="total_value_usd" />
            </div>
          ) : null}

          <div className="grid-2">
            <div className="panel panel-pad">
              <div className="section-title">Top products</div>
              <GroupTable section={{ kind: "product_codes", rows: profile.top_products }} />
            </div>
            <div className="panel panel-pad">
              <div className="section-title">Top suppliers</div>
              <GroupTable section={{ kind: "senders", rows: profile.top_senders }} />
            </div>
          </div>

          <div className="panel panel-pad">
            <div className="section-title">Origin countries</div>
            <GroupTable
              section={{ kind: "origin_countries", rows: profile.top_origin_countries }}
            />
          </div>

          {profile.price_sections.length > 0 ? (
            <div className="panel panel-pad">
              <div className="section-title">{t("analytics_prices")}</div>
              <PriceTable metrics={profile.price_sections} />
            </div>
          ) : null}
        </>
      )}
    </div>
  );
}
