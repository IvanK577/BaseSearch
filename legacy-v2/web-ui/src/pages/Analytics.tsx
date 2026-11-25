import { useCallback, useEffect, useMemo, useRef, useState, type ReactNode } from "react";

import { api, ApiError } from "../api/client";
import type {
  Analytics,
  AnalyticsFilterAction,
  AnalyticsGroupRow,
  AnalyticsOverview,
  AnalyticsScope,
  AnalyticsSection,
  Filters,
  PivotDim,
  PivotMetric,
  PivotResult,
  Query,
  SchemaResponse,
} from "../api/types";
import {
  CurrencySummary,
  MonthChart,
  PriceTable,
  StatCard,
  ValuePerWeightSummary,
  WeightSummary,
} from "../components/analytics";
import { Icon } from "../components/Icon";
import { Banner, EmptyState, Loading } from "../components/ui";
import { useI18n, type MessageKey } from "../lib/i18n";
import { FILTER_FIELDS } from "../lib/filterFields";
import { fieldRefOf } from "../lib/advanced";
import { copyText } from "../lib/clipboard";
import { downloadCsv } from "../lib/csv";
import { buildReportHtml, buildReportText, queryLabel } from "../lib/report";
import {
  commonCurrency,
  compatibleCurrencyTotal,
  currencyLabel,
  rawNetWeightIsKg,
  safeNetWeightKg,
  safeRowShare,
  safeValuePerNetWeight,
  unitLabel,
} from "../lib/analyticsMeasures";
import {
  formatCompact,
  formatInt,
  formatMoney,
  formatMonth,
  formatPercent,
  monthBounds,
} from "../lib/format";
import { navigate } from "../lib/router";
import { useQueryStore } from "../state/query";
import { useStore } from "../state/store";

type Tab =
  | "overview"
  | "months"
  | "companies"
  | "products"
  | "countries"
  | "prices"
  | "pivot"
  | "report"
  | "compare";

const TABS: { id: Tab; key: MessageKey; secondary?: boolean }[] = [
  { id: "overview", key: "analytics_overview" },
  { id: "months", key: "analytics_months" },
  { id: "companies", key: "analytics_companies" },
  { id: "products", key: "analytics_products" },
  { id: "countries", key: "analytics_countries" },
  { id: "prices", key: "analytics_prices" },
  { id: "compare", key: "analytics_compare" },
  { id: "pivot", key: "analytics_pivot", secondary: true },
  { id: "report", key: "analytics_report", secondary: true },
];

// Tabs that self-fetch (their own state) rather than going through the shared
// scoped loader.
const SELF_FETCH_TABS: Tab[] = ["pivot", "report", "compare"];

const SCOPE_FOR_TAB: Record<Tab, AnalyticsScope | null> = {
  overview: null,
  months: null,
  companies: "companies",
  products: "products",
  countries: "countries",
  prices: "prices",
  pivot: null,
  report: null,
  compare: null,
};

export function AnalyticsPage() {
  const { t } = useI18n();
  const {
    query,
    isEmpty,
    applyText,
    applyFilter,
    applyAdvanced,
    applyDrilldown,
    undo,
    canUndo,
    reset,
  } = useQueryStore();
  const { openCompany, toast } = useStore();
  const [tab, setTab] = useState<Tab>("overview");
  const [forceAll, setForceAll] = useState(false);
  const [analytics, setAnalytics] = useState<Analytics | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [hsLevel, setHsLevel] = useState(10);
  // How many rows each ranked section pulls: 50, 200, or 500 ("see all").
  const [sectionLimit, setSectionLimit] = useState(200);
  const [schema, setSchema] = useState<SchemaResponse | null>(null);

  useEffect(() => {
    api.schema().then(setSchema).catch(() => {});
  }, []);

  const shouldRun = !isEmpty || forceAll;

  // Per-query cache so switching tabs is instant and never re-runs an identical
  // aggregation. Cleared when the query changes or the database grows (an
  // import finished), so analytics never serve stale totals.
  const { status } = useStore();
  const queryKey = useMemo(
    () => `${JSON.stringify(query)}|rows:${status?.total_rows ?? 0}`,
    [query, status?.total_rows],
  );
  const cacheRef = useRef<{ key: string; map: Map<string, Analytics> }>({
    key: "",
    map: new Map(),
  });
  // Monotonic request id: a slower earlier response can never overwrite a
  // newer one when the user switches tabs quickly.
  const reqRef = useRef(0);

  const load = useCallback(
    async (scope: AnalyticsScope | null) => {
      const cacheKey = `${scope ?? "_overview"}:${hsLevel}:${sectionLimit}`;
      const store = cacheRef.current;
      if (store.key !== queryKey) {
        store.key = queryKey;
        store.map.clear();
      }
      const cached = store.map.get(cacheKey);
      if (cached) {
        // Invalidate any slower in-flight request so it cannot overwrite the
        // cached data we are about to show, and drop a stuck spinner.
        reqRef.current += 1;
        setAnalytics(cached);
        setError(null);
        setLoading(false);
        return;
      }
      const id = ++reqRef.current;
      setLoading(true);
      setError(null);
      // The server admits only a couple of heavy reads at once and answers 503
      // under load; a transient busy reply is retried briefly before surfacing.
      const sleep = (ms: number) => new Promise((resolve) => setTimeout(resolve, ms));
      try {
        let res: Awaited<ReturnType<typeof api.analytics>> | null = null;
        for (let attempt = 0; attempt < 4; attempt += 1) {
          try {
            res = await api.analytics(query, scope, hsLevel, sectionLimit);
            break;
          } catch (err) {
            if (id !== reqRef.current) return;
            if ((err as ApiError)?.status === 503 && attempt < 3) {
              await sleep(500 * (attempt + 1));
              continue;
            }
            throw err;
          }
        }
        if (id !== reqRef.current || !res) return;
        store.map.set(cacheKey, res.data);
        setAnalytics(res.data);
      } catch (err) {
        if (id !== reqRef.current) return;
        setError((err as ApiError)?.message ?? t("analytics_failed"));
        // Drop the previous result too. The error banner and the panels render
        // independently, and the query chips above them already show the new
        // query, so keeping the old numbers presented them as the answer to a
        // question they were never computed for.
        setAnalytics(null);
      } finally {
        if (id === reqRef.current) setLoading(false);
      }
    },
    [query, queryKey, hsLevel, sectionLimit],
  );

  useEffect(() => {
    if (SELF_FETCH_TABS.includes(tab) || !shouldRun) return;
    load(SCOPE_FOR_TAB[tab]);
  }, [tab, load, shouldRun]);

  // Clicking a month filters the query to that month via a date range. Date
  // fields only accept Equals/Range, so a range over the whole month is the
  // correct (and valid) filter — `StartsWith` on a date is rejected by the core.
  const drillMonth = (month: string) => {
    const dateField = schema?.search_fields.find((f) => f.kind === "date");
    const bounds = monthBounds(month);
    if (!dateField || !bounds) {
      toast(t("analytics_no_date_column"), "error");
      return;
    }
    applyDrilldown({
      Group: {
        op: "And",
        negated: false,
        children: [
          {
            Condition: {
              field: fieldRefOf(dateField),
              op: "Range",
              value: { Range: { from: bounds.from, to: bounds.to } },
              negated: false,
            },
          },
        ],
      },
    });
    toast(`${t("analytics_filtered_to")} ${formatMonth(month)}`, "success");
  };

  const onAction = (action: AnalyticsFilterAction) => {
    applyFilter(action.field as keyof Filters, action.value);
  };

  if (!shouldRun) {
    return (
      <div className="stack">
        <AnalyticsSearchBar />
        <EmptyState
          icon="analytics"
          title={t("analytics_need_query")}
          hint={t("analytics_need_query_hint")}
          action={
            <div className="row" style={{ gap: 10 }}>
              <button className="btn btn-primary" onClick={() => setForceAll(true)}>
                {t("analytics_whole_db")}
              </button>
            </div>
          }
        />
      </div>
    );
  }

  return (
    <div className="stack">
      <AnalyticsSearchBar />
      <QueryChips
        query={query}
        onClearText={() => applyText("")}
        onClearFilter={(key) => applyFilter(key, "")}
        onClearAdvanced={() => applyAdvanced(null)}
        onClearAll={() => {
          // Back to the consent screen: an unfiltered aggregation over the
          // whole database must always be an explicit choice, never a side
          // effect of clearing chips.
          reset();
          setForceAll(false);
        }}
        onUndo={undo}
        canUndo={canUndo}
      />
      <div className="tabs" role="tablist" aria-label={t("nav_analytics")}>
        {TABS.map((tabDef) => (
          <button
            key={tabDef.id}
            className={`tab ${tab === tabDef.id ? "active" : ""} ${tabDef.secondary ? "secondary" : ""}`}
            onClick={() => setTab(tabDef.id)}
            role="tab"
            aria-selected={tab === tabDef.id}
          >
            {t(tabDef.key)}
          </button>
        ))}
      </div>

      {error ? <Banner>{error}</Banner> : null}

      {tab === "pivot" ? (
        <PivotPanel />
      ) : tab === "report" ? (
        <ReportPanel />
      ) : tab === "compare" ? (
        <ComparePanel />
      ) : loading ? (
        <Loading label={t("common_loading")} />
      ) : analytics ? (
        <>
          {tab === "overview" ? (
            <OverviewPanel
              analytics={analytics}
              query={query}
              hsLevel={hsLevel}
              onMonth={drillMonth}
              onOpenTab={setTab}
              onRow={onAction}
              onOpenCompany={openCompany}
            />
          ) : null}
          {tab === "months" ? <MonthsPanel analytics={analytics} onMonth={drillMonth} /> : null}

          {tab === "companies" ? (
            <SectionGroupPanel
              sections={analytics.company_sections}
              overview={analytics.overview}
              limit={sectionLimit}
              onLimit={setSectionLimit}
              onRow={(section, action) =>
                section.kind === "edrpou" ? openCompany(action.value) : onAction(action)
              }
              hintFor={(section) =>
                section.kind === "edrpou" ? t("analytics_open_company_hint") : undefined
              }
            />
          ) : null}

          {tab === "products" ? (
            <SectionGroupPanel
              sections={analytics.product_sections}
              overview={analytics.overview}
              limit={sectionLimit}
              onLimit={setSectionLimit}
              onRow={(_, action) => onAction(action)}
              extraControls={
                <div className="row" style={{ gap: 6 }}>
                  <span className="field-label" style={{ margin: 0 }}>
                    {t("analytics_hs_grouping")}:
                  </span>
                  {[2, 4, 6, 10].map((lvl) => (
                    <button
                      key={lvl}
                      className={`btn btn-sm ${hsLevel === lvl ? "" : "btn-ghost"}`}
                      onClick={() => setHsLevel(lvl)}
                    >
                      {lvl === 10 ? t("analytics_hs_full") : lvl}
                    </button>
                  ))}
                </div>
              }
            />
          ) : null}

          {tab === "countries" ? (
            <SectionGroupPanel
              sections={analytics.country_sections}
              overview={analytics.overview}
              limit={sectionLimit}
              onLimit={setSectionLimit}
              onRow={(_, action) => onAction(action)}
            />
          ) : null}

          {tab === "prices" ? (
            <div className="stack">
              <UnmappedCurrencyHint overview={analytics.overview} />
              <div className="panel panel-pad">
                <div className="row" style={{ justifyContent: "space-between", alignItems: "baseline" }}>
                  <div className="section-title" style={{ marginBottom: 2 }}>
                    {t("analytics_prices")}
                  </div>
                  <button
                    className="btn btn-sm btn-ghost"
                    disabled={analytics.price_sections.every((m) => m.count === 0)}
                    onClick={() =>
                      downloadCsv(
                        "prices",
                        [
                          t("price_col_metric"),
                          t("price_col_samples"),
                          t("price_col_median"),
                          t("price_col_average"),
                          t("price_col_weighted"),
                          "P25",
                          "P75",
                          t("price_col_min"),
                          t("price_col_max"),
                        ],
                        analytics.price_sections
                          .filter((m) => m.count > 0)
                          .map((m) => [
                            m.kind,
                            m.count,
                            m.median,
                            m.average,
                            m.weighted_average,
                            m.p25,
                            m.p75,
                            m.minimum,
                            m.maximum,
                          ]),
                      )
                    }
                  >
                    <Icon name="download" size={14} /> {t("sec_export")}
                  </button>
                </div>
                <p className="muted" style={{ margin: "0 0 10px", fontSize: 13 }}>
                  {t("analytics_prices_intro")}
                </p>
                <PriceTable metrics={analytics.price_sections} />
              </div>
            </div>
          ) : null}
        </>
      ) : null}
    </div>
  );
}

