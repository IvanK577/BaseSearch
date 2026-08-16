use serde::{Deserialize, Serialize};

use crate::search::{FieldInfo, QueryExpr};

/// What opening a database is doing right now.
///
/// Opening is usually instant, but a database carried over from an older
/// version is rebuilt on the first open: backed up, verified, copied, its row
/// fingerprints and typed columns recomputed. On a multi-gigabyte database that
/// is minutes of work. It used to report itself only through `eprintln!`, and
/// the release build has no console — so the window sat on "Opening database"
/// with a spinner and a rising seconds counter, which is indistinguishable from
/// a hang. Every phase below is a thing the person can be told.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StartupPhase {
    CheckingVersion,
    CheckingFreeSpace,
    CreatingBackup,
    VerifyingBackup,
    UpgradingStructure,
    RebuildingFingerprints { done: u64, total: u64 },
    ComputingTypedColumns { done: u64, total: u64 },
    VerifyingUpgrade,
}

impl StartupPhase {
    /// Progress within the phase, when it has any. `None` means the phase
    /// cannot say how far along it is and the bar stays indeterminate.
    pub fn progress(&self) -> Option<(u64, u64)> {
        match *self {
            StartupPhase::RebuildingFingerprints { done, total }
            | StartupPhase::ComputingTypedColumns { done, total } => Some((done, total)),
            _ => None,
        }
    }

    /// True while the database is being rewritten, as opposed to inspected.
    /// The window uses this to add "do not close" and the one-time note.
    pub fn is_upgrade(&self) -> bool {
        !matches!(
            self,
            StartupPhase::CheckingVersion | StartupPhase::CheckingFreeSpace
        )
    }
}

/// Filter values; an empty string means the filter is not set.
#[derive(Default, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Filters {
    #[serde(default)]
    pub year: String,
    #[serde(default)]
    pub product_code: String,
    #[serde(default)]
    pub trademark: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub sender: String,
    #[serde(default)]
    pub recipient: String,
    #[serde(default)]
    pub edrpou: String,
    #[serde(default)]
    pub trade_country: String,
    #[serde(default)]
    pub dispatch_country: String,
    #[serde(default)]
    pub origin_country: String,
}

impl Filters {
    pub fn is_empty(&self) -> bool {
        [
            &self.year,
            &self.product_code,
            &self.trademark,
            &self.description,
            &self.sender,
            &self.recipient,
            &self.edrpou,
            &self.trade_country,
            &self.dispatch_country,
            &self.origin_country,
        ]
        .iter()
        .all(|value| value.trim().is_empty())
    }

    pub fn clear(&mut self) {
        *self = Filters::default();
    }
}

#[derive(Default, Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecordScope {
    #[default]
    Canonical,
    Occurrences,
}

#[derive(Default, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Query {
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub filters: Filters,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub advanced: Option<QueryExpr>,
    #[serde(default)]
    pub record_scope: RecordScope,
}

impl Query {
    pub fn is_empty(&self) -> bool {
        self.text.trim().is_empty()
            && self.filters.is_empty()
            && self.advanced.as_ref().is_none_or(QueryExpr::is_empty)
    }
}

/// One row prepared for insertion during import.
pub struct ImportRecord {
    pub hash: [u8; 16],
    pub year: Option<i64>,
    pub values: Vec<String>,
    /// Source columns not stored in compatibility schema fields. JSON array of
    /// [header, value] pairs.
    pub extra: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FtsRepairIssue {
    MissingIndex,
    VersionStale,
    IntegrityCheckFailed,
    ContentMismatch,
    WatermarkMismatch,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FtsRepairReport {
    pub rebuilt: bool,
    pub cancelled: bool,
    pub indexed_rows: u64,
    pub watermark: i64,
    pub schema_version: String,
    pub issues: Vec<FtsRepairIssue>,
}

#[derive(Debug)]
pub enum FtsRepairError {
    Database(rusqlite::Error),
    SourceChanged,
    Validation(String),
}

impl std::fmt::Display for FtsRepairError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Database(error) => write!(formatter, "{error}"),
            Self::SourceChanged => {
                formatter.write_str("records changed during FTS rebuild; the live index was kept")
            }
            Self::Validation(message) => formatter.write_str(message),
        }
    }
}

