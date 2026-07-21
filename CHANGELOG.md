# Changelog

All notable project changes are documented here.

## 2.0.0

- Usability pass over analytics for databases of any size: the company, goods,
  and country rankings show one full-height table at a time behind a compact
  section switcher instead of a tall stack; the monthly view lost its redundant
  second wall of stat cards; spacing across panels and tables was tightened; a
  permanently-zero "Documents" card is hidden.
- Trust fixes: a comparison snapshot disappears when the active query changes
  instead of being relabelled "Current"; a built pivot is dropped the moment
  the query or dimensions change; a failed row count shows "N+" instead of
  presenting a page size as the total; switching back to a cached analytics tab
  can no longer be overwritten by a slower stale request; analytics caches
  invalidate automatically when an import finishes.
- Whole-database scans are always an explicit choice: clearing all filters
  returns to the consent screen, and the price-risk screen now asks before
  scanning everything, exactly like Analytics.
- The price-risk screen is fully localized (all 11 languages via the
  translation layer) alongside the new analytics controls; the last Russian
  error string left in the importer was replaced with English.

- Completed the currency- and unit-safe analytics migration: the overview now
  computes real per-currency value buckets, per-unit weight buckets (normalized
  to kilograms), value-per-kg pairs, and exclusion counters; months, groups,
  and pivots carry the wire-compatible USD fields only when every valued row
  sits in one known USD cohort, so mixed-currency data can never be summed into
  a false total. The currency can come from a mapped column, a schema-level
  fixed value chosen at import, or the value column's own header ("Value USD");
  a pivot over mixed currencies says so instead of rendering a wrong matrix.
- Finished source-schema identity: direct source-field queries, sorting, cards,
  and exports are schema-exact (a same-named column in another file's schema
  never leaks in), the compatibility shape keeps the stable header-derived
  column ids the rest of the app was built on, and assigning a column meaning
  writes through to the registered schema fields.
- A bare numeric search ("8517") stays a global text search on tables without a
  product-code column instead of silently matching nothing.

- Added a local browser workspace: an Axum server on `127.0.0.1` that serves a
  new React/TypeScript UI over a JSON `/api` on top of the same core. Start it
  with `BaseSearch --browser` or `base-search-cli browser <db>`. It reuses the
  existing import, search, analytics, and export engines; no cloud, loopback
  only by default.
- Long operations (import, export, reindex, optimize, clear, optional DuckDB
  rebuild) run as background jobs with progress, so the UI never blocks; SQLite's
  single writer is respected by allowing one write job at a time.
- Rebuilt the frontend from scratch (Vite, React 19, TypeScript) with a
  fire-palette dark theme: search with filters/advanced rules, a many-column
  results grid, record cards, analytics, imports, exports, column mapping,
  settings, and a jobs view. Playwright smoke tests cover the main flows.
- Brought the remaining core capabilities into the browser: a per-company
  dossier reachable by clicking an EDRPOU anywhere, a price-risk screen that
  flags rows priced far below the code median, sortable result columns
  (numeric-aware, backed by the materialized typed columns), a right-click row
  menu (copy value/row, search this value, open company), saved searches and
  recent history, an analytics growth view with click-to-filter months, and a
  compact/VACUUM maintenance action.
- Live DuckDB OLAP analytics: with the `duckdb-olap` feature, analytics run on
  the columnar projection when it is fresh and projection-compatible, falling
  back to SQLite for text search, advanced queries, or a stale projection. A
  correctness guard only trusts the projection when its overview reproduces the
  SQLite totals, so a projection that cannot see a dataset's value columns never
  serves zeroed aggregates. The analytics view shows the active engine and can
  build/refresh the projection.
- Optional networked, multi-user mode: binding a non-loopback host requires
  sign-in. Local accounts (argon2, stored separately), server-side sessions with
  an HttpOnly cookie, and admin/viewer roles. Loopback stays password-free.
  Manage accounts from the CLI (`user-add`/`user-list`/`user-remove`) or the
  admin UI. Traffic is unencrypted, so LAN use is warned and a TLS reverse proxy
  is recommended.
- The interface now offers the engine's full language set (English, Ukrainian,
  German, Spanish, French, Polish, Portuguese, Romanian, Hungarian, Bulgarian,
  Chinese); English is the default. The landing screen is Search.
- Kept the production core intact: Excel import, universal columns, SQLite
  storage, SQLite FTS5 search, analytics, export, maintenance commands, and
  optional DuckDB OLAP experiments.
- Rewrote localized number parsing: scientific notation is honored, thousands
  groups with spaces or repeated separators are decisive, and the ambiguous
  lone `1.250` form is resolved per column meaning.
- Broad analytics on the SQLite path now stop after 60 seconds with an
  actionable message instead of pinning a worker thread indefinitely.