/** True when the dataset has values but no recognized currency for any of them. */
function currencyIsUnmapped(overview: AnalyticsOverview): boolean {
  const totals = overview.measures.currency_totals;
  return totals.length > 0 && totals.every((total) => !total.known);
}

function UnmappedCurrencyHint({ overview }: { overview: AnalyticsOverview }) {
  const { t } = useI18n();
  if (!currencyIsUnmapped(overview)) return null;
  return (
    <div className="panel panel-pad analytics-hint">
      <Icon name="alert" size={18} className="analytics-hint-icon" />
      <div className="analytics-hint-body">
        <strong>{t("analytics_currency_unmapped")}</strong>
        <span className="muted"> {t("analytics_currency_unmapped_hint")}</span>
      </div>
      <button className="btn btn-sm" onClick={() => navigate("columns")}>
        <Icon name="columns" size={14} /> {t("analytics_map_columns")}
      </button>
    </div>
  );
}

// The overview response carries only totals + months, so these top-5 previews
// are fetched lazily after the instant stats render. Each is a single grouped
// scan; running them sequentially keeps a large database responsive and warms
// the per-tab cache for when the user drills in.
const PREVIEW_DIMS: {
  scope: AnalyticsScope;
  tab: Tab;
  titleKey: MessageKey;
  pick: (a: Analytics) => AnalyticsSection | undefined;
}[] = [
  {
    scope: "companies",
    tab: "companies",
    titleKey: "analytics_companies",
    pick: (a) => a.company_sections[0],
  },
  {
    scope: "products",
    tab: "products",
    titleKey: "analytics_products",
    pick: (a) => a.product_sections[0],
  },
  {
    scope: "countries",
    tab: "countries",
    titleKey: "analytics_countries",
    pick: (a) => a.country_sections[0],
  },
];

type PreviewState = { section: AnalyticsSection | null; loading: boolean; error: boolean };

function OverviewPreviews({
  query,
  hsLevel,
  onOpenTab,
  onRow,
  onOpenCompany,
}: {
  query: Query;
  hsLevel: number;
  onOpenTab: (tab: Tab) => void;
  onRow: (action: AnalyticsFilterAction) => void;
  onOpenCompany: (edrpou: string) => void;
}) {
  const { t } = useI18n();
  const [previews, setPreviews] = useState<Record<string, PreviewState>>({});

  useEffect(() => {
    let alive = true;
    setPreviews(
      Object.fromEntries(
        PREVIEW_DIMS.map((d) => [d.scope, { section: null, loading: true, error: false }]),
      ),
    );
    (async () => {
      // One request for all three cards. As three scoped requests the server
      // recomputed the shared currency and weight buckets for each one and
      // discarded most of the sections it produced; it also occupied three of
      // its few heavy-read slots in a row. It still replies 503 under load, so
      // the request retries briefly before giving up.
      const sleep = (ms: number) => new Promise((resolve) => setTimeout(resolve, ms));
      for (let attempt = 0; attempt < 4; attempt += 1) {
        try {
          const res = await api.analyticsPreviews(query, hsLevel, 6);
          if (!alive) return;
          setPreviews(
            Object.fromEntries(
              PREVIEW_DIMS.map((dim) => [
                dim.scope,
                { section: dim.pick(res.data) ?? null, loading: false, error: false },
              ]),
            ),
          );
          return;
        } catch (err) {
          if (!alive) return;
          const busy = (err as ApiError)?.status === 503;
          if (busy && attempt < 3) {
            await sleep(500 * (attempt + 1));
            continue;
          }
          setPreviews(
            Object.fromEntries(
              PREVIEW_DIMS.map((dim) => [
                dim.scope,
                { section: null, loading: false, error: true },
              ]),
            ),
          );
          return;
        }
      }
    })();
    return () => {
      alive = false;
    };
  }, [query, hsLevel]);

  return (
    <div className="overview-previews">
      {PREVIEW_DIMS.map((dim) => {
        const state = previews[dim.scope] ?? { section: null, loading: true, error: false };
        return (
          <div className="panel panel-pad overview-preview" key={dim.scope}>
            <div className="row" style={{ justifyContent: "space-between", alignItems: "baseline" }}>
              <div className="section-title" style={{ margin: 0 }}>
                {t(dim.titleKey)}
              </div>
              <button className="btn btn-ghost btn-sm" onClick={() => onOpenTab(dim.tab)}>
                {t("analytics_see_all")} →
              </button>
            </div>
            <div className="faint" style={{ fontSize: 12, marginBottom: 6 }}>
              {t("analytics_top5_rows")}
            </div>
            {state.loading ? (
              <div className="overview-preview-skeleton">
                {[0, 1, 2, 3, 4].map((i) => (
                  <div className="skeleton-row" key={i} />
                ))}
              </div>
            ) : state.error ? (
              <div className="faint">{t("analytics_failed")}</div>
            ) : !state.section || state.section.rows.length === 0 ? (
              <div className="faint">{t("analytics_no_group_data")}</div>
            ) : (
              <div className="overview-preview-list">
                {state.section.rows.slice(0, 5).map((row, i) => {
                  const openable =
                    dim.scope === "companies" && state.section?.kind === "edrpou" && row.label;
                  return (
                    <button
                      className="overview-preview-row"
                      key={`${row.label}-${i}`}
                      onClick={() =>
                        openable
                          ? onOpenCompany(row.label)
                          : row.filter_action && onRow(row.filter_action)
                      }
                      disabled={!openable && !row.filter_action}
                      title={row.label}
                    >
                      <span className="overview-preview-label">{row.label || "—"}</span>
                      <span className="overview-preview-metric">
                        {formatInt(row.rows)} {t("common_rows")}
                        {row.measures.currency_totals.length > 0 ? (
                          <span className="faint">
                            {" · "}
                            {formatCompact(row.measures.currency_totals[0].total_value)}{" "}
                            {currencyLabel(
                              row.measures.currency_totals[0].currency,
                              t("analytics_unknown_currency"),
                            )}
                          </span>
                        ) : null}
                      </span>
                    </button>
                  );
                })}
              </div>
            )}
          </div>
        );
      })}
    </div>
  );
}

