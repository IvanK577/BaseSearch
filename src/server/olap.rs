//! DuckDB projection gating. A projection is usable only when its generation
//! and fingerprint match the live SQLite records, schema, and semantic mapping.
//! Baseline totals are verified once per matching contract; stale, unreadable,
//! or untrusted states stay on SQLite.

#![cfg(feature = "duckdb-olap")]

use std::path::{Path, PathBuf};

use crate::db::{
    Analytics, AnalyticsCurrencyTotal, AnalyticsGroupRow, AnalyticsMeasures, AnalyticsMonthRow,
    AnalyticsOverview, AnalyticsPriceMetric, AnalyticsScope, AnalyticsSection,
    AnalyticsUsdCompatibility, AnalyticsValuePerWeight, AnalyticsWeightTotal, Db, Query,
};
use crate::duckdb_olap;
use crate::engines::{AnalyticsEngine, DuckDbAnalyticsEngine};

use super::error::ApiError;
use super::state::AppState;

/// Returns the projection path only when it is fresh (covers every imported
/// row) and trusted (its aggregates match SQLite). Otherwise `None`, and the
/// caller uses SQLite.
pub(crate) fn ready_projection(state: &AppState) -> Result<Option<PathBuf>, ApiError> {
    let path = duckdb_olap::default_projection_path(state.db_path());
    if !path.exists() {
        return Ok(None);
    }
    let Ok(meta) = duckdb_olap::read_projection_meta(&path) else {
        return Ok(None);
    };

    match duckdb_olap::projection_is_current(state.db_path(), &path) {
        Ok(true) => {}
        Ok(false) | Err(_) => return Ok(None),
    }

    // The fingerprint covers the source generation, schema, semantic mapping,
    // and projection contract, so cached trust cannot outlive any of them.
    let trust_key = format!("{}:{}", meta.source_fingerprint, meta.rollup_fingerprint);
    if let Some((fingerprint, trusted)) = state.projection_trust.lock().unwrap().as_ref()
        && *fingerprint == trust_key
    {
        return Ok(trusted.then(|| path.clone()));
    }

    let db = state.open_read()?;
    let trusted = match verify(&db, state.db_path(), &path) {
        Ok(trusted) => trusted,
        Err(_) => {
            eprintln!("[base-search] DuckDB projection verification failed; using SQLite.");
            return Ok(None);
        }
    };
    let trusted = trusted
        && matches!(
            duckdb_olap::projection_is_current(state.db_path(), &path),
            Ok(true)
        );
    *state.projection_trust.lock().unwrap() = Some((trust_key, trusted));
    if !trusted {
        eprintln!(
            "[base-search] DuckDB projection does not reproduce SQLite totals for this dataset; \
             using SQLite for analytics."
        );
    }
    Ok(trusted.then_some(path))
}

/// True when the fresh projection is trusted (used by the engine-status view).
pub(crate) fn projection_trusted(state: &AppState) -> Result<bool, ApiError> {
    Ok(ready_projection(state)?.is_some())
}

pub(crate) fn ready_engine(state: &AppState) -> Result<Option<DuckDbAnalyticsEngine>, ApiError> {
    Ok(ready_projection(state)?
        .map(|projection_path| DuckDbAnalyticsEngine::new(state.db_path(), projection_path)))
}

fn verify(db: &Db, sqlite_path: &Path, path: &Path) -> Result<bool, ApiError> {
    let query = Query::default();
    let engine = DuckDbAnalyticsEngine::new(sqlite_path, path);
    for scope in AnalyticsScope::ALL {
        let sqlite = db
            .analytics_scoped(&query, 20, Some(scope), 10)
            .map_err(|err| ApiError::internal("verify projection (sqlite)", err))?;
        let duck = engine
            .analytics(&query, 20, Some(scope), 10)
            .map_err(|err| ApiError::internal("verify projection (duckdb)", err))?;
        if !analytics_matches(&sqlite, &duck) {
            return Ok(false);
        }
    }
    Ok(true)
}

fn overview_matches(a: &AnalyticsOverview, b: &AnalyticsOverview) -> bool {
    a.row_count == b.row_count
        && a.declaration_count == b.declaration_count
        && a.distinct_senders == b.distinct_senders
        && a.distinct_recipients == b.distinct_recipients
        && a.distinct_edrpou == b.distinct_edrpou
        && a.distinct_trademarks == b.distinct_trademarks
        && a.distinct_product_codes == b.distinct_product_codes
        && a.distinct_origin_countries == b.distinct_origin_countries
        && a.distinct_dispatch_countries == b.distinct_dispatch_countries
        && a.distinct_trade_countries == b.distinct_trade_countries
        && close(a.total_quantity, b.total_quantity)
        && usd_compatibility_matches(a.compatible_usd.as_ref(), b.compatible_usd.as_ref())
        && measures_match(&a.measures, &b.measures)
}

