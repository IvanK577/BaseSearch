//! DuckDB projection gating. A projection is usable only when its generation
//! and fingerprint match the live SQLite records, schema, and semantic mapping.
//! Baseline totals are verified once per matching contract; stale, unreadable,
//! or untrusted states stay on SQLite.

#![cfg(feature = "duckdb-olap")]

use std::path::{Path, PathBuf};

use crate::db::{
    Analytics, AnalyticsCurrencyTotal, AnalyticsGroupRow, AnalyticsMeasures, AnalyticsMonthRow,
    AnalyticsOverview, AnalyticsPriceMetric, AnalyticsScope, AnalyticsSection,
    AnalyticsSectionKind, AnalyticsUsdCompatibility, AnalyticsValuePerWeight, AnalyticsWeightTotal,
    Db, Filters, Query,
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

/// How much of a section each verification probe reads, and which HS level it
/// groups product codes by. The probes are aggregate scans: the point is to
/// compare the numbers, not to page through them.
const VERIFY_LIMIT: u64 = 20;
const VERIFY_HS_LEVEL: u8 = 10;

/// Compares the two engines on this database and reports whether the
/// projection can be trusted for analytics.
///
/// The empty query alone is not enough evidence. It exercises no predicate at
/// all, so a filter that silently matches nothing — a needle folded to ASCII
/// while the column is folded by Unicode, a year read off the wrong column —
/// reproduces SQLite perfectly right up until a user asks a question. The
/// probes below are built from this database's own data so that they actually
/// select rows here, and the totals the browser renders are compared alongside
/// the per-bucket measures.
fn verify(db: &Db, sqlite_path: &Path, path: &Path) -> Result<bool, ApiError> {
    let engine = DuckDbAnalyticsEngine::new(sqlite_path, path);
    let empty = Query::default();
    let mut baseline: Option<Analytics> = None;
    for scope in AnalyticsScope::ALL {
        let sqlite = sqlite_analytics(db, &empty, scope)?;
        let duck = duck_analytics(&engine, &empty, scope)?;
        if !analytics_matches(&sqlite, &duck) {
            return Ok(false);
        }
        if matches!(scope, AnalyticsScope::Companies) {
            baseline = Some(sqlite);
        }
    }

    for query in probe_queries(baseline.as_ref()) {
        // Companies carries the month series and the group rows; Products adds
        // the derived product-code grouping. Between them every row shape the
        // browser renders is covered for a filtered query.
        for scope in [AnalyticsScope::Companies, AnalyticsScope::Products] {
            let sqlite = sqlite_analytics(db, &query, scope)?;
            let duck = duck_analytics(&engine, &query, scope)?;
            if !analytics_matches(&sqlite, &duck) {
                return Ok(false);
            }
        }
    }

    free_text_folding_holds(path, baseline.as_ref())
}

fn sqlite_analytics(db: &Db, query: &Query, scope: AnalyticsScope) -> Result<Analytics, ApiError> {
    db.analytics_scoped(query, VERIFY_LIMIT, Some(scope), VERIFY_HS_LEVEL)
        .map_err(|err| ApiError::internal("verify projection (sqlite)", err))
}

fn duck_analytics(
    engine: &DuckDbAnalyticsEngine,
    query: &Query,
    scope: AnalyticsScope,
) -> Result<Analytics, ApiError> {
    engine
        .analytics(query, VERIFY_LIMIT, Some(scope), VERIFY_HS_LEVEL)
        .map_err(|err| ApiError::internal("verify projection (duckdb)", err))
}

/// Non-empty probes derived from the baseline answer, so each one selects rows
/// on this database instead of comparing two empty results.
fn probe_queries(baseline: Option<&Analytics>) -> Vec<Query> {
    let Some(baseline) = baseline else {
        return Vec::new();
    };
    let mut probes = Vec::new();
    // A year filter is the only filter where both engines DERIVE the value they
    // match on instead of reading it, so it is the only one that can disagree
    // about which rows exist rather than about how to compare a string.
    if let Some(year) = baseline.months.last().and_then(|row| row.month.get(..4))
        && year.len() == 4
        && year.chars().all(|digit| digit.is_ascii_digit())
    {
        probes.push(Query {
            filters: Filters {
                year: year.to_string(),
                ..Filters::default()
            },
            ..Query::default()
        });
    }
    // A "contains" filter over a real company name. On Ukrainian data this is
    // exactly the needle ASCII-only case folding can never match. The WHOLE
    // label is used rather than a fragment: SQLite also requires the label's
    // FTS prefix terms, and a fragment starting mid-token would legitimately
    // match one engine and not the other.
    if let Some(label) = top_label(baseline, AnalyticsSectionKind::Recipients) {
        probes.push(Query {
            filters: Filters {
                recipient: label,
                ..Filters::default()
            },
            ..Query::default()
        });
    }
    probes
}

fn top_label(analytics: &Analytics, kind: AnalyticsSectionKind) -> Option<String> {
    analytics
        .company_sections
        .iter()
        .find(|section| section.kind == kind)
        .and_then(|section| section.rows.first())
        .map(|row| row.label.clone())
        .filter(|label| !label.trim().is_empty())
}

/// The one property of the free-text predicate both engines must share.
///
/// They do not answer free text the same way, and are not meant to: SQLite
/// matches FTS tokens, the projection scans for a substring, and the analytics
/// API only ever routes a text query to SQLite. Comparing the two row for row
/// would reject healthy projections over a difference no user can observe.
/// What IS observable — through the OLAP benchmark, and through anything that
/// later reaches for the projection's own filter — is whether that predicate
/// folds case the way the rest of the app does. ASCII-only folding left every
/// Cyrillic needle uppercase while the column arrived lowercase, so the same
/// words found rows in one spelling and nothing in the other. That asymmetry is
/// what this rules out, using a company name that exists in this database.
fn free_text_folding_holds(path: &Path, baseline: Option<&Analytics>) -> Result<bool, ApiError> {
    let Some(needle) = baseline.and_then(|analytics| {
        top_label(analytics, AnalyticsSectionKind::Recipients)
            .or_else(|| top_label(analytics, AnalyticsSectionKind::Senders))
    }) else {
        return Ok(true);
    };
    let lower = needle.to_lowercase();
    let upper = lower.to_uppercase();
    // WHY the round-trip guard: this proves "the needle is folded the way the
    // column is folded", and it can only prove it with two spellings that a
    // correct fold maps onto the SAME needle. Some letters never survive the
    // trip through upper case — German "ß" uppercases to "SS" and comes back as
    // "ss", Greek final sigma and the Turkish dotted I behave the same way. For
    // such a label the two probes are genuinely two different needles even after
    // a perfectly correct Unicode fold, they legitimately select different rows,
    // and a healthy projection would be distrusted because a supplier is called
    // "Großmann". Skipping is safe: it costs one probe, not the check.
    if upper == lower || upper.to_lowercase() != lower {
        return Ok(true);
    }
    Ok(text_row_count(path, &upper)? == text_row_count(path, &lower)?)
}

fn text_row_count(path: &Path, text: &str) -> Result<u64, ApiError> {
    let query = Query {
        text: text.to_string(),
        ..Query::default()
    };
    duckdb_olap::projection_row_count(path, &query)
        .map_err(|err| ApiError::internal("verify projection (duckdb text)", err))
}

/// Which of the two engines' plain totals ask the same question on this data.
///
/// `total_net_kg` and `total_gross_kg` are serialized, so they have to agree —
/// but the engines reach them differently on purpose: SQLite sums the weight
/// column exactly as the source stores it, the projection sums it converted to
/// kilograms and leaves out rows whose unit it cannot convert. Those are the
/// same number precisely when every contributing bucket is already kilograms,
/// which covers customs data and every dataset whose unit maps to kg. Anywhere
/// else the honest per-unit answer is in `measures`, which is compared
/// unconditionally, and demanding equality here would distrust a projection for
/// reporting the better number.
#[derive(Clone, Copy)]
struct ComparableTotals {
    net_kg: bool,
    gross_kg: bool,
}

impl ComparableTotals {
    fn for_overviews(a: &AnalyticsOverview, b: &AnalyticsOverview) -> Self {
        Self {
            net_kg: already_kilograms(&a.measures.net_weight_totals)
                && already_kilograms(&b.measures.net_weight_totals),
            gross_kg: already_kilograms(&a.measures.gross_weight_totals)
                && already_kilograms(&b.measures.gross_weight_totals),
        }
    }
}

fn already_kilograms(buckets: &[AnalyticsWeightTotal]) -> bool {
    buckets.iter().all(|bucket| {
        bucket.known
            && bucket.source_unit == "kg"
            && bucket.normalized_unit.as_deref() == Some("kg")
    })
}

/// The money total, compared where it means the same thing on both sides.
///
/// SQLite keeps a plain cross-currency sum in this field for the legacy desktop
/// view; the projection reports 0 unless the whole set is one known USD cohort.
/// When that cohort exists both hold the same USD sum — and that is also the
/// number `share_percent` is computed against, so a drift here moves every
/// share on the page.
fn value_total_matches(
    a_compatible: Option<&AnalyticsUsdCompatibility>,
    a_total: f64,
    b_compatible: Option<&AnalyticsUsdCompatibility>,
    b_total: f64,
) -> bool {
    match (a_compatible, b_compatible) {
        (Some(_), Some(_)) => close(a_total, b_total),
        _ => true,
    }
}

fn overview_matches(
    a: &AnalyticsOverview,
    b: &AnalyticsOverview,
    comparable: ComparableTotals,
) -> bool {
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
        && value_total_matches(
            a.compatible_usd.as_ref(),
            a.total_value_usd,
            b.compatible_usd.as_ref(),
            b.total_value_usd,
        )
        && (!comparable.net_kg || close(a.total_net_kg, b.total_net_kg))
        && (!comparable.gross_kg || close(a.total_gross_kg, b.total_gross_kg))
        && usd_compatibility_matches(a.compatible_usd.as_ref(), b.compatible_usd.as_ref())
        && measures_match(&a.measures, &b.measures)
}

fn analytics_matches(a: &Analytics, b: &Analytics) -> bool {
    // The weight buckets that decide comparability are a property of the whole
    // result set, not of one row: a group row inherits an empty bucket list when
    // the query mixes units, and reading comparability off the row would then
    // claim a mixed-unit set is directly comparable.
    let comparable = ComparableTotals::for_overviews(&a.overview, &b.overview);
    overview_matches(&a.overview, &b.overview, comparable)
        && months_match(&a.months, &b.months, comparable)
        && sections_match(&a.company_sections, &b.company_sections, comparable)
        && sections_match(&a.product_sections, &b.product_sections, comparable)
        && sections_match(&a.country_sections, &b.country_sections, comparable)
        && prices_match(&a.price_sections, &b.price_sections)
}

fn months_match(
    a: &[AnalyticsMonthRow],
    b: &[AnalyticsMonthRow],
    comparable: ComparableTotals,
) -> bool {
    a.len() == b.len()
        && a.iter().zip(b).all(|(a, b)| {
            a.month == b.month
                && a.rows == b.rows
                && a.declarations == b.declarations
                && value_total_matches(
                    a.compatible_usd.as_ref(),
                    a.total_value_usd,
                    b.compatible_usd.as_ref(),
                    b.total_value_usd,
                )
                && (!comparable.net_kg || close(a.total_net_kg, b.total_net_kg))
                && usd_compatibility_matches(a.compatible_usd.as_ref(), b.compatible_usd.as_ref())
                && measures_match(&a.measures, &b.measures)
        })
}

fn sections_match(
    a: &[AnalyticsSection],
    b: &[AnalyticsSection],
    comparable: ComparableTotals,
) -> bool {
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
                                .is_some_and(|right_row| {
                                    group_rows_match(left_row, right_row, comparable)
                                })
                        })
                })
        })
}