export function OverviewPanel({
  analytics,
  query,
  hsLevel,
  onMonth,
  onOpenTab,
  onRow,
  onOpenCompany,
}: {
  analytics: Analytics;
  query: Query;
  hsLevel: number;
  onMonth: (month: string) => void;
  onOpenTab: (tab: Tab) => void;
  onRow: (action: AnalyticsFilterAction) => void;
  onOpenCompany: (edrpou: string) => void;
}) {
  const { t } = useI18n();
  const o = analytics.overview;
  const legacyRawKg = rawNetWeightIsKg(o.measures);
  const monthCurrency = commonCurrency(analytics.months);
  const monthNetIsComparable = analytics.months.every(
    (month) => safeNetWeightKg(month, legacyRawKg) !== null,
  );
  const monthMetric = monthCurrency
    ? "value"
    : monthNetIsComparable
      ? "net_weight"
      : "rows";
  const avgPerMonth =
    analytics.months.length > 0 ? o.row_count / analytics.months.length : o.row_count;

  const copySummary = () => {
    const lines = [
      `${t("common_rows")}\t${o.row_count}`,
      `${t("common_declarations")}\t${o.declaration_count}`,
      `${t("analytics_companies")}\t${o.distinct_edrpou}`,
      `${t("analytics_product_codes")}\t${o.distinct_product_codes}`,
      `${t("company_suppliers")}\t${o.distinct_senders}`,
      `${t("analytics_origin_countries")}\t${o.distinct_origin_countries}`,
    ];
    copyText(lines.join("\n"));
  };

  return (
    <div className="stack">
      <UnmappedCurrencyHint overview={o} />
      <div className="row" style={{ justifyContent: "flex-end" }}>
        <button className="btn btn-sm btn-ghost" onClick={copySummary}>
          {t("analytics_copy_summary")}
        </button>
      </div>
      <div className="stat-grid">
        <StatCard label={t("common_rows")} value={formatInt(o.row_count)} />
        {/* Datasets without document numbers get no permanent "0" card. */}
        {o.declaration_count > 0 ? (
          <StatCard
            label={t("common_declarations")}
            value={formatInt(o.declaration_count)}
          />
        ) : null}
        <StatCard
          label={t("analytics_value_by_currency")}
          value={<CurrencySummary measures={o.measures} legacyUsd={o.total_value_usd} />}
        />
        <StatCard
          label={t("analytics_net_weight")}
          value={<WeightSummary totals={o.measures.net_weight_totals} />}
        />
        <StatCard
          label={t("analytics_value_per_weight")}
          value={<ValuePerWeightSummary measures={o.measures} />}
        />
        <StatCard label={t("analytics_companies")} value={formatInt(o.distinct_edrpou)} />
        {o.distinct_senders > 0 ? (
          <StatCard label={t("company_suppliers")} value={formatInt(o.distinct_senders)} />
        ) : null}
        <StatCard label={t("analytics_product_codes")} value={formatInt(o.distinct_product_codes)} />
        {o.distinct_trademarks > 0 ? (
          <StatCard label={t("sec_trademarks")} value={formatInt(o.distinct_trademarks)} />
        ) : null}
        <StatCard
          label={t("analytics_origin_countries")}
          value={formatInt(o.distinct_origin_countries)}
        />
        {analytics.months.length > 1 ? (
          <StatCard
            label={t("analytics_avg_per_month")}
            value={formatInt(avgPerMonth)}
            hint={`${formatInt(analytics.months.length)} ${t("common_months").toLowerCase()}`}
          />
        ) : null}
      </div>

      {analytics.months.length > 0 ? (
        <div className="panel panel-pad">
          <div className="row" style={{ justifyContent: "space-between" }}>
            <div className="section-title" style={{ margin: 0 }}>
              {t("analytics_months")}
            </div>
            <div className="faint">
              {formatMonth(analytics.months[0].month)} –{" "}
              {formatMonth(analytics.months[analytics.months.length - 1].month)}
            </div>
          </div>
          <MonthChart
            months={analytics.months}
            metric={monthMetric}
            onSelect={onMonth}
            allowLegacyRawKg={legacyRawKg}
          />
        </div>
      ) : null}

      <OverviewPreviews
        query={query}
        hsLevel={hsLevel}
        onOpenTab={onOpenTab}
        onRow={onRow}
        onOpenCompany={onOpenCompany}
      />
    </div>
  );
}

type MonthMetric = "value" | "net_weight" | "rows";

type MonthSortField = "month" | "value" | "net_kg" | "rows" | "docs" | "mom" | "yoy" | "cumulative";

function priorYearMonth(month: string): string {
  const [year, rest] = month.split("-");
  const y = Number(year);
  return Number.isFinite(y) ? `${y - 1}-${rest}` : month;
}

function growthCell(value: number | null): ReactNode {
  if (value === null) return <span style={{ color: "var(--text-faint)" }}>—</span>;
  return (
    <span style={{ color: value >= 0 ? "var(--success-text)" : "var(--danger-text)" }}>
      {value >= 0 ? "+" : ""}
      {formatPercent(value, 0)}
    </span>
  );
}