impl std::error::Error for FtsRepairError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Database(error) => Some(error),
            Self::SourceChanged | Self::Validation(_) => None,
        }
    }
}

impl From<rusqlite::Error> for FtsRepairError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Database(error)
    }
}

pub struct RecordCard {
    pub fields: Vec<(String, String)>,
    pub source_file: String,
    /// Extra source columns this file had beyond the known schema, in file order.
    pub extra: Vec<(String, String)>,
}

#[derive(Clone)]
pub struct ImportLogEntry {
    pub file_name: String,
    pub total_rows: u64,
    pub imported: u64,
    pub duplicates: u64,
    pub seconds: f64,
    pub imported_at: String,
    pub quality: ImportQuality,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ImportQuality {
    pub layout: String,
    pub header_row: u64,
    pub source_columns: u64,
    pub recognized_columns: u64,
    pub extra_columns: u64,
    pub non_empty_cells: u64,
    pub empty_cells: u64,
    pub warnings: Vec<String>,
}

impl ImportQuality {
    pub fn filled_percent(&self) -> f64 {
        let total = self.non_empty_cells + self.empty_cells;
        if total == 0 {
            0.0
        } else {
            self.non_empty_cells as f64 * 100.0 / total as f64
        }
    }

    pub(crate) fn warnings_text(&self) -> String {
        self.warnings.join("\n")
    }