fn analytics_matches(a: &Analytics, b: &Analytics) -> bool {
    overview_matches(&a.overview, &b.overview)
        && months_match(&a.months, &b.months)
        && sections_match(&a.company_sections, &b.company_sections)
        && sections_match(&a.product_sections, &b.product_sections)
        && sections_match(&a.country_sections, &b.country_sections)
        && prices_match(&a.price_sections, &b.price_sections)
}

fn months_match(a: &[AnalyticsMonthRow], b: &[AnalyticsMonthRow]) -> bool {
    a.len() == b.len()
        && a.iter().zip(b).all(|(a, b)| {
            a.month == b.month
                && a.rows == b.rows
                && a.declarations == b.declarations
                && usd_compatibility_matches(a.compatible_usd.as_ref(), b.compatible_usd.as_ref())
                && measures_match(&a.measures, &b.measures)
        })
}

fn sections_match(a: &[AnalyticsSection], b: &[AnalyticsSection]) -> bool {
    a.len() == b.len()
        && a.iter().all(|left_section| {
            b.iter()
                .find(|right_section| right_section.kind == left_section.kind)
                .is_some_and(|right_section| {
                    left_section.rows.len() == right_section.rows.len()
                        && left_section.rows.iter().all(|left_row| {
                            right_section
                                .rows
                                .iter()
                                .find(|right_row| right_row.label == left_row.label)
                                .is_some_and(|right_row| group_rows_match(left_row, right_row))
                        })
                })
        })
}

fn group_rows_match(a: &AnalyticsGroupRow, b: &AnalyticsGroupRow) -> bool {
    let compatible_share_matches = match (a.compatible_usd.as_ref(), b.compatible_usd.as_ref()) {
        (None, None) => true,
        (Some(_), Some(_)) => close(a.share_percent, b.share_percent),
        _ => false,
    };
    a.label == b.label
        && a.rows == b.rows
        && a.declarations == b.declarations
        && a.companies == b.companies
        && close(a.total_quantity, b.total_quantity)
        && compatible_share_matches
        && usd_compatibility_matches(a.compatible_usd.as_ref(), b.compatible_usd.as_ref())
        && measures_match(&a.measures, &b.measures)
}

fn usd_compatibility_matches(
    a: Option<&AnalyticsUsdCompatibility>,
    b: Option<&AnalyticsUsdCompatibility>,
) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some(a), Some(b)) => {
            close(a.total_value_usd, b.total_value_usd)
                && optional_close(a.avg_value_per_net_kg, b.avg_value_per_net_kg)
        }
        _ => false,
    }
}

fn measures_match(a: &AnalyticsMeasures, b: &AnalyticsMeasures) -> bool {
    currency_totals_match(&a.currency_totals, &b.currency_totals)
        && weight_totals_match(&a.net_weight_totals, &b.net_weight_totals)
        && weight_totals_match(&a.gross_weight_totals, &b.gross_weight_totals)
        && value_per_weight_match(&a.value_per_net_weight, &b.value_per_net_weight)
        && currency_total_option_matches(
            a.compatible_value_total.as_ref(),
            b.compatible_value_total.as_ref(),
        )
        && value_per_weight_option_matches(
            a.compatible_value_per_net_weight.as_ref(),
            b.compatible_value_per_net_weight.as_ref(),
        )
        && a.exclusions == b.exclusions
}

fn currency_totals_match(a: &[AnalyticsCurrencyTotal], b: &[AnalyticsCurrencyTotal]) -> bool {
    a.len() == b.len()
        && a.iter().all(|left| {
            b.iter()
                .find(|right| right.currency == left.currency)
                .is_some_and(|right| currency_total_matches(left, right))
        })
}

fn currency_total_matches(a: &AnalyticsCurrencyTotal, b: &AnalyticsCurrencyTotal) -> bool {
    a.currency == b.currency
        && a.known == b.known
        && a.valued_rows == b.valued_rows
        && close(a.total_value, b.total_value)
}

fn currency_total_option_matches(
    a: Option<&AnalyticsCurrencyTotal>,
    b: Option<&AnalyticsCurrencyTotal>,
) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some(a), Some(b)) => currency_total_matches(a, b),
        _ => false,
    }
}

fn weight_totals_match(a: &[AnalyticsWeightTotal], b: &[AnalyticsWeightTotal]) -> bool {
    a.len() == b.len()
        && a.iter().all(|left| {
            b.iter()
                .find(|right| right.source_unit == left.source_unit)
                .is_some_and(|right| weight_total_matches(left, right))
        })
}