function MonthsPanel({
  analytics,
  onMonth,
}: {
  analytics: Analytics;
  onMonth: (month: string) => void;
}) {
  const { t } = useI18n();
  const allMonths = analytics.months;
  const legacyRawKg = rawNetWeightIsKg(analytics.overview.measures);
  const valueCurrency = commonCurrency(allMonths);
  const netIsComparable =
    allMonths.length > 0 &&
    allMonths.every((month) => safeNetWeightKg(month, legacyRawKg) !== null);
  const defaultMetric: MonthMetric = valueCurrency
    ? "value"
    : netIsComparable
      ? "net_weight"
      : "rows";
  const [metric, setMetric] = useState<MonthMetric>(defaultMetric);
  const [sortField, setSortField] = useState<MonthSortField>("month");
  const [dir, setDir] = useState<"asc" | "desc">("asc");
  // Range selector so long histories stay readable; null means "all".
  const [range, setRange] = useState<number | null>(null);

  useEffect(() => {
    if (
      (metric === "value" && !valueCurrency) ||
      (metric === "net_weight" && !netIsComparable)
    ) {
      setMetric(defaultMetric);
    }
  }, [defaultMetric, metric, netIsComparable, valueCurrency]);

  const months = useMemo(
    () => (range && allMonths.length > range ? allMonths.slice(-range) : allMonths),
    [allMonths, range],
  );

  // The primary metric feeds trend math (MoM/YoY/cumulative/peak) so every
  // derived number reflects the same, currency-honest measure.
  const primaryOf = useCallback(
    (m: (typeof allMonths)[number]): number | null => {
      if (valueCurrency) return compatibleCurrencyTotal(m)?.total_value ?? null;
      if (netIsComparable) return safeNetWeightKg(m, legacyRawKg);
      return m.rows;
    },
    [valueCurrency, netIsComparable, legacyRawKg],
  );

  // Year-over-year needs the same calendar month a year earlier, matched by key
  // across the full history (not just the visible range).
  const primaryByMonth = useMemo(() => {
    const map = new Map<string, number | null>();
    for (const m of allMonths) map.set(m.month, primaryOf(m));
    return map;
  }, [allMonths, primaryOf]);

  const enriched = useMemo(() => {
    let running = 0;
    return months.map((m, i) => {
      const value = primaryOf(m);
      const previous = i > 0 ? primaryOf(months[i - 1]) : null;
      const mom =
        value !== null && previous !== null && previous > 0
          ? ((value - previous) / previous) * 100
          : null;
      const yearAgo = primaryByMonth.get(priorYearMonth(m.month)) ?? null;
      const yoy =
        value !== null && yearAgo !== null && yearAgo > 0
          ? ((value - yearAgo) / yearAgo) * 100
          : null;
      running += value ?? 0;
      return {
        ...m,
        compatibleValue: valueCurrency ? compatibleCurrencyTotal(m)?.total_value ?? null : null,
        compatibleNetKg: safeNetWeightKg(m, legacyRawKg),
        primary: value,
        mom,
        yoy,
        cumulative: value !== null ? running : null,
      };
    });
  }, [months, primaryOf, primaryByMonth, valueCurrency, legacyRawKg]);

  const sorted = useMemo(() => {
    const key = (r: (typeof enriched)[number]): number | string => {
      switch (sortField) {
        case "month":
          return r.month;
        case "value":
          return r.primary ?? -Infinity;
        case "net_kg":
          return r.compatibleNetKg ?? r.rows;
        case "rows":
          return r.rows;
        case "docs":
          return r.declarations;
        case "mom":
          return r.mom ?? -Infinity;
        case "yoy":
          return r.yoy ?? -Infinity;
        case "cumulative":
          return r.cumulative ?? -Infinity;
      }
    };
    return [...enriched].sort((a, b) => {
      const ka = key(a);
      const kb = key(b);
      const cmp =
        typeof ka === "string" ? ka.localeCompare(kb as string) : (ka as number) - (kb as number);
      return dir === "asc" ? cmp : -cmp;
    });
  }, [enriched, sortField, dir]);

  if (allMonths.length === 0) {
    return <EmptyState icon="analytics" title={t("analytics_no_group_data")} />;
  }

  const totalValue = valueCurrency
    ? enriched.reduce((sum, month) => sum + (month.compatibleValue ?? 0), 0)
    : null;
  const totalNet = netIsComparable
    ? enriched.reduce((sum, month) => sum + (month.compatibleNetKg ?? 0), 0)
    : null;
  const totalRows = enriched.reduce((sum, month) => sum + month.rows, 0);
  const totalDocs = enriched.reduce((sum, month) => sum + month.declarations, 0);
  const peak = enriched.reduce((a, b) => ((b.primary ?? 0) > (a.primary ?? 0) ? b : a));
  const first = enriched[0]?.primary ?? null;
  const last = enriched[enriched.length - 1]?.primary ?? null;
  const overall =
    first !== null && last !== null && first > 0 ? ((last - first) / first) * 100 : null;
  const avgPrimary =
    enriched.length > 0
      ? enriched.reduce((sum, m) => sum + (m.primary ?? 0), 0) / enriched.length
      : 0;
  const primaryUnit = valueCurrency ?? (netIsComparable ? "kg" : t("common_rows"));

  const metricOptions: { id: MonthMetric; label: string }[] = [
    ...(valueCurrency
      ? [{ id: "value" as const, label: `${t("common_value")} ${valueCurrency}` }]
      : []),
    ...(netIsComparable ? [{ id: "net_weight" as const, label: t("common_net_kg") }] : []),
    { id: "rows", label: t("common_rows") },
  ];

  const setSort = (field: MonthSortField) => {
    if (field === sortField) {
      setDir((d) => (d === "asc" ? "desc" : "asc"));
    } else {
      setSortField(field);
      setDir(field === "month" ? "asc" : "desc");
    }
  };
  const arrow = (field: MonthSortField) =>
    sortField === field ? (dir === "asc" ? " ▲" : " ▼") : "";

  const exportCsv = () => {
    downloadCsv(
      "monthly_dynamics",
      [
        t("analytics_month"),
        valueCurrency ? `${t("common_value")} ${valueCurrency}` : t("analytics_value_by_currency"),
        t("analytics_net_weight"),
        t("common_rows"),
        t("common_declarations"),
        `${t("analytics_mom")} %`,
        `${t("analytics_yoy")} %`,
        t("analytics_cumulative"),
      ],
      [...enriched]
        .sort((a, b) => a.month.localeCompare(b.month))
        .map((m) => [
          m.month,
          m.primary ?? "",
          m.compatibleNetKg ?? "",
          m.rows,
          m.declarations,
          m.mom !== null ? m.mom.toFixed(1) : "",
          m.yoy !== null ? m.yoy.toFixed(1) : "",
          m.cumulative ?? "",
        ]),
    );
  };

  const rangeOptions: { id: number | null; label: string }[] = [
    { id: 12, label: "12" },
    { id: 24, label: "24" },
    { id: null, label: t("analytics_range_all") },
  ];

  return (
    <div className="stack">
      <div className="panel panel-pad row wrap faint" style={{ gap: 14, fontSize: 12 }}>
        <span>
          {formatInt(months.length)} {t("common_months").toLowerCase()} /{" "}
          {formatMonth(months[0].month)} - {formatMonth(months[months.length - 1].month)}
        </span>
        <span>
          {t("analytics_peak_month")}: <strong>{formatMonth(peak.month)}</strong> (
          {formatCompact(peak.primary ?? peak.rows)} {primaryUnit})
        </span>
        <span>
          {t("analytics_average")}: <strong>{formatCompact(avgPrimary)} {primaryUnit}</strong>
        </span>
        {overall !== null ? (
          <span>
            {t("analytics_first_last")}:{" "}
            <strong>{`${overall >= 0 ? "+" : ""}${formatPercent(overall, 0)}`}</strong>
          </span>
        ) : null}
        {totalValue !== null && valueCurrency ? (
          <span>
            {t("common_value")}: <strong>{formatCompact(totalValue)} {valueCurrency}</strong>
          </span>
        ) : null}
        {totalNet !== null ? (
          <span>
            {t("analytics_net_weight")}: <strong>{formatCompact(totalNet)} kg</strong>
          </span>
        ) : null}
      </div>

      <div className="panel panel-pad">
        <div className="row wrap" style={{ justifyContent: "space-between", gap: 8 }}>
          <div className="section-title" style={{ margin: 0 }}>
            {t("analytics_months")}
          </div>
          <div className="row wrap" style={{ gap: 10 }}>
            <div className="row" style={{ gap: 6 }}>
              <span className="field-label" style={{ margin: 0 }}>{t("chart_metric")}:</span>
              {metricOptions.map((m) => (
                <button
                  key={m.id}
                  className={`btn btn-sm ${metric === m.id ? "" : "btn-ghost"}`}
                  onClick={() => setMetric(m.id)}
                >
                  {m.label}
                </button>
              ))}
            </div>
            {allMonths.length > 12 ? (
              <div className="row" style={{ gap: 6 }}>
                <span className="field-label" style={{ margin: 0 }}>{t("analytics_range")}:</span>
                {rangeOptions.map((r) => (
                  <button
                    key={r.label}
                    className={`btn btn-sm ${range === r.id ? "" : "btn-ghost"}`}
                    onClick={() => setRange(r.id)}
                  >
                    {r.label}
                  </button>
                ))}
              </div>
            ) : null}
          </div>
        </div>
        <MonthChart
          months={months}
          metric={metric}
          onSelect={onMonth}
          allowLegacyRawKg={legacyRawKg}
        />
      </div>

      <div className="panel panel-pad">
        <div className="row" style={{ justifyContent: "space-between", alignItems: "baseline" }}>
          <div className="section-title">{t("analytics_month_breakdown")}</div>
          <button className="btn btn-sm" onClick={exportCsv}>
            {t("sec_export")}
          </button>
        </div>
        <div className="table-wrap" style={{ maxHeight: "none" }}>
          <table className="grid" style={{ width: "100%" }}>
            <thead>
              <tr>
                <SortTh label={t("analytics_month")} onClick={() => setSort("month")} arrow={arrow("month")} />
                <SortTh
                  label={valueCurrency ? `${t("common_value")} ${valueCurrency}` : t("analytics_value_by_currency")}
                  onClick={() => setSort("value")}
                  arrow={arrow("value")}
                />
                <SortTh label={t("analytics_net_weight")} onClick={() => setSort("net_kg")} arrow={arrow("net_kg")} />
                <SortTh label={t("common_rows")} onClick={() => setSort("rows")} arrow={arrow("rows")} />
                <SortTh label={t("common_declarations")} onClick={() => setSort("docs")} arrow={arrow("docs")} />
                <SortTh label={t("analytics_mom")} onClick={() => setSort("mom")} arrow={arrow("mom")} />
                <SortTh label={t("analytics_yoy")} onClick={() => setSort("yoy")} arrow={arrow("yoy")} />
                <SortTh label={t("analytics_cumulative")} onClick={() => setSort("cumulative")} arrow={arrow("cumulative")} />
              </tr>
            </thead>
            <tbody>
              {sorted.map((m) => (
                <tr key={m.month} onClick={() => onMonth(m.month)} style={{ cursor: "pointer" }}>
                  <td>{formatMonth(m.month)}</td>
                  <td>
                    <CurrencySummary measures={m.measures} legacyUsd={m.total_value_usd} />
                  </td>
                  <td>
                    {m.compatibleNetKg !== null ? (
                      `${formatInt(m.compatibleNetKg)} kg`
                    ) : (
                      <WeightSummary totals={m.measures.net_weight_totals} />
                    )}
                  </td>
                  <td>{formatInt(m.rows)}</td>
                  <td>{formatInt(m.declarations)}</td>
                  <td>{growthCell(m.mom)}</td>
                  <td>{growthCell(m.yoy)}</td>
                  <td>
                    {m.cumulative !== null ? (
                      <span className="faint">
                        {formatCompact(m.cumulative)} {primaryUnit}
                      </span>
                    ) : (
                      "—"
                    )}
                  </td>
                </tr>
              ))}
            </tbody>
            <tfoot>
              <tr className="month-total-row">
                <td>
                  <strong>{t("analytics_total")}</strong>
                </td>
                <td>
                  <strong>
                    {totalValue !== null && valueCurrency
                      ? `${formatCompact(totalValue)} ${valueCurrency}`
                      : "—"}
                  </strong>
                </td>
                <td>
                  <strong>{totalNet !== null ? `${formatCompact(totalNet)} kg` : "—"}</strong>
                </td>
                <td>
                  <strong>{formatInt(totalRows)}</strong>
                </td>
                <td>
                  <strong>{formatInt(totalDocs)}</strong>
                </td>
                <td colSpan={3} />
              </tr>
            </tfoot>
          </table>
        </div>
      </div>
    </div>
  );
}

// One ranked table at a time with a compact section switcher, instead of a
// tall stack of huge tables: the screen never sprawls, whatever the database
// size, and the visible table gets the full height.
function SectionGroupPanel({
  sections,
  overview,
  limit,
  onLimit,
  onRow,
  hintFor,
  extraControls,
}: {
  sections: AnalyticsSection[];
  overview: AnalyticsOverview;
  limit: number;
  onLimit: (n: number) => void;
  onRow: (section: AnalyticsSection, action: AnalyticsFilterAction) => void;
  hintFor?: (section: AnalyticsSection) => string | undefined;
  extraControls?: ReactNode;
}) {
  const { t } = useI18n();
  const withData = sections.filter((section) => section.rows.length > 0);
  const list = withData.length > 0 ? withData : sections.slice(0, 1);
  const [activeKind, setActiveKind] = useState(list[0]?.kind);
  const active = list.find((section) => section.kind === activeKind) ?? list[0];
  if (!active) {
    return <EmptyState icon="analytics" title={t("analytics_no_group_data")} />;
  }
  return (
    <div className="stack" style={{ gap: 10 }}>
      <div className="row wrap" style={{ gap: 8, justifyContent: "space-between" }}>
        <div className="row wrap" style={{ gap: 6 }}>
          {list.map((section) => (
            <button
              key={section.kind}
              className={`btn btn-sm ${active.kind === section.kind ? "" : "btn-ghost"}`}
              onClick={() => setActiveKind(section.kind)}
            >
              {sectionTitle(section.kind, t)}
              <span className="faint" style={{ marginLeft: 4 }}>
                {formatInt(section.rows.length)}
              </span>
            </button>
          ))}
        </div>
        <div className="row wrap" style={{ gap: 10 }}>
          {extraControls}
          <SectionLimitBar value={limit} onChange={onLimit} />
        </div>
      </div>
      <SectionTable
        title={sectionTitle(active.kind, t)}
        section={active}
        overview={overview}
        limit={limit}
        onRow={(action) => onRow(active, action)}
        hint={hintFor?.(active)}
        fullHeight
      />
    </div>
  );
}

