//! DuckDB projection gating. A projection is usable only when its generation
//! and fingerprint match the live SQLite records, schema, and semantic mapping.
//! Baseline totals are verified once per matching contract; stale, unreadable,
//! or untrusted states stay on SQLite.

#![cfg(feature = "duckdb-olap")]

use std::path::{Path, PathBuf};

use crate::db::{
    Analytics, AnalyticsGroupRow, AnalyticsMonthRow, AnalyticsOverview, AnalyticsPriceMetric,
    AnalyticsScope, AnalyticsSection, Db, Query,
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
        && close(a.total_value_usd, b.total_value_usd)
        && close(a.total_net_kg, b.total_net_kg)
        && close(a.total_gross_kg, b.total_gross_kg)
        && close(a.total_quantity, b.total_quantity)
        && close(a.avg_value_per_net_kg, b.avg_value_per_net_kg)
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
                && close(a.total_value_usd, b.total_value_usd)
                && close(a.total_net_kg, b.total_net_kg)
        })
}

fn sections_match(a: &[AnalyticsSection], b: &[AnalyticsSection]) -> bool {
    a.len() == b.len()
        && a.iter().zip(b).all(|(a, b)| {
            a.kind == b.kind
                && a.rows.len() == b.rows.len()
                && a.rows
                    .iter()
                    .zip(&b.rows)
                    .all(|(a, b)| group_rows_match(a, b))
        })
}

fn group_rows_match(a: &AnalyticsGroupRow, b: &AnalyticsGroupRow) -> bool {
    a.label == b.label
        && a.rows == b.rows
        && a.declarations == b.declarations
        && a.companies == b.companies
        && close(a.total_value_usd, b.total_value_usd)
        && close(a.total_net_kg, b.total_net_kg)
        && close(a.total_gross_kg, b.total_gross_kg)
        && close(a.total_quantity, b.total_quantity)
        && close(a.share_percent, b.share_percent)
        && close(a.avg_value_per_net_kg, b.avg_value_per_net_kg)
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
    use crate::db::AnalyticsOverview;

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
            ..Default::default()
        };
        let duck = AnalyticsOverview {
            total_value_usd: 1_001.0,
            ..Default::default()
        };

        assert!(!overview_matches(&sqlite, &duck));
    }
}