- Analytics reliability and design pass: clicking a month now applies a valid
  date-range filter instead of an unsupported `StartsWith`, which previously
  crashed the analytics view; search-input mistakes surface as a clear 400 in
  the UI instead of a scary "unexpected server error"; analytics results are
  cached per query so switching sub-tabs is instant and never re-runs an
  identical aggregation, with a request guard so a slow earlier response can no
  longer overwrite a newer one; and the monthly chart was rebuilt with a value
  axis, month labels, and a highlighted peak so it is actually readable.
- Brought the last desktop analytics features into the browser: a **Report**
  tab with a clean working summary (headline numbers plus top companies, goods,
  countries, and prices) that copies to the clipboard or opens as a printable /
  save-as-PDF page, and a **Compare** tab that runs the current query against
  another product, company, or the previous year side by side with a signed
  difference table.
- Added an import **Preview**: pick a spreadsheet and see its sheets, columns,
  and a sample row before importing, then import the same file in one click.
  Backed by a read-only `/api/imports/peek` endpoint.
- Translated the analytics labels that were still hardcoded in English (section
  titles, price and pivot labels, report and compare strings) through the i18n
  layer, with English and Ukrainian provided and English fallback for the rest.
- Made analytics genuinely usable, not just a top-N teaser: the company, goods,
  and country rankings are now full tables you can **sort by any column, filter
  by name, and export to CSV**, with a 50 / 200 / 500 "show top" control instead
  of a fixed dozen. The monthly breakdown table is sortable too (month-over-month
  stays anchored to the calendar), and the chart-metric toggle is labelled so it
  no longer looks like it should reorder the table. Every constraint narrowing
  the numbers — including a month or company drill — now shows as a removable
  chip with a Clear-all, so a drill is always visible and reversible. Pivot
  dimensions read as names ("Origin countries") instead of raw field ids.

## 1.6.5

- Added DuckDB-enabled release packaging for broad analytical workloads while
  keeping SQLite and SQLite FTS5 as the local source of truth and search
  engine.
- Added repeatable benchmark commands for search, analytics, pivots, and
  possible undervaluation checks.
- Preserved compatibility with existing local SQLite databases.

## 1.6.0

- Materialized typed and normalized values once at import: numeric amounts and
  weights, cleaned company labels, normalized country codes, and analysis
  month.
- Rewrote analytics to aggregate typed columns directly, reducing repeated
  text parsing during heavy aggregations.
- Country filters now compare against normalized key columns.
- Added a one-time migration that backfills the new columns for existing
  databases.

## 1.5.1

- Added import quality reporting for every completed file: detected layout,
  header row, source columns, recognized semantic columns, preserved extra
  columns, table fill rate, and warnings.
- Stored import quality metadata in `import_log` with automatic migration for
  existing databases.
- Extended the desktop import report and `base-search-cli stats` with quality
  details.

## 1.5.0

- Added universal table import: spreadsheets that do not match any known
  customs layout are imported as generic tables instead of being rejected.
- Preserved every generic source column as a dynamic field, indexed it for
  full-text search, and exposed it in Advanced Search.
- Switched desktop result pages to dynamic result columns so imported extra
  fields are visible directly in the main table.
- Updated full CSV/XLSX export to include dynamic imported columns, not only
  the fixed customs schema.
- Preserved extra-column order by first appearance in the source data.
- Added regression coverage for arbitrary non-customs Excel tables.

## 1.4.1

- Optimized startup migration for existing large databases so compatible FTS
  indexes are reused instead of being rebuilt unnecessarily.
- Added database storage reporting to `base-search-cli stats`.
- Added `base-search-cli compact <db> [--vacuum]` for safe WAL truncation and
  optional SQLite `VACUUM` compaction without deleting records.
- Ignored local release package folders so zip artifacts do not get committed
  accidentally.

## 1.4.0

- Added a flexible desktop Advanced Search builder with editable rule chips,
  all/any groups, exclusion rules and groups, nested groups, range filters,
  empty/not-empty checks, and extra-column conditions.
- Added a universal structured query model that keeps flat filters working
  while compiling advanced rules into parameterized SQLite queries.
- Added saved and recent search serialization for advanced queries, with
  backwards-compatible decoding for legacy saved searches.
- Added a field catalog that combines known record fields, the virtual year
  field, and extra headers discovered from imported spreadsheets.
- Localized Advanced Search controls, operators, hints, and summaries across
  all supported interface languages.

## 1.3.0

- Preserved columns beyond the known schema with each imported row, included
  them in full-text search, and exposed them on record cards.

## 1.2.0

- Added the context-aware Questions menu for routing common business questions
  into the correct analytics view.
- Expanded customs header hints and glossary coverage.
- Added printable reports, compare mode, and company dossier polish.

## 1.1.1

- Added guided first-run scripts for macOS and Linux.
- Added 11 interface languages and CJK font fallback.
- Centralized more UI strings in the translation layer.

## 1.1

- Added cross-platform support for Windows, Linux, and macOS.
- Reworked Analytics into focused sub-tabs and added pivot, company dossier,
  price-undervaluation scan, and CI builds.

## 1.0

- Initial public release with Excel import, SQLite/FTS5 search, filters,
  analytics, duplicate protection, CSV/XLSX export, and light/dark themes.