fn weight_total_matches(a: &AnalyticsWeightTotal, b: &AnalyticsWeightTotal) -> bool {
    a.source_unit == b.source_unit
        && a.known == b.known
        && a.normalized_unit == b.normalized_unit
        && optional_close(a.factor_to_kg, b.factor_to_kg)
        && a.weighted_rows == b.weighted_rows
        && close(a.total_source_weight, b.total_source_weight)
        && optional_close(a.total_kg, b.total_kg)
}

fn value_per_weight_match(a: &[AnalyticsValuePerWeight], b: &[AnalyticsValuePerWeight]) -> bool {
    a.len() == b.len()
        && a.iter().all(|left| {
            b.iter()
                .find(|right| {
                    right.currency == left.currency
                        && right.normalized_weight_unit == left.normalized_weight_unit
                })
                .is_some_and(|right| value_per_weight_row_matches(left, right))
        })
}

fn value_per_weight_row_matches(a: &AnalyticsValuePerWeight, b: &AnalyticsValuePerWeight) -> bool {
    let mut a_units = a.source_weight_units.clone();
    let mut b_units = b.source_weight_units.clone();
    a_units.sort();
    b_units.sort();
    a.currency == b.currency
        && a.normalized_weight_unit == b.normalized_weight_unit
        && a_units == b_units
        && a.paired_rows == b.paired_rows
        && close(a.total_value, b.total_value)
        && close(a.total_weight, b.total_weight)
        && optional_close(a.value_per_weight, b.value_per_weight)
}

fn value_per_weight_option_matches(
    a: Option<&AnalyticsValuePerWeight>,
    b: Option<&AnalyticsValuePerWeight>,
) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some(a), Some(b)) => value_per_weight_row_matches(a, b),
        _ => false,
    }
}

fn optional_close(a: Option<f64>, b: Option<f64>) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some(a), Some(b)) => close(a, b),
        _ => false,
    }
}

fn prices_match(a: &[AnalyticsPriceMetric], b: &[AnalyticsPriceMetric]) -> bool {
    a.len() == b.len()
        && a.iter().zip(b).all(|(a, b)| {
            a.kind == b.kind
                && a.count == b.count
                && close(a.average, b.average)
                && close(a.minimum, b.minimum)
                && close(a.maximum, b.maximum)
                && close(a.weighted_average, b.weighted_average)
                && close(a.median, b.median)
                && close(a.p25, b.p25)
                && close(a.p75, b.p75)
        })
}

fn close(a: f64, b: f64) -> bool {
    let diff = (a - b).abs();
    diff <= 1e-6 || diff / a.abs().max(b.abs()).max(1.0) < 1e-9
}

#[cfg(test)]
mod tests {
    use super::overview_matches;
    use crate::db::{
        AnalyticsCurrencyTotal, AnalyticsMeasures, AnalyticsOverview, AnalyticsUsdCompatibility,
    };

    #[test]
    fn verification_rejects_dimension_rollup_drift() {
        let sqlite = AnalyticsOverview {
            distinct_product_codes: 1,
            distinct_recipients: 1,
            distinct_origin_countries: 1,
            ..Default::default()
        };
        let duck = AnalyticsOverview::default();

        assert!(!overview_matches(&sqlite, &duck));
    }

    #[test]
    fn verification_rejects_material_numeric_drift() {
        let sqlite = AnalyticsOverview {
            total_value_usd: 1_000.0,
            compatible_usd: Some(AnalyticsUsdCompatibility {
                total_value_usd: 1_000.0,
                avg_value_per_net_kg: None,
            }),
            ..Default::default()
        };
        let duck = AnalyticsOverview {
            total_value_usd: 1_001.0,
            compatible_usd: Some(AnalyticsUsdCompatibility {
                total_value_usd: 1_001.0,
                avg_value_per_net_kg: None,
            }),
            ..Default::default()
        };

        assert!(!overview_matches(&sqlite, &duck));
    }

    #[test]
    fn verification_rejects_currency_cohort_drift() {
        let sqlite = AnalyticsOverview {
            measures: AnalyticsMeasures {
                currency_totals: vec![AnalyticsCurrencyTotal {
                    currency: "USD".to_string(),
                    known: true,
                    valued_rows: 2,
                    total_value: 1_000.0,
                }],
                ..Default::default()
            },
            ..Default::default()
        };
        let duck = AnalyticsOverview {
            measures: AnalyticsMeasures {
                currency_totals: vec![AnalyticsCurrencyTotal {
                    currency: "EUR".to_string(),
                    known: true,
                    valued_rows: 2,
                    total_value: 1_000.0,
                }],
                ..Default::default()
            },
            ..Default::default()
        };

        assert!(!overview_matches(&sqlite, &duck));
    }
}