    pub(crate) fn with_warnings_text(mut self, warnings: String) -> ImportQuality {
        self.warnings = warnings
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(ToOwned::to_owned)
            .collect();
        self
    }
}

pub struct ImportLogWrite<'a> {
    pub file_name: &'a str,
    pub total_rows: u64,
    pub imported: u64,
    pub duplicates: u64,
    pub seconds: f64,
    pub file_hash: Option<&'a str>,
    pub quality: &'a ImportQuality,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct AnalyticsCurrencyTotal {
    /// Normalized ISO-like currency code, or `unknown` when the source value is
    /// missing or cannot be interpreted safely.
    pub currency: String,
    pub known: bool,
    pub valued_rows: u64,
    pub total_value: f64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct AnalyticsWeightTotal {
    /// Normalized source unit (`kg`, `g`, `tonne`, `lb`) or the preserved
    /// unknown source label.
    pub source_unit: String,
    pub known: bool,
    pub normalized_unit: Option<String>,
    pub factor_to_kg: Option<f64>,
    pub weighted_rows: u64,
    pub total_source_weight: f64,
    pub total_kg: Option<f64>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct AnalyticsValuePerWeight {
    pub currency: String,
    pub normalized_weight_unit: String,
    pub source_weight_units: Vec<String>,
    pub paired_rows: u64,
    pub total_value: f64,
    pub total_weight: f64,
    pub value_per_weight: Option<f64>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnalyticsMeasureExclusions {
    pub value_without_known_currency: u64,
    pub net_weight_without_known_unit: u64,
    pub gross_weight_without_known_unit: u64,
    pub ratio_without_known_currency: u64,
    pub ratio_without_known_weight_unit: u64,
    pub ratio_with_zero_or_missing_weight: u64,
}

/// Currency- and unit-safe measures shared by overview, group, and monthly
/// analytics. Money is never added across currency buckets. Recognized mass
/// units are normalized to kilograms; unknown units remain explicit buckets.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct AnalyticsMeasures {
    pub currency_totals: Vec<AnalyticsCurrencyTotal>,
    pub net_weight_totals: Vec<AnalyticsWeightTotal>,
    pub gross_weight_totals: Vec<AnalyticsWeightTotal>,
    pub value_per_net_weight: Vec<AnalyticsValuePerWeight>,
    pub compatible_value_total: Option<AnalyticsCurrencyTotal>,
    pub compatible_value_per_net_weight: Option<AnalyticsValuePerWeight>,
    pub exclusions: AnalyticsMeasureExclusions,
}

impl AnalyticsMeasures {
    pub fn total_net_kg(&self) -> f64 {
        self.net_weight_totals
            .iter()
            .filter_map(|total| total.total_kg)
            .sum()
    }

    pub fn total_gross_kg(&self) -> f64 {
        self.gross_weight_totals
            .iter()
            .filter_map(|total| total.total_kg)
            .sum()
    }

    /// The one money figure these rows can honestly show, with the label it
    /// has to carry: the currency code for a recognized bucket, or an empty
    /// label for a single bucket whose currency the source never stated.
    ///
    /// `None` means the rows span more than one currency, and no single number
    /// is true of them. Callers that print a scalar must render this as
    /// "not comparable" and fall back to `currency_totals`, never to
    /// `SUM(value)` — adding hryvnia to dollars produces a number that looks
    /// authoritative and means nothing.
    pub fn single_currency_total(&self) -> Option<(f64, &str)> {
        match self.currency_totals.as_slice() {
            [only] => Some((
                only.total_value,
                if only.known {
                    only.currency.as_str()
                } else {
                    ""
                },
            )),
            _ => None,
        }
    }

    /// Value per kilogram on the same rule: one currency bucket, one figure.
    pub fn single_currency_per_net_kg(&self) -> Option<(f64, &str)> {
        match self.value_per_net_weight.as_slice() {
            [only] => only.value_per_weight.map(|ratio| {
                (
                    ratio,
                    if only.currency.starts_with("__unknown__") {
                        ""
                    } else {
                        only.currency.as_str()
                    },
                )
            }),
            _ => None,
        }
    }

    pub fn compatible_usd_total(&self) -> Option<f64> {
        self.compatible_value_total
            .as_ref()
            .filter(|total| total.known && total.currency == "USD")
            .map(|total| total.total_value)
    }

    pub fn compatible_usd_per_net_kg(&self) -> Option<f64> {
        self.compatible_value_per_net_weight
            .as_ref()
            .filter(|metric| metric.currency == "USD")
            .and_then(|metric| metric.value_per_weight)
    }
}

/// Wire-compatible USD fields. This object is flattened only when the query
/// has one known USD currency cohort. The old scalar names are intentionally
/// absent for EUR, mixed-currency, and unknown-currency results.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct AnalyticsUsdCompatibility {
    pub total_value_usd: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub avg_value_per_net_kg: Option<f64>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct AnalyticsOverview {
    pub row_count: u64,
    pub declaration_count: u64,
    pub distinct_senders: u64,
    pub distinct_recipients: u64,
    pub distinct_edrpou: u64,
    pub distinct_trademarks: u64,
    pub distinct_product_codes: u64,
    pub distinct_origin_countries: u64,
    pub distinct_dispatch_countries: u64,
    pub distinct_trade_countries: u64,
    /// Deprecated in-memory compatibility value for the legacy desktop UI.
    /// It is not serialized; use `measures.currency_totals` instead.
    #[serde(skip)]
    pub total_value_usd: f64,
    #[serde(flatten, skip_serializing_if = "Option::is_none")]
    pub compatible_usd: Option<AnalyticsUsdCompatibility>,
    pub total_gross_kg: f64,
    pub total_net_kg: f64,
    pub total_quantity: f64,
    /// Deprecated in-memory compatibility value for the legacy desktop UI.
    /// It is not serialized; use `measures.value_per_net_weight` instead.
    #[serde(skip)]
    pub avg_value_per_net_kg: f64,
    pub measures: AnalyticsMeasures,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnalyticsFilterField {
    Recipient,
    Sender,
    Edrpou,
    ProductCode,
    Trademark,
    OriginCountry,
    DispatchCountry,
    TradeCountry,
    Description,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnalyticsFilterAction {
    pub field: AnalyticsFilterField,
    pub value: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct AnalyticsGroupRow {
    pub label: String,
    pub rows: u64,
    pub declarations: u64,
    pub companies: u64,
    #[serde(skip)]
    pub total_value_usd: f64,
    #[serde(flatten, skip_serializing_if = "Option::is_none")]
    pub compatible_usd: Option<AnalyticsUsdCompatibility>,
    pub total_net_kg: f64,
    pub total_gross_kg: f64,
    pub total_quantity: f64,
    pub share_percent: f64,
    #[serde(skip)]
    pub avg_value_per_net_kg: f64,
    pub measures: AnalyticsMeasures,
    pub filter_action: Option<AnalyticsFilterAction>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnalyticsSectionKind {
    #[default]
    Recipients,
    Senders,
    Edrpou,
    ProductCodes,
    Trademarks,
    ProductGroups,
    OriginCountries,
    DispatchCountries,
    TradeCountries,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct AnalyticsSection {
    pub kind: AnalyticsSectionKind,
    pub rows: Vec<AnalyticsGroupRow>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PriceMetricKind {
    #[default]
    ValuePerNetKg,
    RfvUsdKg,
    RmvNetUsdKg,
    RmvUsdExtraUnit,
    RmvGrossUsdKg,
    MinBaseUsdKg,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct AnalyticsPriceMetric {
    pub kind: PriceMetricKind,
    pub count: u64,
    pub average: f64,
    pub minimum: f64,
    pub maximum: f64,
    pub weighted_average: f64,
    /// Robust statistics: median and quartiles are less sensitive to outliers
    /// and source-data mistakes than min/max.
    pub median: f64,
    pub p25: f64,
    pub p75: f64,
    pub cohorts: Vec<AnalyticsPriceCohort>,
    pub excluded_rows: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct AnalyticsPriceCohort {
    pub currency: String,
    pub normalized_weight_unit: String,
    pub source_weight_units: Vec<String>,
    pub count: u64,
    pub average: f64,
    pub minimum: f64,
    pub maximum: f64,
    pub weighted_average: Option<f64>,
    pub median: f64,
    pub p25: f64,
    pub p75: f64,
    pub numerator_total: f64,
    pub denominator_total: f64,
}

/// Analytics category computed independently, so the GUI can load only
/// the visible one.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnalyticsScope {
    #[default]
    Companies,
    Products,
    Countries,
    Prices,
}

impl AnalyticsScope {
    pub const ALL: [AnalyticsScope; 4] = [
        AnalyticsScope::Companies,
        AnalyticsScope::Products,
        AnalyticsScope::Countries,
        AnalyticsScope::Prices,
    ];

    pub fn index(self) -> usize {
        match self {
            AnalyticsScope::Companies => 0,
            AnalyticsScope::Products => 1,
            AnalyticsScope::Countries => 2,
            AnalyticsScope::Prices => 3,
        }
    }
}

/// One month of import dynamics (chart data).
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct AnalyticsMonthRow {
    /// "2024-03"
    pub month: String,
    pub rows: u64,
    pub declarations: u64,
    #[serde(skip)]
    pub total_value_usd: f64,
    #[serde(flatten, skip_serializing_if = "Option::is_none")]
    pub compatible_usd: Option<AnalyticsUsdCompatibility>,
    pub total_net_kg: f64,
    pub measures: AnalyticsMeasures,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Analytics {
    pub overview: AnalyticsOverview,
    pub months: Vec<AnalyticsMonthRow>,
    pub company_sections: Vec<AnalyticsSection>,
    pub product_sections: Vec<AnalyticsSection>,
    pub country_sections: Vec<AnalyticsSection>,
    pub price_sections: Vec<AnalyticsPriceMetric>,
    pub top_recipients: Vec<AnalyticsGroupRow>,
    pub top_senders: Vec<AnalyticsGroupRow>,
    pub top_trademarks: Vec<AnalyticsGroupRow>,
    pub top_product_codes: Vec<AnalyticsGroupRow>,
    pub top_origin_countries: Vec<AnalyticsGroupRow>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RiskConfidence {
    #[default]
    Low,
    Medium,
    High,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RiskLimitation {
    pub code: String,
    pub message: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct RiskCohort {
    pub product_code: String,
    pub period: String,
    pub currency: String,
    pub weight_unit: String,
    pub brand: Option<String>,
    pub country: Option<String>,
    pub dimensions: Vec<String>,
    pub sample_count: u64,
    pub median: f64,
    pub p25: f64,
    pub p75: f64,
    pub iqr: f64,
    pub lower_fence: f64,
    pub median_ratio_cutoff: f64,
    pub robust_cutoff: f64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct PriceRiskContract {
    pub price_basis: String,
    pub period_granularity: String,
    pub required_dimensions: Vec<String>,
    pub optional_dimensions: Vec<String>,
    pub min_samples: u64,
    pub max_median_ratio: f64,
    pub iqr_multiplier: f64,
    pub includes_subject_record: bool,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct RiskExclusions {
    pub query_rows: u64,
    pub missing_product_code: u64,
    pub missing_period: u64,
    pub missing_currency: u64,
    pub missing_weight_unit: u64,
    pub invalid_value: u64,
    pub invalid_weight: u64,
    pub insufficient_cohort: u64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct RiskCurrencyTotal {
    pub currency: String,
    pub flagged_rows: u64,
    pub flagged_value: f64,
    pub estimated_gap: f64,
}

/// One row flagged by the explainable price-risk heuristic.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct UndervaluedRow {
    pub id: i64,
    pub declaration_date: String,
    pub declaration_number: String,
    pub recipient: String,
    pub sender: String,
    pub edrpou: String,
    pub product_code: String,
    pub description: String,
    pub source_value: f64,
    pub net_kg: f64,
    pub price_per_kg: f64,
    pub code_median: f64,
    pub code_p25: f64,
    pub code_p75: f64,
    pub code_sample_count: u64,
    pub estimated_gap: f64,
    /// price_per_kg / code_median (0.3 means 30% of the typical price).
    pub ratio: f64,
    pub cohort: RiskCohort,
    pub deviation_percent: f64,
    pub confidence: RiskConfidence,
    pub reason: String,
    pub limitations: Vec<RiskLimitation>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Undervaluation {
    pub available: bool,
    pub rows: Vec<UndervaluedRow>,
    /// Number of distinct product codes that had enough samples to judge.
    pub checked_codes: u64,
    /// Priced rows in those judged product codes.
    pub checked_rows: u64,
    pub flagged_rows: u64,
    pub flagged_codes: u64,
    pub flagged_value: f64,
    pub estimated_gap: f64,
    pub eligible_rows: u64,
    pub evaluated_rows: u64,
    pub checked_cohorts: u64,
    pub contract: PriceRiskContract,
    pub exclusions: RiskExclusions,
    pub limitations: Vec<RiskLimitation>,
    pub currency_totals: Vec<RiskCurrencyTotal>,
}

/// Dimension for the pivot table (rows or columns).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PivotDim {
    Recipient,
    Sender,
    Edrpou,
    ProductCode,
    Trademark,
    OriginCountry,
    DispatchCountry,
    TradeCountry,
    Month,
    Year,
}

impl PivotDim {
    /// The filter field this dimension maps to, for drill-down clicks.
    pub fn filter_field(self) -> Option<AnalyticsFilterField> {
        match self {
            PivotDim::Recipient => Some(AnalyticsFilterField::Recipient),
            PivotDim::Sender => Some(AnalyticsFilterField::Sender),
            PivotDim::Edrpou => Some(AnalyticsFilterField::Edrpou),
            PivotDim::ProductCode => Some(AnalyticsFilterField::ProductCode),
            PivotDim::Trademark => Some(AnalyticsFilterField::Trademark),
            PivotDim::OriginCountry => Some(AnalyticsFilterField::OriginCountry),
            PivotDim::DispatchCountry => Some(AnalyticsFilterField::DispatchCountry),
            PivotDim::TradeCountry => Some(AnalyticsFilterField::TradeCountry),
            PivotDim::Month | PivotDim::Year => None,
        }
    }
}

pub fn pivot_filter_action(
    dim: PivotDim,
    value: impl Into<String>,
) -> Option<AnalyticsFilterAction> {
    dim.filter_field().map(|field| AnalyticsFilterAction {
        field,
        value: value.into(),
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PivotMetric {
    Value,
    Rows,
    NetKg,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PivotLimits {
    pub rows: usize,
    pub cols: usize,
}

/// Cross-tab: a matrix of one dimension by another for a chosen metric.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct PivotMeasurePartition {
    pub key: String,
    pub known: bool,
    pub unit: String,
    pub cells: Vec<Vec<f64>>,
    pub row_totals: Vec<f64>,
    pub col_totals: Vec<f64>,
    pub grand_total: f64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct PivotCompatibilityMatrix {
    pub cells: Vec<Vec<f64>>,
    pub row_totals: Vec<f64>,
    pub col_totals: Vec<f64>,
    pub grand_total: f64,
    pub metric_unit: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct PivotResult {
    pub row_labels: Vec<String>,
    pub col_labels: Vec<String>,
    /// cells[row][col].
    #[serde(skip)]
    pub cells: Vec<Vec<f64>>,
    #[serde(skip)]
    pub row_totals: Vec<f64>,
    #[serde(skip)]
    pub col_totals: Vec<f64>,
    #[serde(skip)]
    pub grand_total: f64,
    #[serde(flatten, skip_serializing_if = "Option::is_none")]
    pub compatible_matrix: Option<PivotCompatibilityMatrix>,
    pub partitions: Vec<PivotMeasurePartition>,
    /// True when low-ranked rows/columns were folded into an "others" bucket.
    pub rows_truncated: bool,
    pub cols_truncated: bool,
    /// True when clicking a row or column label can safely apply a legacy
    /// filter. Generic source-column dimensions are calculated correctly but
    /// are not yet mapped to the old filter fields.
    pub row_filterable: bool,
    pub col_filterable: bool,
}

/// Single-company dossier built for one EDRPOU: everything an analyst needs
/// to answer "tell me everything about this importer" on one screen.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct CompanyProfile {
    pub edrpou: String,
    /// All recipient-name variants seen for this EDRPOU.
    pub names: Vec<String>,
    pub overview: AnalyticsOverview,
    pub months: Vec<AnalyticsMonthRow>,
    pub top_products: Vec<AnalyticsGroupRow>,
    pub top_senders: Vec<AnalyticsGroupRow>,
    pub top_origin_countries: Vec<AnalyticsGroupRow>,
    pub product_sections: Vec<AnalyticsSection>,
    pub country_sections: Vec<AnalyticsSection>,
    pub price_sections: Vec<AnalyticsPriceMetric>,
}

/// Optional ordering for a result page: a result-field id and a direction.
/// When absent, the default recency order is used.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ResultSort {
    /// Result field id (matches `FieldInfo::id` from the schema/results).
    pub field: String,
    #[serde(default)]
    pub descending: bool,
}

pub type SearchPage = (Vec<i64>, Vec<Vec<String>>, Vec<Option<String>>);
pub type DynamicSearchPage = (
    Vec<FieldInfo>,
    Vec<i64>,
    Vec<Vec<String>>,
    Vec<Option<String>>,
);

pub fn analytics_should_run(q: &Query) -> bool {
    !q.is_empty()
}

#[cfg(test)]
mod tests {
    use super::{Filters, Query};

    #[test]
    fn query_deserializes_partial_filters() {
        let empty: Query = serde_json::from_str(r#"{"text":"","filters":{}}"#).unwrap();
        assert!(empty.filters.is_empty());

        let partial: Query =
            serde_json::from_str(r#"{"text":"","filters":{"year":"2026"}}"#).unwrap();
        assert_eq!(partial.filters.year, "2026");
        assert_eq!(
            partial.filters,
            Filters {
                year: "2026".to_string(),
                ..Filters::default()
            }
        );
    }
}