fn group_rows_match(
    a: &AnalyticsGroupRow,
    b: &AnalyticsGroupRow,
    comparable: ComparableTotals,
) -> bool {
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
        && value_total_matches(
            a.compatible_usd.as_ref(),
            a.total_value_usd,
            b.compatible_usd.as_ref(),
            b.total_value_usd,
        )
        && (!comparable.net_kg || close(a.total_net_kg, b.total_net_kg))
        && (!comparable.gross_kg || close(a.total_gross_kg, b.total_gross_kg))
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
    use super::{ComparableTotals, overview_matches};
    use crate::db::{
        AnalyticsCurrencyTotal, AnalyticsMeasures, AnalyticsOverview, AnalyticsUsdCompatibility,
        AnalyticsWeightTotal,
    };

    fn overviews_match(sqlite: &AnalyticsOverview, duck: &AnalyticsOverview) -> bool {
        overview_matches(sqlite, duck, ComparableTotals::for_overviews(sqlite, duck))
    }

    fn kilograms(total_kg: f64) -> AnalyticsWeightTotal {
        AnalyticsWeightTotal {
            source_unit: "kg".to_string(),
            known: true,
            normalized_unit: Some("kg".to_string()),
            factor_to_kg: Some(1.0),
            weighted_rows: 1,
            total_source_weight: total_kg,
            total_kg: Some(total_kg),
        }
    }

    #[test]
    fn verification_rejects_dimension_rollup_drift() {
        let sqlite = AnalyticsOverview {
            distinct_product_codes: 1,
            distinct_recipients: 1,
            distinct_origin_countries: 1,
            ..Default::default()
        };
        let duck = AnalyticsOverview::default();

        assert!(!overviews_match(&sqlite, &duck));
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

        assert!(!overviews_match(&sqlite, &duck));
    }

    /// The weight totals are serialized, so a projection that reports a
    /// different number of kilograms for the same kilogram data is not usable —
    /// and nothing in the comparison used to look at them.
    #[test]
    fn verification_rejects_weight_total_drift_for_kilogram_data() {
        let measures = |total_kg: f64| AnalyticsMeasures {
            net_weight_totals: vec![kilograms(total_kg)],
            gross_weight_totals: vec![kilograms(total_kg)],
            ..Default::default()
        };
        let sqlite = AnalyticsOverview {
            total_net_kg: 10.0,
            total_gross_kg: 10.0,
            measures: measures(10.0),
            ..Default::default()
        };
        let duck = AnalyticsOverview {
            total_net_kg: 0.0,
            total_gross_kg: 10.0,
            measures: measures(10.0),
            ..Default::default()
        };

        assert!(!overviews_match(&sqlite, &duck));
    }

    /// A unit that has to be converted is the one case where the two engines
    /// answer different questions on purpose — SQLite sums grams as grams, the
    /// projection sums them as kilograms — so the plain totals must not be the
    /// thing that decides trust. The per-unit `measures` still are.
    #[test]
    fn verification_tolerates_converted_units_in_plain_totals() {
        let grams = AnalyticsWeightTotal {
            source_unit: "g".to_string(),
            known: true,
            normalized_unit: Some("kg".to_string()),
            factor_to_kg: Some(0.001),
            weighted_rows: 1,
            total_source_weight: 1_000.0,
            total_kg: Some(1.0),
        };
        let measures = AnalyticsMeasures {
            net_weight_totals: vec![grams],
            ..Default::default()
        };
        let sqlite = AnalyticsOverview {
            total_net_kg: 1_000.0,
            measures: measures.clone(),
            ..Default::default()
        };
        let duck = AnalyticsOverview {
            total_net_kg: 1.0,
            measures,
            ..Default::default()
        };

        assert!(overviews_match(&sqlite, &duck));
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

        assert!(!overviews_match(&sqlite, &duck));
    }
}
