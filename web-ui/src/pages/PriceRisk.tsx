import { useCallback, useEffect, useState } from "react";

import { api, ApiError } from "../api/client";
import type { RiskConfidence, RiskExclusions, Undervaluation } from "../api/types";
import { StatCard } from "../components/analytics";
import { RecordDrawer } from "../components/RecordDrawer";
import { Banner, EmptyState, Loading } from "../components/ui";
import { useI18n, type MessageKey } from "../lib/i18n";
import { formatCompact, formatInt, formatMoney, formatPercent } from "../lib/format";
import { navigate } from "../lib/router";
import { useQueryStore } from "../state/query";
import { useStore } from "../state/store";

const CONFIDENCE_KEYS: Record<RiskConfidence, MessageKey> = {
  high: "risk_confidence_high",
  medium: "risk_confidence_medium",
  low: "risk_confidence_low",
};

function cohortLabel(cohort: Undervaluation["rows"][number]["cohort"]): string {
  return [
    cohort.period,
    `${cohort.currency}/${cohort.weight_unit}`,
    cohort.brand,
    cohort.country,
  ]
    .filter(Boolean)
    .join(" · ");
}

const EXCLUSION_KEYS: Array<[keyof RiskExclusions, MessageKey]> = [
  ["missing_product_code", "risk_excl_product"],
  ["missing_period", "risk_excl_period"],
  ["missing_currency", "risk_excl_currency"],
  ["missing_weight_unit", "risk_excl_unit"],
  ["invalid_value", "risk_excl_value"],
  ["invalid_weight", "risk_excl_weight"],
  ["insufficient_cohort", "risk_excl_cohort"],
];