type SortField = "name" | "share" | "rows" | "docs" | "value" | "net_kg" | "vpk";

// A full, sortable, filterable, exportable ranking table — the "see all"
// replacement for the old top-N teaser. Sort/filter run client-side over the
// rows already fetched (up to `limit`).
export function SectionTable({
  title,
  section,
  overview,
  limit,
  onRow,
  hint,
  fullHeight,
}: {
  title: string;
  section: AnalyticsSection;
  overview: AnalyticsOverview;
  limit: number;
  onRow?: (action: AnalyticsFilterAction) => void;
  hint?: string;
  fullHeight?: boolean;
}) {
  const { t } = useI18n();
  const valueCurrency = compatibleCurrencyTotal(overview)?.currency ?? null;
  const legacyRawKg = rawNetWeightIsKg(overview.measures);
  const [sortField, setSortField] = useState<SortField>("rows");
  const [dir, setDir] = useState<"asc" | "desc">("desc");
  const [filter, setFilter] = useState("");
  // Minimum row-share threshold (%) to focus on the material groups.
  const [minShare, setMinShare] = useState(0);

  const rows = useMemo(() => {
    const needle = filter.trim().toLowerCase();
    let list = needle
      ? section.rows.filter((r) => r.label.toLowerCase().includes(needle))
      : section.rows.slice();
    if (minShare > 0) {
      list = list.filter(
        (r) => safeRowShare(r, overview.row_count, valueCurrency !== null) >= minShare,
      );
    }
    const key = (r: AnalyticsGroupRow): number | string => {
      switch (sortField) {
        case "name":
          return r.label.toLowerCase();
        case "share":
          return safeRowShare(r, overview.row_count, valueCurrency !== null);
        case "rows":
          return r.rows;
        case "docs":
          return r.declarations;
        case "value": {
          const total = compatibleCurrencyTotal(r);
          return valueCurrency && total?.currency === valueCurrency
            ? total.total_value
            : r.rows;
        }
        case "net_kg":
          return safeNetWeightKg(r, legacyRawKg) ?? r.rows;
        case "vpk": {
          const ratio = safeValuePerNetWeight(r, legacyRawKg);
          return valueCurrency && ratio?.currency === valueCurrency
            ? (ratio.value_per_weight ?? r.rows)
            : r.rows;
        }
      }
    };
    list.sort((a, b) => {
      const ka = key(a);
      const kb = key(b);
      const cmp =
        typeof ka === "string"
          ? ka.localeCompare(kb as string)
          : (ka as number) - (kb as number);
      return dir === "asc" ? cmp : -cmp;
    });
    return list;
  }, [section.rows, filter, sortField, dir, legacyRawKg, overview.row_count, valueCurrency]);

  const setSort = (field: SortField) => {
    if (field === sortField) {
      setDir((d) => (d === "asc" ? "desc" : "asc"));
    } else {
      setSortField(field);
      setDir(field === "name" ? "asc" : "desc");
    }
  };
  const arrow = (field: SortField) =>
    sortField === field ? (dir === "asc" ? " ▲" : " ▼") : "";

  const exportCsv = () => {
    const headers = [
      t("col_name"),
      t("analytics_rows_share"),
      t("common_rows"),
      t("analytics_value_by_currency"),
      t("analytics_net_weight"),
      t("analytics_value_per_weight"),
    ];
    const data = rows.map((r) => {
      const currencyTotals =
        r.measures.currency_totals.length > 0
          ? r.measures.currency_totals
          : compatibleCurrencyTotal(r)
            ? [compatibleCurrencyTotal(r)!]
            : [];
      const value = currencyTotals
        .map(
          (total) =>
            `${total.total_value} ${currencyLabel(total.currency, t("analytics_unknown_currency"))}`,
        )
        .join(" | ");
      const weight = r.measures.net_weight_totals
        .map((total) => {
          const sourceUnit = unitLabel(total.source_unit, t("analytics_unknown_unit"));
          return total.known && total.normalized_unit === "kg" && total.total_kg !== null
            ? `${total.total_source_weight} ${sourceUnit} -> ${total.total_kg} kg`
            : `${total.total_source_weight} ${sourceUnit}`;
        })
        .join(" | ");
      const ratios = r.measures.value_per_net_weight
        .filter((ratio) => ratio.value_per_weight !== null)
        .map(
          (ratio) =>
            `${ratio.value_per_weight} ${currencyLabel(ratio.currency, t("analytics_unknown_currency"))}/${unitLabel(ratio.normalized_weight_unit, t("analytics_unknown_unit"))}`,
        )
        .join(" | ");
      return [
        r.label,
        safeRowShare(r, overview.row_count, valueCurrency !== null).toFixed(2),
        r.rows,
        value,
        weight,
        ratios,
      ];
    });
    downloadCsv(title.replace(/[^\w]+/g, "_").toLowerCase() || "section", headers, data);
  };

  const capped = section.rows.length >= limit;

  // Cumulative share (Pareto): running share of the whole dataset as you read
  // down the ranked rows, so "top N = X%" concentration is obvious at a glance.
  const maxShare = Math.max(
    ...rows.map((r) => safeRowShare(r, overview.row_count, valueCurrency !== null)),
    1,
  );
  let runningRows = 0;
  const cumulativeByIndex = rows.map((r) => {
    runningRows += r.rows;
    return overview.row_count > 0 ? (runningRows / overview.row_count) * 100 : 0;
  });
  const visibleRows = rows.reduce((sum, r) => sum + r.rows, 0);
  const visibleDocs = rows.reduce((sum, r) => sum + r.declarations, 0);
  const visibleShare =
    overview.row_count > 0 ? (visibleRows / overview.row_count) * 100 : 0;

  const copyTable = () => {
    const header = [
      "#",
      t("col_name"),
      `${t("analytics_rows_share")} %`,
      t("common_rows"),
      t("common_declarations"),
    ].join("\t");
    const body = rows.map((r, i) =>
      [
        i + 1,
        r.label,
        safeRowShare(r, overview.row_count, valueCurrency !== null).toFixed(2),
        r.rows,
        r.declarations,
      ].join("\t"),
    );
    copyText([header, ...body].join("\n"));
  };

  return (
    <div className="panel panel-pad">
      <div className="row wrap" style={{ justifyContent: "space-between", gap: 10 }}>
        <div className="section-title" style={{ margin: 0 }}>{title}</div>
        <div className="row wrap" style={{ gap: 8 }}>
          <input
            className="input"
            style={{ width: 160, padding: "6px 10px" }}
            placeholder={t("sec_filter")}
            value={filter}
            onChange={(e) => setFilter(e.target.value)}
          />
          <label className="row" style={{ gap: 4, alignItems: "center" }}>
            <span className="field-label" style={{ margin: 0 }}>{t("sec_min_share")}</span>
            <input
              className="input"
              type="number"
              min={0}
              max={100}
              step={1}
              style={{ width: 62, padding: "6px 8px" }}
              value={minShare || ""}
              onChange={(e) => setMinShare(Math.max(0, Number(e.target.value) || 0))}
            />
            <span className="faint">%</span>
          </label>
          <button
            className="btn btn-sm btn-ghost"
            onClick={copyTable}
            disabled={rows.length === 0}
            title={t("report_copy")}
          >
            {t("report_copy")}
          </button>
          <button className="btn btn-sm btn-ghost" onClick={exportCsv} disabled={rows.length === 0}>
            <Icon name="download" size={14} /> {t("sec_export")}
          </button>
        </div>
      </div>
      {hint ? (
        <div className="faint" style={{ fontSize: 12, marginTop: 4 }}>{hint}</div>
      ) : null}
      {section.rows.length === 0 ? (
        <div className="faint" style={{ marginTop: 10 }}>{t("analytics_no_group_data")}</div>
      ) : (
        <>
          <div
            className="table-wrap"
            style={{
              maxHeight: fullHeight ? "calc(100vh - 330px)" : 460,
              marginTop: 10,
            }}
          >
            <table className="grid" style={{ width: "100%" }}>
              <thead>
                <tr>
                  <th style={{ width: 34, textAlign: "right" }}>#</th>
                  <SortTh label={t("col_name")} onClick={() => setSort("name")} arrow={arrow("name")} />
                  <SortTh label={t("analytics_rows_share")} onClick={() => setSort("share")} arrow={arrow("share")} />
                  <SortTh label={t("common_rows")} onClick={() => setSort("rows")} arrow={arrow("rows")} />
                  <SortTh label={t("common_declarations")} onClick={() => setSort("docs")} arrow={arrow("docs")} />
                  <th style={{ width: 96 }}>{t("analytics_cumulative_share")}</th>
                  <SortTh label={t("analytics_value_by_currency")} onClick={() => setSort("value")} arrow={arrow("value")} />
                  <SortTh label={t("analytics_net_weight")} onClick={() => setSort("net_kg")} arrow={arrow("net_kg")} />
                  <SortTh label={t("analytics_value_per_weight")} onClick={() => setSort("vpk")} arrow={arrow("vpk")} />
                </tr>
              </thead>
              <tbody>
                {rows.map((row, i) => {
                  const share = safeRowShare(row, overview.row_count, valueCurrency !== null);
                  return (
                    <tr
                      key={`${row.label}-${i}`}
                      onClick={() => row.filter_action && onRow?.(row.filter_action)}
                      style={{ cursor: row.filter_action && onRow ? "pointer" : "default" }}
                    >
                      <td style={{ textAlign: "right", color: "var(--text-faint)" }}>{i + 1}</td>
                      <td title={row.label} style={{ maxWidth: 320 }}>{row.label || "—"}</td>
                      <td>
                        <div className="row" style={{ gap: 8, alignItems: "center" }}>
                          <div className="bar-track" style={{ width: 66 }}>
                            <div
                              className="bar-fill"
                              style={{ width: `${(share / maxShare) * 100}%` }}
                            />
                          </div>
                          <span className="faint">{formatPercent(share)}</span>
                        </div>
                      </td>
                      <td>{formatInt(row.rows)}</td>
                      <td>{formatInt(row.declarations)}</td>
                      <td className="faint">{formatPercent(cumulativeByIndex[i], 0)}</td>
                      <td>
                        <CurrencySummary measures={row.measures} legacyUsd={row.total_value_usd} />
                      </td>
                      <td>
                        {safeNetWeightKg(row, legacyRawKg) !== null ? (
                          `${formatInt(safeNetWeightKg(row, legacyRawKg) ?? 0)} kg`
                        ) : (
                          <WeightSummary totals={row.measures.net_weight_totals} />
                        )}
                      </td>
                      <td>
                        <ValuePerWeightSummary
                          measures={row.measures}
                          legacyUsdPerKg={legacyRawKg ? row.avg_value_per_net_kg : undefined}
                        />
                      </td>
                    </tr>
                  );
                })}
              </tbody>
              <tfoot>
                <tr className="month-total-row">
                  <td />
                  <td>
                    <strong>{t("analytics_total")}</strong>
                    <span className="faint" style={{ marginLeft: 6 }}>
                      {formatInt(rows.length)}
                    </span>
                  </td>
                  <td>
                    <strong>{formatPercent(visibleShare, 0)}</strong>
                  </td>
                  <td>
                    <strong>{formatInt(visibleRows)}</strong>
                  </td>
                  <td>
                    <strong>{formatInt(visibleDocs)}</strong>
                  </td>
                  <td colSpan={4} />
                </tr>
              </tfoot>
            </table>
          </div>
          <div className="faint" style={{ fontSize: 12, marginTop: 8 }}>
            {t("sec_shown")}: {formatInt(rows.length)}
            {" · "}
            {t("sec_concentration", {
              n: formatInt(rows.length),
              pct: formatPercent(visibleShare, 0),
            })}
            {capped ? ` · ${t("sec_capped")}` : ""}
          </div>
        </>
      )}
    </div>
  );
}

