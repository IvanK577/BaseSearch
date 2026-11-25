use std::time::Instant;

use serde::Serialize;

use crate::db::{
    AnalyticsScope, Db, PivotDim, PivotLimits, PivotMetric, Query, analytics_should_run,
};

#[derive(Debug, Clone)]
pub struct OlapBenchmarkOptions {
    pub repeat: usize,
    pub warmups: usize,
    pub page_limit: u64,
    pub section_limit: u64,
    pub hs_level: u8,
    pub pivot_rows: usize,
    pub pivot_cols: usize,
}

impl Default for OlapBenchmarkOptions {
    fn default() -> Self {
        Self {
            repeat: 3,
            warmups: 1,
            page_limit: 100,
            section_limit: 10,
            hs_level: 10,
            pivot_rows: 20,
            pivot_cols: 12,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct OlapBenchmarkReport {
    pub backend: &'static str,
    pub total_database_rows: u64,
    pub unindexed_rows: u64,
    pub query: Query,
    pub query_is_empty: bool,
    pub scenarios: Vec<OlapScenarioReport>,
}

#[derive(Debug, Clone, Serialize)]
pub struct OlapScenarioReport {
    pub name: &'static str,
    pub category: &'static str,
    pub output_rows: u64,
    pub average_ms: f64,
    pub minimum_ms: f64,
    pub maximum_ms: f64,
    pub runs_ms: Vec<f64>,
    pub note: &'static str,
}

pub fn run_sqlite_benchmark(
    db: &Db,
    query: &Query,
    options: &OlapBenchmarkOptions,
) -> Result<OlapBenchmarkReport, String> {
    let mut scenarios = Vec::new();
    scenarios.push(measure_scenario(
        options,
        "Search count",
        "search",
        "Counts all rows matching the query.",
        || db.count(query).map_err(|err| err.to_string()),
    )?);
    scenarios.push(measure_scenario(
        options,
        "First result page",
        "search",
        "Reads visible table rows with the dynamic source-column layout.",
        || {
            db.search_page_dynamic(query, options.page_limit, 0)
                .map(|(_, _, rows, _)| rows.len() as u64)
                .map_err(|err| err.to_string())
        },
    )?);
    scenarios.push(measure_scenario(
        options,
        "Analytics overview",
        "olap",
        "Computes headline totals, distinct counts, and monthly dynamics.",
        || {
            db.analytics_scoped(query, options.section_limit, None, options.hs_level)
                .map(|analytics| analytics.overview.row_count)
                .map_err(|err| err.to_string())
        },
    )?);
    scenarios.push(measure_scenario(
        options,
        "Companies aggregation",
        "olap",
        "Groups matching rows by company identifiers, recipients, and senders.",
        || {
            db.analytics_scoped(
                query,
                options.section_limit,
                Some(AnalyticsScope::Companies),
                options.hs_level,
            )
            .map(|analytics| count_section_rows(&analytics.company_sections))
            .map_err(|err| err.to_string())
        },
    )?);
    scenarios.push(measure_scenario(
        options,
        "Products aggregation",
        "olap",
        "Groups matching rows by product codes, trademarks, and description groups.",
        || {
            db.analytics_scoped(
                query,
                options.section_limit,
                Some(AnalyticsScope::Products),
                options.hs_level,
            )
            .map(|analytics| count_section_rows(&analytics.product_sections))
            .map_err(|err| err.to_string())
        },
    )?);
    scenarios.push(measure_scenario(
        options,
        "Countries aggregation",
        "olap",
        "Groups matching rows by origin, dispatch, and trade countries.",
        || {
            db.analytics_scoped(
                query,
                options.section_limit,
                Some(AnalyticsScope::Countries),
                options.hs_level,
            )
            .map(|analytics| count_section_rows(&analytics.country_sections))
            .map_err(|err| err.to_string())
        },
    )?);
    scenarios.push(measure_scenario(
        options,
        "Price metrics",
        "olap",
        "Calculates average, weighted, min/max, median, and quartile price metrics.",
        || {
            db.analytics_scoped(
                query,
                options.section_limit,
                Some(AnalyticsScope::Prices),
                options.hs_level,
            )
            .map(|analytics| {
                analytics
                    .price_sections
                    .iter()
                    .filter(|metric| metric.count > 0)
                    .count() as u64
            })
            .map_err(|err| err.to_string())
        },
    )?);
    scenarios.push(measure_scenario(
        options,
        "Pivot: recipient by month",
        "olap",
        "Builds a value cross-tab for top recipients by month.",
        || {
            db.pivot(
                query,
                PivotDim::Recipient,
                PivotDim::Month,
                PivotMetric::Value,
                PivotLimits {
                    rows: options.pivot_rows,
                    cols: options.pivot_cols,
                },
                "Other",
            )
            .map(|pivot| (pivot.row_labels.len() * pivot.col_labels.len()) as u64)
            .map_err(|err| err.to_string())
        },
    )?);
    scenarios.push(measure_scenario(
        options,
        "Possible undervaluation",
        "olap",
        "Scans priced rows against product-code median price-per-kg baselines.",
        || {
            db.undervaluation(query, 0.55, 5, 100)
                .map(|risk| risk.rows.len() as u64)
                .map_err(|err| err.to_string())
        },
    )?);

    Ok(OlapBenchmarkReport {
        backend: "sqlite",
        total_database_rows: db.total_rows(),
        unindexed_rows: db.unindexed_rows(),
        query: query.clone(),
        query_is_empty: !analytics_should_run(query),
        scenarios,
    })
}

fn measure_scenario(
    options: &OlapBenchmarkOptions,
    name: &'static str,
    category: &'static str,
    note: &'static str,
    mut run: impl FnMut() -> Result<u64, String>,
) -> Result<OlapScenarioReport, String> {
    for _ in 0..options.warmups {
        run()?;
    }

    let repeat = options.repeat.max(1);
    let mut runs_ms = Vec::with_capacity(repeat);
    let mut output_rows = 0;
    for _ in 0..repeat {
        let started = Instant::now();
        output_rows = run()?;
        runs_ms.push(round_ms(started.elapsed().as_secs_f64() * 1000.0));
    }

    Ok(OlapScenarioReport {
        name,
        category,
        output_rows,
        average_ms: round_ms(runs_ms.iter().sum::<f64>() / runs_ms.len() as f64),
        minimum_ms: runs_ms.iter().copied().fold(f64::INFINITY, f64::min),
        maximum_ms: runs_ms.iter().copied().fold(0.0, f64::max),
        runs_ms,
        note,
    })
}

fn count_section_rows(sections: &[crate::db::AnalyticsSection]) -> u64 {
    sections
        .iter()
        .map(|section| section.rows.len() as u64)
        .sum()
}

fn round_ms(value: f64) -> f64 {
    (value * 1000.0).round() / 1000.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_benchmark_options_are_bounded_for_local_runs() {
        let options = OlapBenchmarkOptions::default();
        assert_eq!(options.repeat, 3);
        assert_eq!(options.warmups, 1);
        assert!(options.page_limit <= 500);
        assert!(options.section_limit <= 50);
        assert!(options.pivot_rows <= 50);
        assert!(options.pivot_cols <= 50);
    }

    #[test]
    fn benchmark_measurement_runs_at_least_once() {
        let options = OlapBenchmarkOptions {
            repeat: 0,
            warmups: 0,
            ..Default::default()
        };
        let mut calls = 0;
        let scenario = measure_scenario(&options, "test", "unit", "note", || {
            calls += 1;
            Ok(42)
        })
        .unwrap();
        assert_eq!(calls, 1);
        assert_eq!(scenario.output_rows, 42);
        assert_eq!(scenario.runs_ms.len(), 1);
    }
}
