// Wire types mirroring the Rust `/api` DTOs. Kept in one place so every page
// speaks the same shapes as the backend.

export interface Filters {
  year: string;
  product_code: string;
  trademark: string;
  description: string;
  sender: string;
  recipient: string;
  edrpou: string;
  trade_country: string;
  dispatch_country: string;
  origin_country: string;
}

export function emptyFilters(): Filters {
  return {
    year: "",
    product_code: "",
    trademark: "",
    description: "",
    sender: "",
    recipient: "",
    edrpou: "",
    trade_country: "",
    dispatch_country: "",
    origin_country: "",
  };
}

// Advanced query: serde external-tagging is mirrored exactly so the backend can
// deserialize `Query.advanced` without a custom format.
export type FieldRef =
  | { Column: string }
  | { Extra: string }
  | { SourceField: string };

export type ConditionOp =
  | "Contains"
  | "Equals"
  | "StartsWith"
  | "IsAnyOf"
  | "Range"
  | "IsEmpty"
  | "IsNotEmpty";

export type ConditionValue =
  | "None"
  | { Single: string }
  | { List: string[] }
  | { Range: { from: string | null; to: string | null } };

export interface QueryCondition {
  field: FieldRef;
  op: ConditionOp;
  value: ConditionValue;
  negated: boolean;
}

export interface QueryGroup {
  op: "And" | "Or";
  negated: boolean;
  children: QueryExpr[];
}

export type QueryExpr = { Condition: QueryCondition } | { Group: QueryGroup };

export type RecordScope = "canonical" | "occurrences";

export interface Query {
  text: string;
  filters: Filters;
  advanced?: QueryExpr | null;
  record_scope?: RecordScope;
}

export function emptyQuery(): Query {
  return { text: "", filters: emptyFilters(), record_scope: "canonical" };
}

export type FieldKind = "text" | "code" | "country" | "number" | "date" | "year";

export type FieldSource =
  | { kind: "column"; name: string }
  | { kind: "extra"; header: string }
  | { kind: "source_field"; field_id: string };

export interface FieldDto {
  id: string;
  label: string;
  kind: FieldKind;
  source: FieldSource;
  operators: ConditionOp[];
}

export interface RowDto {
  id: number;
  values: string[];
  duplicate_of?: string;
}

export interface SearchResponse {
  fields: FieldDto[];
  rows: RowDto[];
  offset: number;
  limit: number;
  has_next: boolean;
}

export interface CountResponse {
  total: number;
}

export interface KeyValue {
  label: string;
  value: string;
}

export interface RecordDto {
  id: number;
  source_file: string;
  fields: KeyValue[];
  extra: KeyValue[];
}

export type SemanticField =
  | "Date"
  | "DeclarationNumber"
  | "CompanyCode"
  | "Sender"
  | "Recipient"
  | "ProductCode"
  | "Description"
  | "Trademark"
  | "Country"
  | "OriginCountry"
  | "DispatchCountry"
  | "TradeCountry"
  | "Quantity"
  | "NetWeight"
  | "GrossWeight"
  | "Value"
  | "Currency"
  | "WeightUnit";

export type ColumnRole =
  | "Text"
  | "Number"
  | "Date"
  | "Year"
  | "Country"
  | "Code"
  | "Identifier"
  | "Money"
  | "Weight";

export interface SourceColumn {
  id: string;
  header: string;
  source_index: number;
  role: ColumnRole;
  semantic: SemanticField | null;
  storage: "SourceJson" | { SchemaColumn: string };
}

export interface SchemaResponse {
  search_fields: FieldDto[];
  result_fields: FieldDto[];
  columns: SourceColumn[];
  has_shape: boolean;
  total_rows: number;
}

export interface StorageInfo {
  database_bytes: number;
  wal_bytes: number;
  shm_bytes: number;
  freelist_pages: number;
  freelist_bytes: number;
  total_file_bytes: number;
}

export interface StatusResponse {
  version: string;
  db_path: string;
  total_rows: number;
  unindexed_rows: number;
  has_data: boolean;
  has_shape: boolean;
  lan_exposed: boolean;
  storage: StorageInfo;
  extra_headers: string[];
}

export interface DatabaseStats {
  total_rows: number;
  unindexed_rows: number;
  has_shape: boolean;
  import_count: number;
  last_import: string | null;
  storage: StorageInfo;
}

// Analytics ------------------------------------------------------------------

export interface AnalyticsOverview {
  row_count: number;
  declaration_count: number;
  distinct_senders: number;
  distinct_recipients: number;
  distinct_edrpou: number;
  distinct_trademarks: number;
  distinct_product_codes: number;
  distinct_origin_countries: number;
  distinct_dispatch_countries: number;
  distinct_trade_countries: number;
  total_value_usd: number;
  total_gross_kg: number;
  total_net_kg: number;
  total_quantity: number;
  avg_value_per_net_kg: number;
}