function SortTh({ label, onClick, arrow }: { label: string; onClick: () => void; arrow: string }) {
  const ariaSort = arrow.includes("▲")
    ? "ascending"
    : arrow.includes("▼")
      ? "descending"
      : "none";
  return (
    <th aria-sort={ariaSort}>
      <button className="sort-button" type="button" onClick={onClick}>
        {label}<span aria-hidden="true">{arrow}</span>
      </button>
    </th>
  );
}

function SectionLimitBar({ value, onChange }: { value: number; onChange: (n: number) => void }) {
  const { t } = useI18n();
  return (
    <div className="row" style={{ gap: 8 }}>
      <span className="field-label" style={{ margin: 0 }}>{t("sec_show_top")}:</span>
      {[50, 200, 500].map((n) => (
        <button
          key={n}
          className={`btn btn-sm ${value === n ? "" : "btn-ghost"}`}
          onClick={() => onChange(n)}
        >
          {n}
        </button>
      ))}
    </div>
  );
}

// The constraints currently narrowing the analytics, as removable chips — so a
// month/company drill is always visible and reversible.
// In-page query editor so the user never has to leave Analytics to change what
// is being analyzed: a full-text box plus the same direct filters as Search,
// applied in one step to the shared applied-query.
function AnalyticsSearchBar() {
  const { t } = useI18n();
  const { query, applyQuery } = useQueryStore();
  const [text, setText] = useState(query.text);
  const [filters, setFilters] = useState<Filters>(query.filters);
  const [showFilters, setShowFilters] = useState(false);

  // Reflect changes made elsewhere (chip removal, month drill, undo, clear-all).
  useEffect(() => {
    setText(query.text);
    setFilters(query.filters);
  }, [query]);

  const activeCount = FILTER_FIELDS.filter((f) => query.filters[f.key].trim()).length;
  const run = () => applyQuery({ ...query, text: text.trim(), filters });

  return (
    <div className="panel panel-pad analytics-searchbar">
      <div className="row" style={{ gap: 8 }}>
        <input
          className="input grow"
          value={text}
          placeholder={t("analytics_search_placeholder")}
          aria-label={t("analytics_search_placeholder")}
          onChange={(event) => setText(event.target.value)}
          onKeyDown={(event) => event.key === "Enter" && run()}
        />
        <button className="btn btn-primary" onClick={run}>
          <Icon name="analytics" size={15} /> {t("analytics_analyze")}
        </button>
        <button
          className={`btn ${showFilters ? "" : "btn-ghost"}`}
          onClick={() => setShowFilters((value) => !value)}
          aria-expanded={showFilters}
        >
          <Icon name="filter" size={15} /> {t("search_filters")}
          {activeCount > 0 ? ` (${activeCount})` : ""}
        </button>
      </div>
      {showFilters ? (
        <div className="filters-grid" style={{ marginTop: 10 }}>
          {FILTER_FIELDS.map((field) => (
            <div key={field.key}>
              <label className="field-label">{t(field.labelKey)}</label>
              <input
                className="input"
                value={filters[field.key]}
                onChange={(event) =>
                  setFilters((prev) => ({ ...prev, [field.key]: event.target.value }))
                }
                onKeyDown={(event) => event.key === "Enter" && run()}
              />
            </div>
          ))}
          <div className="row" style={{ alignItems: "flex-end" }}>
            <button className="btn btn-primary btn-sm" onClick={run}>
              {t("common_apply")}
            </button>
          </div>
        </div>
      ) : null}
    </div>
  );
}

function QueryChips({
  query,
  onClearText,
  onClearFilter,
  onClearAdvanced,
  onClearAll,
  onUndo,
  canUndo,
}: {
  query: Query;
  onClearText: () => void;
  onClearFilter: (key: keyof Filters) => void;
  onClearAdvanced: () => void;
  onClearAll: () => void;
  onUndo: () => void;
  canUndo: boolean;
}) {
  const { t } = useI18n();
  const filterEntries = (Object.entries(query.filters) as [keyof Filters, string][]).filter(
    ([, value]) => value.trim(),
  );
  const hasAny = !!query.text.trim() || filterEntries.length > 0 || !!query.advanced;
  if (!hasAny) return null;

  const label = (key: keyof Filters): string => {
    const map: Partial<Record<keyof Filters, string>> = {
      year: t("common_year"),
      edrpou: "EDRPOU",
      product_code: "HS",
      recipient: t("sec_recipients"),
      sender: t("sec_senders"),
      trademark: t("sec_trademarks"),
      origin_country: t("analytics_origin_countries"),
      dispatch_country: t("sec_dispatch_countries"),
      trade_country: t("sec_trade_countries"),
    };
    return map[key] ?? String(key);
  };

  return (
    <div className="panel panel-pad row wrap" style={{ gap: 8, alignItems: "center" }}>
      <span className="field-label" style={{ margin: 0 }}>{t("filter_active")}:</span>
      {query.text.trim() ? (
        <span className="chip">
          “{query.text.trim()}” <button onClick={onClearText}>×</button>
        </span>
      ) : null}
      {filterEntries.map(([key, value]) => (
        <span key={key} className="chip">
          {label(key)}: {value} <button onClick={() => onClearFilter(key)}>×</button>
        </span>
      ))}
      {query.advanced ? (
        <span className="chip">
          {t("search_advanced")} <button onClick={onClearAdvanced}>×</button>
        </span>
      ) : null}
      <div className="grow" />
      {canUndo ? (
        <button className="btn btn-sm btn-ghost" onClick={onUndo}>{t("common_undo")}</button>
      ) : null}
      <button className="btn btn-sm btn-ghost" onClick={onClearAll}>{t("filter_clear_all")}</button>
    </div>
  );
}

const SECTION_TITLE_KEYS: Record<string, MessageKey> = {
  recipients: "sec_recipients",
  senders: "sec_senders",
  edrpou: "sec_edrpou",
  product_codes: "sec_product_codes",
  trademarks: "sec_trademarks",
  product_groups: "sec_product_groups",
  origin_countries: "sec_origin_countries",
  dispatch_countries: "sec_dispatch_countries",
  trade_countries: "sec_trade_countries",
};

function sectionTitle(kind: string, t: (key: MessageKey) => string): string {
  const key = SECTION_TITLE_KEYS[kind];
  return key ? t(key) : kind;
}

const DIMS: PivotDim[] = [
  "recipient",
  "sender",
  "edrpou",
  "product_code",
  "trademark",
  "origin_country",
  "dispatch_country",
  "trade_country",
  "month",
  "year",
];

const METRIC_IDS: PivotMetric[] = ["value", "rows", "net_kg"];

function pivotDimLabel(d: PivotDim, t: (key: MessageKey) => string): string {
  const map: Record<PivotDim, string> = {
    recipient: t("sec_recipients"),
    sender: t("sec_senders"),
    edrpou: "EDRPOU",
    product_code: "HS",
    trademark: t("sec_trademarks"),
    origin_country: t("analytics_origin_countries"),
    dispatch_country: t("sec_dispatch_countries"),
    trade_country: t("sec_trade_countries"),
    month: t("analytics_month"),
    year: t("common_year"),
  };
  return map[d];
}

function pivotMetricLabel(m: PivotMetric, t: (key: MessageKey) => string): string {
  return m === "value"
    ? `${t("common_value")} USD`
    : m === "rows"
      ? t("common_rows")
      : t("common_net_kg");
}