export function RiskResults({
  result,
  onOpenRecord,
  onOpenCompany,
}: {
  result: Undervaluation;
  onOpenRecord: (id: number) => void;
  onOpenCompany: (edrpou: string) => void;
}) {
  const { t } = useI18n();
  if (!result.available) {
    return (
      <div className="panel panel-pad risk-unavailable" role="status">
        <div className="section-title">{t("risk_unavailable_title")}</div>
        <p className="muted">{t("risk_unavailable_hint")}</p>
        <ul className="risk-limitations">
          {result.limitations.map((item) => (
            <li key={item.code}>{item.message}</li>
          ))}
        </ul>
      </div>
    );
  }

  const exclusions = EXCLUSION_KEYS.map(
    ([field, key]) => [key, result.exclusions[field]] as [MessageKey, number],
  ).filter(([, count]) => count > 0);

  return (
    <>
      <div className="stat-grid">
        <StatCard label={t("risk_flagged_rows")} value={formatInt(result.flagged_rows)} />
        <StatCard label={t("risk_cohorts")} value={formatInt(result.checked_cohorts)} />
        <StatCard
          label={t("risk_rows_evaluated")}
          value={formatInt(result.evaluated_rows)}
          hint={`${formatInt(result.eligible_rows)} ${t("risk_eligible_hint")}`}
        />
        <StatCard
          label={t("risk_estimated_gap")}
          value={
            result.currency_totals.length === 1
              ? `${formatCompact(result.currency_totals[0].estimated_gap)} ${result.currency_totals[0].currency}`
              : `${formatInt(result.currency_totals.length)} ${t("risk_currencies")}`
          }
          hint={t("risk_gap_hint")}
        />
      </div>

      {result.currency_totals.length > 0 ? (
        <div className="panel panel-pad risk-currency-totals">
          {result.currency_totals.map((total) => (
            <div key={total.currency}>
              <strong>
                {total.currency} {t("risk_totals_suffix")}
              </strong>
              <span>
                {formatInt(total.flagged_rows)} {t("risk_flagged_short")} ·{" "}
                {t("common_value").toLowerCase()} {formatMoney(total.flagged_value)} ·{" "}
                {t("risk_gap_short")} {formatMoney(total.estimated_gap)}
              </span>
            </div>
          ))}
        </div>
      ) : null}

      <div className="panel panel-pad risk-contract">
        <div>
          <strong>{t("risk_how_title")}</strong>
          <span className="muted">{t("risk_how_hint")}</span>
        </div>
        <div>
          <strong>
            {formatInt(result.contract.min_samples)} {t("risk_min_samples_label")}
          </strong>
          <span className="muted">
            {t("risk_thresholds_before")}{" "}
            {formatPercent(result.contract.max_median_ratio * 100, 0)}{" "}
            {t("risk_thresholds_after")} {result.contract.iqr_multiplier.toFixed(1)}
            ×IQR
          </span>
        </div>
      </div>

      {exclusions.length > 0 ? (
        <details className="panel panel-pad risk-exclusions">
          <summary>
            {formatInt(exclusions.reduce((sum, [, count]) => sum + count, 0))}{" "}
            {t("risk_excluded_summary")}
          </summary>
          <div className="risk-exclusion-grid">
            {exclusions.map(([key, count]) => (
              <div key={key}>
                <span>{t(key)}</span>
                <strong>{formatInt(count)}</strong>
              </div>
            ))}
          </div>
        </details>
      ) : null}

      {result.rows.length === 0 ? (
        <EmptyState
          icon="check"
          title={t("risk_no_signals_title")}
          hint={t("risk_no_signals_hint")}
        />
      ) : (
        <div className="table-wrap">
          <table className="grid risk-table">
            <thead>
              <tr>
                <th>{t("risk_col_record")}</th>
                <th>{t("risk_col_product")}</th>
                <th>{t("risk_col_cohort")}</th>
                <th>{t("risk_col_observed")}</th>
                <th>{t("risk_col_median_iqr")}</th>
                <th>{t("risk_col_cutoff")}</th>
                <th>{t("risk_col_deviation")}</th>
                <th>{t("risk_col_confidence")}</th>
              </tr>
            </thead>
            <tbody>
              {result.rows.map((row) => (
                <tr key={row.id} onClick={() => onOpenRecord(row.id)}>
                  <td>
                    <strong>{row.declaration_number || `#${row.id}`}</strong>
                    <span className="faint risk-cell-note">
                      {row.declaration_date || t("risk_no_date")}
                    </span>
                    {row.edrpou ? (
                      <button
                        className="btn btn-ghost btn-sm risk-company-link"
                        onClick={(event) => {
                          event.stopPropagation();
                          onOpenCompany(row.edrpou);
                        }}
                      >
                        {row.edrpou}
                      </button>
                    ) : null}
                  </td>
                  <td title={row.description}>
                    <strong>{row.product_code}</strong>
                    <span className="faint risk-cell-note">
                      {row.description || t("risk_no_description")}
                    </span>
                  </td>
                  <td>
                    <strong>{cohortLabel(row.cohort)}</strong>
                    <span className="faint risk-cell-note">
                      n={formatInt(row.cohort.sample_count)}
                    </span>
                  </td>
                  <td>
                    <strong>{formatMoney(row.price_per_kg)}</strong>
                    <span className="faint risk-cell-note">
                      {row.cohort.currency}/{row.cohort.weight_unit}
                    </span>
                  </td>
                  <td>
                    <strong>{formatMoney(row.cohort.median)}</strong>
                    <span className="faint risk-cell-note">
                      {formatMoney(row.cohort.p25)}–{formatMoney(row.cohort.p75)}
                    </span>
                  </td>
                  <td>
                    <strong>{formatMoney(row.cohort.robust_cutoff)}</strong>
                  </td>
                  <td>
                    <strong>{formatPercent(row.deviation_percent)}</strong>
                    <span className="faint risk-cell-note">
                      {formatPercent(row.ratio * 100, 0)} {t("risk_of_median")}
                    </span>
                  </td>
                  <td>
                    <span className={`risk-confidence risk-confidence-${row.confidence}`}>
                      {t(CONFIDENCE_KEYS[row.confidence])}
                    </span>
                    <details
                      className="risk-row-details"
                      onClick={(event) => event.stopPropagation()}
                    >
                      <summary>{t("risk_why")}</summary>
                      <p>{row.reason}</p>
                      {row.limitations.length > 0 ? (
                        <ul className="risk-limitations">
                          {row.limitations.map((item) => (
                            <li key={item.code}>{item.message}</li>
                          ))}
                        </ul>
                      ) : null}
                    </details>
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}
    </>
  );
}

export function PriceRiskPage() {
  const { t } = useI18n();
  const { query, isEmpty } = useQueryStore();
  const { openCompany } = useStore();
  const [threshold, setThreshold] = useState(0.5);
  const [minSamples, setMinSamples] = useState(20);
  const [forceAll, setForceAll] = useState(false);
  const [result, setResult] = useState<Undervaluation | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [drawerId, setDrawerId] = useState<number | null>(null);

  // An unfiltered scan over the entire database is an explicit choice, exactly
  // like on the Analytics screen — never a side effect of opening the page.
  const shouldRun = !isEmpty || forceAll;

  const run = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      setResult(await api.undervaluation(query, threshold, minSamples, 200));
    } catch (err) {
      setError((err as ApiError)?.message ?? t("risk_failed"));
    } finally {
      setLoading(false);
    }
  }, [query, threshold, minSamples, t]);

  useEffect(() => {
    if (!shouldRun) return;
    run();
  }, [run, shouldRun]);

  if (!shouldRun) {
    return (
      <EmptyState
        icon="alert"
        title={t("risk_need_query")}
        action={
          <div className="row" style={{ gap: 10 }}>
            <button className="btn" onClick={() => navigate("search")}>
              {t("nav_search")}
            </button>
            <button className="btn btn-primary" onClick={() => setForceAll(true)}>
              {t("analytics_whole_db")}
            </button>
          </div>
        }
      />
    );
  }

  return (
    <div className="stack">
      <div className="panel panel-pad">
        <div className="row wrap" style={{ justifyContent: "space-between", gap: 10 }}>
          <div>
            <div className="section-title" style={{ margin: 0 }}>
              {t("nav_risk")}
            </div>
            <p className="muted risk-intro" style={{ margin: "6px 0 0" }}>
              {t("risk_intro")}
            </p>
          </div>
          <div className="row wrap" style={{ gap: 10, alignItems: "flex-end" }}>
            <div>
              <label className="field-label">{t("risk_threshold_label")}</label>
              <select
                className="select"
                value={threshold}
                onChange={(event) => setThreshold(Number(event.target.value))}
              >
                <option value={0.3}>30% · {t("risk_threshold_strict")}</option>
                <option value={0.5}>50% · {t("risk_threshold_balanced")}</option>
                <option value={0.7}>70% · {t("risk_threshold_broad")}</option>
              </select>
            </div>
            <div>
              <label className="field-label">{t("risk_min_cohort")}</label>
              <select
                className="select"
                value={minSamples}
                onChange={(event) => setMinSamples(Number(event.target.value))}
              >
                {[20, 30, 50, 100].map((count) => (
                  <option key={count} value={count}>
                    {count} {t("risk_records")}
                  </option>
                ))}
              </select>
            </div>
            <button className="btn btn-primary btn-sm" onClick={run} disabled={loading}>
              {loading ? t("risk_analyzing") : t("risk_analyze")}
            </button>
          </div>
        </div>
      </div>

      {error ? <Banner>{error}</Banner> : null}
      {loading && !result ? <Loading label={t("common_loading")} /> : null}
      {result ? (
        <RiskResults
          result={result}
          onOpenRecord={setDrawerId}
          onOpenCompany={openCompany}
        />
      ) : null}

      {drawerId !== null ? (
        <RecordDrawer id={drawerId} onClose={() => setDrawerId(null)} />
      ) : null}
    </div>
  );
}