export interface AnalyticsFilterAction {
  field: string;
  value: string;
}

export interface AnalyticsGroupRow {
  label: string;
  rows: number;
  declarations: number;
  companies: number;
  total_value_usd: number;
  total_net_kg: number;
  total_gross_kg: number;
  total_quantity: number;
  share_percent: number;
  avg_value_per_net_kg: number;
  filter_action: AnalyticsFilterAction | null;
}

export type AnalyticsSectionKind =
  | "recipients"
  | "senders"
  | "edrpou"
  | "product_codes"
  | "trademarks"
  | "product_groups"
  | "origin_countries"
  | "dispatch_countries"
  | "trade_countries";

export interface AnalyticsSection {
  kind: AnalyticsSectionKind;
  rows: AnalyticsGroupRow[];
}

export type PriceMetricKind =
  | "value_per_net_kg"
  | "rfv_usd_kg"
  | "rmv_net_usd_kg"
  | "rmv_usd_extra_unit"
  | "rmv_gross_usd_kg"
  | "min_base_usd_kg";

export interface AnalyticsPriceMetric {
  kind: PriceMetricKind;
  count: number;
  average: number;
  minimum: number;
  maximum: number;
  weighted_average: number;
  median: number;
  p25: number;
  p75: number;
}

export interface AnalyticsMonthRow {
  month: string;
  rows: number;
  declarations: number;
  total_value_usd: number;
  total_net_kg: number;
}

export interface Analytics {
  overview: AnalyticsOverview;
  months: AnalyticsMonthRow[];
  company_sections: AnalyticsSection[];
  product_sections: AnalyticsSection[];
  country_sections: AnalyticsSection[];
  price_sections: AnalyticsPriceMetric[];
  top_recipients: AnalyticsGroupRow[];
  top_senders: AnalyticsGroupRow[];
  top_trademarks: AnalyticsGroupRow[];
  top_product_codes: AnalyticsGroupRow[];
  top_origin_countries: AnalyticsGroupRow[];
}

export type AnalyticsScope = "companies" | "products" | "countries" | "prices";

export interface AnalyticsEnvelope {
  engine: "duckdb" | "sqlite";
  data: Analytics;
}

export interface CompareSideRequest {
  label: string;
  query: Query;
}

export interface CompareSideEnvelope extends AnalyticsEnvelope {
  label: string;
  query: Query;
}

export interface CompareEnvelope {
  left: CompareSideEnvelope;
  right: CompareSideEnvelope;
}

export interface ProjectionInfo {
  rows: number;
  max_record_id: number;
  built_at: string;
  path: string;
}

export interface EngineStatus {
  duckdb_available: boolean;
  db_rows: number;
  db_max_record_id: number;
  projection: ProjectionInfo | null;
  projection_stale: boolean;
  projection_trusted: boolean;
  default_analytics_engine: "duckdb" | "sqlite";
}

export type PivotDim =
  | "recipient"
  | "sender"
  | "edrpou"
  | "product_code"
  | "trademark"
  | "origin_country"
  | "dispatch_country"
  | "trade_country"
  | "month"
  | "year";

export type PivotMetric = "value" | "rows" | "net_kg";

export interface PivotResult {
  row_labels: string[];
  col_labels: string[];
  // The flat matrix is present only when every contributing row shares one
  // known currency/unit; mixed data ships no matrix rather than false sums.
  cells?: number[][];
  row_totals?: number[];
  col_totals?: number[];
  grand_total?: number;
  metric_unit?: string;
  rows_truncated: boolean;
  cols_truncated: boolean;
  row_filterable: boolean;
  col_filterable: boolean;
}

export interface ResultSort {
  field: string;
  descending: boolean;
}

export interface CompanyProfile {
  edrpou: string;
  names: string[];
  overview: AnalyticsOverview;
  months: AnalyticsMonthRow[];
  top_products: AnalyticsGroupRow[];
  top_senders: AnalyticsGroupRow[];
  top_origin_countries: AnalyticsGroupRow[];
  product_sections: AnalyticsSection[];
  country_sections: AnalyticsSection[];
  price_sections: AnalyticsPriceMetric[];
}

export interface UndervaluedRow {
  id: number;
  declaration_date: string;
  declaration_number: string;
  recipient: string;
  sender: string;
  edrpou: string;
  product_code: string;
  description: string;
  source_value: number;
  net_kg: number;
  price_per_kg: number;
  code_median: number;
  code_p25: number;
  code_p75: number;
  code_sample_count: number;
  estimated_gap: number;
  ratio: number;
  cohort: RiskCohort;
  deviation_percent: number;
  confidence: RiskConfidence;
  reason: string;
  limitations: RiskLimitation[];
}

export type RiskConfidence = "low" | "medium" | "high";

export interface RiskLimitation {
  code: string;
  message: string;
}