function PivotPanel() {
  const { t } = useI18n();
  const { query } = useQueryStore();
  const [rowDim, setRowDim] = useState<PivotDim>("recipient");
  const [colDim, setColDim] = useState<PivotDim>("origin_country");
  const [metric, setMetric] = useState<PivotMetric>("value");
  const [result, setResult] = useState<PivotResult | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const run = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      setResult(await api.pivot(query, rowDim, colDim, metric, 20, 12));
    } catch (err) {
      setError((err as ApiError)?.message ?? t("pivot_failed"));
    } finally {
      setLoading(false);
    }
  }, [query, rowDim, colDim, metric]);

  // A pivot must never present numbers from a query the user already changed:
  // drop the stale table the moment the query, dimensions, or metric differ.
  const builtFor = useRef<string | null>(null);
  const buildKey = `${JSON.stringify(query)}|${rowDim}|${colDim}|${metric}`;
  useEffect(() => {
    if (result && builtFor.current !== buildKey) {
      setResult(null);
    }
  }, [buildKey, result]);
  const runAndRemember = () => {
    builtFor.current = buildKey;
    run();
  };

  const fmt = (v: number) => (metric === "value" ? formatMoney(v) : formatInt(v));

  return (
    <div className="stack">
      <div className="panel panel-pad toolbar" style={{ alignItems: "flex-end" }}>
        <div>
          <label className="field-label">{t("pivot_rows")}</label>
          <select className="select" style={{ width: 170 }} value={rowDim} onChange={(e) => setRowDim(e.target.value as PivotDim)}>
            {DIMS.map((d) => (
              <option key={d} value={d}>{pivotDimLabel(d, t)}</option>
            ))}
          </select>
        </div>
        <div>
          <label className="field-label">{t("pivot_cols")}</label>
          <select className="select" style={{ width: 170 }} value={colDim} onChange={(e) => setColDim(e.target.value as PivotDim)}>
            {DIMS.map((d) => (
              <option key={d} value={d}>{pivotDimLabel(d, t)}</option>
            ))}
          </select>
        </div>
        <div>
          <label className="field-label">{t("analytics_metric")}</label>
          <select className="select" style={{ width: 150 }} value={metric} onChange={(e) => setMetric(e.target.value as PivotMetric)}>
            {METRIC_IDS.map((m) => (
              <option key={m} value={m}>{pivotMetricLabel(m, t)}</option>
            ))}
          </select>
        </div>
        <button className="btn btn-primary btn-sm" onClick={runAndRemember} disabled={loading}>
          {t("pivot_build")}
        </button>
      </div>

      {error ? <Banner>{error}</Banner> : null}
      {loading ? <Loading /> : null}

      {result && !result.cells ? (
        <Banner variant="warn">{t("pivot_mixed_units")}</Banner>
      ) : null}
      {result && result.cells ? (
        <div className="table-wrap">
          <table className="grid">
            <thead>
              <tr>
                <th style={{ position: "sticky", left: 0, zIndex: 3 }}>
                  {pivotDimLabel(rowDim, t)} \ {pivotDimLabel(colDim, t)}
                </th>
                {result.col_labels.map((c) => (
                  <th key={c} title={c}>{c || "—"}</th>
                ))}
                <th>{t("common_total")}</th>
              </tr>
            </thead>
            <tbody>
              {result.row_labels.map((rowLabel, ri) => (
                <tr key={rowLabel} style={{ cursor: "default" }}>
                  <td style={{ position: "sticky", left: 0, background: "var(--panel-solid)" }} title={rowLabel}>
                    {rowLabel || "—"}
                  </td>
                  {(result.cells?.[ri] ?? []).map((cell, ci) => (
                    <td key={ci}>{cell ? fmt(cell) : ""}</td>
                  ))}
                  <td><strong>{fmt(result.row_totals?.[ri] ?? 0)}</strong></td>
                </tr>
              ))}
              <tr style={{ cursor: "default" }}>
                <td style={{ position: "sticky", left: 0, background: "var(--panel-solid)" }}>
                  <strong>{t("common_total")}</strong>
                </td>
                {(result.col_totals ?? []).map((c, ci) => (
                  <td key={ci}><strong>{fmt(c)}</strong></td>
                ))}
                <td><strong>{fmt(result.grand_total ?? 0)}</strong></td>
              </tr>
            </tbody>
          </table>
        </div>
      ) : null}
    </div>
  );
}

// A clean working summary of the current query: headline numbers, top
// companies/goods/countries/prices, and a printable (Save as PDF) HTML report.
function ReportPanel() {
  const { t } = useI18n();
  const { query } = useQueryStore();
  const { toast } = useStore();
  const [analytics, setAnalytics] = useState<Analytics | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const queryKey = useMemo(() => JSON.stringify(query), [query]);

  useEffect(() => {
    let alive = true;
    setLoading(true);
    setError(null);
    api
      .analytics(query, null, 10, 12, "sqlite", true)
      .then((res) => alive && setAnalytics(res.data))
      .catch((err) => alive && setError((err as ApiError)?.message ?? t("report_failed")))
      .finally(() => alive && setLoading(false));
    return () => {
      alive = false;
    };
    // Re-run only when the query changes.
  }, [queryKey]); // eslint-disable-line react-hooks/exhaustive-deps

  const titleOf = (kind: string) => sectionTitle(kind, t);

  const printReport = () => {
    if (!analytics) return;
    const win = window.open("", "_blank");
    if (!win) {
      toast(t("report_popup_blocked"), "error");
      return;
    }
    win.document.write(buildReportHtml(analytics, query, titleOf, t));
    win.document.close();
    win.focus();
    setTimeout(() => win.print(), 300);
  };

  const copyReport = async () => {
    if (!analytics) return;
    const ok = await copyText(buildReportText(analytics, query, t));
    toast(ok ? t("report_copied") : t("report_copy_failed"), ok ? "success" : "error");
  };

  if (loading && !analytics) return <Loading label={t("common_loading")} />;
  if (error) return <Banner>{error}</Banner>;
  if (!analytics) return null;
  const o = analytics.overview;

  return (
    <div className="stack">
      <div className="panel panel-pad row wrap" style={{ justifyContent: "space-between", gap: 10 }}>
        <div>
          <div className="section-title" style={{ margin: 0 }}>{t("report_title")}</div>
          <div className="faint" style={{ fontSize: 12, marginTop: 4 }}>{queryLabel(query, t)}</div>
        </div>
        <div className="row" style={{ gap: 8 }}>
          <button className="btn btn-sm" onClick={copyReport}>
            {t("report_copy")}
          </button>
          <button className="btn btn-sm btn-primary" onClick={printReport}>
            <Icon name="export" size={14} /> {t("report_print")}
          </button>
        </div>
      </div>

      <div className="stat-grid">
        <StatCard label={t("common_rows")} value={formatInt(o.row_count)} />
        {/* Datasets without document numbers get no permanent "0" card. */}
        {o.declaration_count > 0 ? (
          <StatCard
            label={t("common_declarations")}
            value={formatInt(o.declaration_count)}
          />
        ) : null}
        <StatCard
          label={t("analytics_value_by_currency")}
          value={<CurrencySummary measures={o.measures} legacyUsd={o.total_value_usd} />}
        />
        <StatCard
          label={t("analytics_net_weight")}
          value={<WeightSummary totals={o.measures.net_weight_totals} />}
        />
        <StatCard
          label={t("analytics_value_per_weight")}
          value={<ValuePerWeightSummary measures={o.measures} />}
        />
        <StatCard label={t("analytics_companies")} value={formatInt(o.distinct_edrpou)} />
      </div>

      <div className="grid-2">
        <ReportSection
          title={t("analytics_companies")}
          sections={analytics.company_sections}
          overview={o}
          titleOf={titleOf}
        />
        <ReportSection
          title={t("analytics_products")}
          sections={analytics.product_sections}
          overview={o}
          titleOf={titleOf}
        />
      </div>
      <div className="grid-2">
        <ReportSection
          title={t("analytics_countries")}
          sections={analytics.country_sections}
          overview={o}
          titleOf={titleOf}
        />
        <div className="panel panel-pad">
          <div className="section-title">{t("analytics_prices")}</div>
          <PriceTable metrics={analytics.price_sections} />
        </div>
      </div>
    </div>
  );
}

function ReportSection({
  title,
  sections,
  overview,
  titleOf,
}: {
  title: string;
  sections: AnalyticsSection[];
  overview: AnalyticsOverview;
  titleOf: (kind: string) => string;
}) {
  const { t } = useI18n();
  const withRows = sections.filter((s) => s.rows.length > 0);
  return (
    <div className="panel panel-pad">
      <div className="section-title">{title}</div>
      {withRows.length === 0 ? (
        <div className="faint">{t("analytics_no_group_data")}</div>
      ) : (
        withRows.slice(0, 2).map((section) => (
          <div key={section.kind} style={{ marginBottom: 10 }}>
            <div className="faint" style={{ fontSize: 12, marginBottom: 6 }}>
              {titleOf(section.kind)}
            </div>
            {section.rows.slice(0, 5).map((row, i) => (
              <div
                key={`${row.label}-${i}`}
                className="row"
                style={{ justifyContent: "space-between", padding: "3px 0", gap: 12 }}
              >
                <span
                  title={row.label}
                  style={{
                    overflow: "hidden",
                    textOverflow: "ellipsis",
                    whiteSpace: "nowrap",
                    maxWidth: "58%",
                  }}
                >
                  {row.label || "—"}
                </span>
                <span className="report-row-measures">
                  <CurrencySummary measures={row.measures} legacyUsd={row.total_value_usd} />
                  <span className="faint">
                    {formatPercent(
                      safeRowShare(
                        row,
                        overview.row_count,
                        compatibleCurrencyTotal(overview) !== null,
                      ),
                    )}
                  </span>
                </span>
              </div>
            ))}
          </div>
        ))
      )}
    </div>
  );
}

// Compares the current query with another product, company, or the previous
// year, side by side, with a signed difference table.
function ComparePanel() {
  const { t } = useI18n();
  const { query } = useQueryStore();
  const [withText, setWithText] = useState("");
  const [prevYear, setPrevYear] = useState(false);
  const [left, setLeft] = useState<Analytics | null>(null);
  const [right, setRight] = useState<Analytics | null>(null);
  const [leftQuery, setLeftQuery] = useState<Query | null>(null);
  const [rightQuery, setRightQuery] = useState<Query | null>(null);
  const [swapped, setSwapped] = useState(false);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const hasYear = !!query.filters.year?.trim();

  // A comparison snapshot must never masquerade as the live query: when the
  // active query changes after a run, the old result disappears instead of
  // being relabeled "Current".
  const queryKey = JSON.stringify(query);
  useEffect(() => {
    if (leftQuery && JSON.stringify(leftQuery) !== queryKey) {
      setLeft(null);
      setRight(null);
      setLeftQuery(null);
      setRightQuery(null);
      setSwapped(false);
    }
  }, [queryKey, leftQuery]);

  const run = async () => {
    const other: Query = { ...query, filters: { ...query.filters } };
    if (withText.trim()) other.text = withText.trim();
    if (prevYear && hasYear) {
      const year = parseInt(query.filters.year.trim(), 10);
      if (Number.isNaN(year)) {
        setError(t("compare_bad_year"));
        return;
      }
      other.filters = { ...other.filters, year: String(year - 1) };
    }
    if (!withText.trim() && !(prevYear && hasYear)) {
      setError(t("compare_empty"));
      return;
    }
    setLoading(true);
    setError(null);
    try {
      const [l, r] = await Promise.all([
        api.analytics(query, null, 10, 12, "auto"),
        api.analytics(other, null, 10, 12, "auto"),
      ]);
      setLeft(l.data);
      setRight(r.data);
      setLeftQuery(query);
      setRightQuery(other);
      setSwapped(false);
    } catch (err) {
      setError((err as ApiError)?.message ?? t("compare_failed"));
    } finally {
      setLoading(false);
    }
  };

  return (
    <div className="stack">
      <div className="panel panel-pad stack" style={{ gap: 12 }}>
        <div className="faint" style={{ fontSize: 12 }}>{t("compare_hint")}</div>
        <div className="row wrap" style={{ gap: 10 }}>
          <input
            className="input"
            style={{ maxWidth: 280 }}
            placeholder={t("compare_with")}
            value={withText}
            onChange={(e) => setWithText(e.target.value)}
            onKeyDown={(e) => e.key === "Enter" && run()}
          />
          {hasYear ? (
            <label className="row" style={{ gap: 6, cursor: "pointer" }}>
              <input
                type="checkbox"
                checked={prevYear}
                onChange={(e) => setPrevYear(e.target.checked)}
              />
              <span className="faint">{t("compare_prev_year")}</span>
            </label>
          ) : null}
          <button className="btn btn-primary btn-sm" onClick={run} disabled={loading}>
            {t("compare_run")}
          </button>
        </div>
      </div>

      {error ? <Banner>{error}</Banner> : null}
      {loading ? <Loading /> : null}

      {left && right && leftQuery && rightQuery
        ? (() => {
            // `swapped` flips only the display order so the stored left query
            // stays identified with the live query (the snapshot guard depends
            // on it); the diff sign follows the displayed sides.
            const dL = swapped
              ? { data: right, q: rightQuery }
              : { data: left, q: leftQuery };
            const dR = swapped
              ? { data: left, q: leftQuery }
              : { data: right, q: rightQuery };
            return (
              <>
                <div className="row" style={{ justifyContent: "flex-end" }}>
                  <button className="btn btn-sm btn-ghost" onClick={() => setSwapped((s) => !s)}>
                    <Icon name="refresh" size={14} /> {t("compare_swap")}
                  </button>
                </div>
                <div className="grid-2">
                  <CompareCard title={t("compare_current")} label={queryLabel(dL.q, t)} data={dL.data} />
                  <CompareCard title={t("compare_other")} label={queryLabel(dR.q, t)} data={dR.data} />
                </div>
                <div className="panel panel-pad">
                  <div className="section-title">{t("compare_difference")}</div>
                  <div className="table-wrap" style={{ maxHeight: "none" }}>
                    <table className="grid" style={{ width: "100%" }}>
                      <thead>
                        <tr>
                          <th>{t("analytics_metric")}</th>
                          <th>{t("compare_current")}</th>
                          <th>{t("compare_other")}</th>
                          <th>Δ</th>
                        </tr>
                      </thead>
                      <tbody>
                        <CompareRow label={t("common_rows")} a={dL.data.overview.row_count} b={dR.data.overview.row_count} kind="int" />
                        <CompareRow label={t("common_declarations")} a={dL.data.overview.declaration_count} b={dR.data.overview.declaration_count} kind="int" />
                        <MeasureCompareRows left={dL.data.overview} right={dR.data.overview} />
                        <CompareRow label={t("analytics_companies")} a={dL.data.overview.distinct_edrpou} b={dR.data.overview.distinct_edrpou} kind="int" />
                        <CompareRow label={t("analytics_product_codes")} a={dL.data.overview.distinct_product_codes} b={dR.data.overview.distinct_product_codes} kind="int" />
                        <CompareRow label={t("analytics_origin_countries")} a={dL.data.overview.distinct_origin_countries} b={dR.data.overview.distinct_origin_countries} kind="int" />
                      </tbody>
                    </table>
                  </div>
                </div>
              </>
            );
          })()
        : null}
    </div>
  );
}

function CompareCard({ title, label, data }: { title: string; label: string; data: Analytics }) {
  const { t } = useI18n();
  const o = data.overview;
  const line = (name: string, value: ReactNode) => (
    <div className="row" style={{ justifyContent: "space-between", padding: "3px 0" }}>
      <span className="faint">{name}</span>
      <strong>{value}</strong>
    </div>
  );
  return (
    <div className="panel panel-pad">
      <div className="section-title" style={{ marginBottom: 4 }}>{title}</div>
      <div className="faint" style={{ fontSize: 12, marginBottom: 10 }} title={label}>
        {label}
      </div>
      {line(t("common_rows"), formatInt(o.row_count))}
      {line(
        t("analytics_value_by_currency"),
        <CurrencySummary measures={o.measures} legacyUsd={o.total_value_usd} />,
      )}
      {line(t("analytics_net_weight"), <WeightSummary totals={o.measures.net_weight_totals} />)}
      {line(
        t("analytics_value_per_weight"),
        <ValuePerWeightSummary measures={o.measures} />,
      )}
    </div>
  );
}

function MeasureCompareRows({
  left,
  right,
}: {
  left: AnalyticsOverview;
  right: AnalyticsOverview;
}) {
  const { t } = useI18n();
  const leftValue = compatibleCurrencyTotal(left);
  const rightValue = compatibleCurrencyTotal(right);
  const leftNet = safeNetWeightKg(left, rawNetWeightIsKg(left.measures));
  const rightNet = safeNetWeightKg(right, rawNetWeightIsKg(right.measures));
  const leftRatio = safeValuePerNetWeight(left, rawNetWeightIsKg(left.measures));
  const rightRatio = safeValuePerNetWeight(right, rawNetWeightIsKg(right.measures));
  const comparableValue =
    leftValue && rightValue && leftValue.currency === rightValue.currency
      ? { currency: leftValue.currency, a: leftValue.total_value, b: rightValue.total_value }
      : null;
  const comparableRatio =
    leftRatio &&
    rightRatio &&
    leftRatio.currency === rightRatio.currency &&
    leftRatio.normalized_weight_unit === rightRatio.normalized_weight_unit &&
    leftRatio.value_per_weight !== null &&
    rightRatio.value_per_weight !== null
      ? {
          unit: `${leftRatio.currency}/${leftRatio.normalized_weight_unit}`,
          a: leftRatio.value_per_weight,
          b: rightRatio.value_per_weight,
        }
      : null;

  return (
    <>
      {comparableValue ? (
        <CompareRow
          label={`${t("common_value")} ${comparableValue.currency}`}
          a={comparableValue.a}
          b={comparableValue.b}
          kind="money"
        />
      ) : null}
      {leftNet !== null && rightNet !== null ? (
        <CompareRow
          label={`${t("analytics_net_weight")} (kg)`}
          a={leftNet}
          b={rightNet}
          kind="int"
        />
      ) : null}
      {comparableRatio ? (
        <CompareRow
          label={`${t("analytics_value_per_weight")} (${comparableRatio.unit})`}
          a={comparableRatio.a}
          b={comparableRatio.b}
          kind="money"
        />
      ) : null}
    </>
  );
}

function CompareRow({
  label,
  a,
  b,
  kind,
}: {
  label: string;
  a: number;
  b: number;
  kind: "int" | "money";
}) {
  const fmt = kind === "money" ? formatMoney : formatInt;
  const delta = b - a;
  const pct = Math.abs(a) > 1e-9 ? (delta / a) * 100 : null;
  const color =
    delta === 0 ? "var(--text-faint)" : delta > 0 ? "var(--success-text)" : "var(--danger-text)";
  return (
    <tr style={{ cursor: "default" }}>
      <td>{label}</td>
      <td>{fmt(a)}</td>
      <td>{fmt(b)}</td>
      <td style={{ color }}>
        {delta >= 0 ? "+" : ""}
        {fmt(delta)}
        {pct !== null ? ` (${delta >= 0 ? "+" : ""}${formatPercent(pct, 0)})` : ""}
      </td>
    </tr>
  );
}