export interface RiskCohort {
  product_code: string;
  period: string;
  currency: string;
  weight_unit: string;
  brand: string | null;
  country: string | null;
  dimensions: string[];
  sample_count: number;
  median: number;
  p25: number;
  p75: number;
  iqr: number;
  lower_fence: number;
  median_ratio_cutoff: number;
  robust_cutoff: number;
}

export interface PriceRiskContract {
  price_basis: string;
  period_granularity: string;
  required_dimensions: string[];
  optional_dimensions: string[];
  min_samples: number;
  max_median_ratio: number;
  iqr_multiplier: number;
  includes_subject_record: boolean;
}

export interface RiskExclusions {
  query_rows: number;
  missing_product_code: number;
  missing_period: number;
  missing_currency: number;
  missing_weight_unit: number;
  invalid_value: number;
  invalid_weight: number;
  insufficient_cohort: number;
}

export interface RiskCurrencyTotal {
  currency: string;
  flagged_rows: number;
  flagged_value: number;
  estimated_gap: number;
}

export interface Undervaluation {
  available: boolean;
  rows: UndervaluedRow[];
  checked_codes: number;
  checked_rows: number;
  flagged_rows: number;
  flagged_codes: number;
  flagged_value: number;
  estimated_gap: number;
  eligible_rows: number;
  evaluated_rows: number;
  checked_cohorts: number;
  contract: PriceRiskContract;
  exclusions: RiskExclusions;
  limitations: RiskLimitation[];
  currency_totals: RiskCurrencyTotal[];
}

// Jobs -----------------------------------------------------------------------

export type JobKind =
  | "import"
  | "export"
  | "optimize"
  | "compact"
  | "reindex"
  | "clear"
  | "olap_build";

export type JobStatus =
  | "queued"
  | "running"
  | "succeeded"
  | "failed"
  | "cancelled";

export interface JobProgress {
  phase: string;
  done: number;
  total: number;
  percent: number;
}

export interface Job {
  id: number;
  kind: JobKind;
  status: JobStatus;
  title: string;
  progress: JobProgress;
  message?: string;
  error?: string;
  result?: unknown;
  input?: unknown;
  cancellable: boolean;
  created_ms: number;
  updated_ms: number;
}

export interface ImportQuality {
  layout: string;
  header_row: number;
  source_columns: number;
  recognized_columns: number;
  extra_columns: number;
  non_empty_cells: number;
  empty_cells: number;
  filled_percent: number;
  warnings: string[];
}

export interface ImportLogEntry {
  file_name: string;
  total_rows: number;
  imported: number;
  duplicates: number;
  seconds: number;
  imported_at: string;
  quality: ImportQuality;
}

export interface ColumnPeek {
  index: number;
  id: string;
  header: string;
  sample: string;
  role: ColumnRole;
  semantic: SemanticField | null;
}

export interface SheetPeek {
  name: string;
  rows: number;
  cols: number;
  header_row: number;
  layout: string;
  columns: ColumnPeek[];
  signature: string;
  profile_suggestions: SourceMappingProfileCollection;
}

export interface WorkbookPeek {
  sheets: SheetPeek[];
}

export type FixedSemanticField = "Currency" | "WeightUnit";

export interface SourceMappingProfile {
  id: number;
  name: string;
  signature: string;
  mapping: (SemanticField | null)[];
  fixed_values: Partial<Record<FixedSemanticField, string>>;
  created_at: string;
  updated_at: string;
}

export interface SourceMappingProfileCorruption {
  id: number;
  reason: string;
}

export interface SourceMappingProfileCollection {
  profiles: SourceMappingProfile[];
  ignored_corrupt_rows: SourceMappingProfileCorruption[];
}

export interface SourceMappingProfileUpsert {
  id?: number;
  name: string;
  signature: string;
  mapping: (SemanticField | null)[];
  fixed_values: Partial<Record<FixedSemanticField, string>>;
}

export interface ImportFileResult {
  file_name: string;
  total_rows: number;
  imported: number;
  duplicates: number;
  seconds: number;
  error: string | null;
  cancelled: boolean;
  skipped_duplicate_of: string | null;
  quality: ImportQuality;
}

export interface ImportJobResult {
  files: ImportFileResult[];
  total_rows: number;
  imported: number;
  duplicates: number;
}

export interface ExportJobResult {
  file_name: string;
  token: string;
  rows: number;
  count: number;
  bytes: number;
  download_url: string;
  fields: { id: string; label: string }[];
  field_ids: string[];
  sort: ResultSort | null;
  query: Query;
  record_scope: RecordScope;
  format: "csv" | "xlsx";
}

export interface ApiErrorBody {
  error: { code: string; message: string };
}

export type UserRole = "owner" | "admin" | "editor" | "viewer";

export interface SessionUser {
  username: string;
  role: UserRole;
}

export interface AuthState {
  /// True when this server enforces sign-in (non-loopback bind).
  required: boolean;
  authenticated: boolean;
  user?: SessionUser;
}

export interface AccountInfo {
  username: string;
  role: UserRole;
  created_at: string;
}
