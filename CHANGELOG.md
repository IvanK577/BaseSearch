# Changelog

All notable changes to Base Search are documented in this file.

## 2.0.2 - 2026-07-24

A large analytics quality release: every Analyze tab gained genuinely useful,
well-integrated tools, the workspace never leaks internal placeholders, and the
whole surface stays responsive on multi-gigabyte databases.

### Added

- Added an in-page query bar to Analytics: a full-text box plus the same direct
  filters as Search, applied in one step. Analytics is now a self-contained
  analytical search tool — the query can be changed without leaving the page,
  and it is offered even on the whole-database consent screen.
- Enriched the Overview: lazy top-5 previews of companies, goods, and countries
  (each opens the full tab and warms its cache), additional supplier, trademark,
  and average-per-month metrics, and a one-click "copy summary".
- Reworked Monthly dynamics: a 12/24/all range selector, month-over-month and
  year-over-year change columns, a cumulative running total, a totals row, CSV
  export, and a peak/average/first-to-last context line.
- Enriched the company, goods, and country ranking tables: a rank column, a
  visual row-share bar, a cumulative-share (Pareto) column with a
  "top N = X% of rows" concentration callout, a sortable declarations column, a
  minimum-share filter, a totals row, and copy-to-clipboard alongside the
  existing sort, search, CSV export, and adjustable limit.
- Enriched Prices: a compact box-plot of each metric's distribution (robust
  Tukey scaling with an outlier marker), an interquartile-range column, CSV
  export, and a plain-language explanation of the price basis.
- Enriched Compare: a swap-sides control and additional difference rows for
  distinct products and origin countries.

### Changed

- Reworked the interface polish around a calm, minimalist fire palette: a subtle
  gradient on primary actions, a soft lift on stat cards, and small transitions
  on tabs, buttons, and rows.

### Fixed

- Fixed an internal `__unknown__` placeholder leaking into the interface for
  unmapped currencies and units; unknown values now read as a friendly label
  (and keep any original code) across analytics cards, tables, reports, and CSV
  export.
- Fixed the company dossier and analytics staying blank instead of guiding the
  user when the dataset has no recognized currency: a hint now links straight to
  the column mapping.
- Fixed enriched analytics failing with "server is busy" on large databases when
  several heavy reads collided: the analytics loader and the overview previews
  now retry a transient overload reply with a short backoff.
- Fixed the translation quality gate and kept all eleven interface languages
  complete for the new tools.

## 2.0.1 - 2026-07-22

### Fixed

- Fixed servers and the stats command stalling for many minutes on large
  databases while a search-index rebuild was pending: the "rows not yet
  indexed" check ran a correlated self-join over every row on every startup
  and every status request. It now uses two index-backed counts with the same
  exact result and answers in seconds even on tens of millions of rows.
- Fixed the one-time search-index rebuild losing all progress when the
  application was closed: chunk progress now commits together with a resume
  cursor, an interrupted rebuild continues where it stopped, and an import in
  between safely restarts it from scratch. Startup phases slower than one
  second are now reported in the log.

- Fixed the price-risk screen silently re-running its expensive analysis on
  every background poll (about every 1.5 seconds), which made the page flicker
  and kept the database busy. The analysis now runs exactly once per query and
  settings combination, and translation lookups no longer destabilize effect
  dependencies application-wide.
- Fixed the launcher keeping a dead workspace URL visible after a failed or
  stopped start; the URL is now a clickable link only once the server passes
  its health check, and shows as plain "starting" text before that.
- Fixed starting a second launcher silently picking a neighboring port while
  the first workspace keeps running; the launcher now refuses with a clear
  message naming the already-running workspace URL.
- Fixed the README quick start pointing only at unpublished GitHub Releases;
  it now documents the ready-to-run `dist\BaseSearch` folder shipped in the
  repository and warns against running two launchers at once.
- Fixed the CI translation gate failing on the new Portuguese strings; the
  affected label now uses sentence case, which the mojibake check accepts.

### Added

- Added price-risk work tools: sortable columns, confidence and currency
  filters, a free-text filter, a "companies with the most signals" summary
  with dossier links, CSV export of the visible rows, and a result-size
  selector — localized in all eleven languages.
- Added an end-to-end test that drives the advanced query builder against a
  live server: a contains-condition narrows results, NOT inverts it, and OR
  with a second condition widens it.

## 2.0.0 - 2026-07-22

Base Search 2 is a major browser-first release. Existing V1 SQLite workspaces
remain compatible and the legacy desktop workspace remains available as an
explicit fallback.

### Added

- Added a native launcher that starts the local server, reports database and
  migration progress, shows the exact local or LAN URL, and opens the browser
  only after the workspace is healthy.
- Added a React 19 and TypeScript browser workspace backed by a versioned Axum
  API. The primary areas are Search, Analyze, Data, and Settings, with jobs and
  related tools available in context.
- Added password-free Personal mode on `127.0.0.1` and optional Trusted LAN mode
  for private LAN or VPN use.
- Added local owner, admin, editor, and viewer accounts for LAN mode. Passwords
  are Argon2-hashed; sessions are server-side, bounded, expiring, HttpOnly, and
  protected by SameSite cookies and CSRF checks.
- Added background jobs for import, export, database maintenance, FTS repair,
  and DuckDB projection work. Jobs expose status, progress, stage, timestamps,
  cancellation, and errors without blocking existing searches.
- Added browser import preview, sheet selection, semantic column mapping,
  quality reports, import history, and progress for `.xlsx`, `.xlsb`, `.xls`,
  `.xlsm`, `.ods`, `.csv`, and `.tsv` files.
- Added one-column delimited-table support, stable generated columns for wider
  late rows, bounded upload metadata, workbook safety limits, and streaming
  preview paths for XLSX and XLSB.
- Added universal source-schema identity. All source columns are preserved and
  remain available to full-text search, direct filters, row cards, results,
  sorting where supported, and CSV/XLSX export.
- Added the browser advanced query builder with nested all/any groups,
  exclusions, ranges, empty checks, direct source fields, saved searches, and
  recent search history.
- Added analytics overview, monthly trends, company/product/country rankings,
  prices, pivots, comparisons, reports, company dossiers, and possible
  undervaluation signals for datasets with suitable column meanings.
- Added sortable and filterable full analytics lists with configurable limits
  and CSV export.
- Added a production DuckDB OLAP projection for broad analytical scans while
  retaining SQLite and FTS5 as the source of truth and search engine.
- Added eleven selectable interface languages: English, Ukrainian, German,
  Spanish, French, Polish, Portuguese, Romanian, Hungarian, Bulgarian, and
  Chinese.
- Added a dark default theme and a light theme option stored in the browser
  workspace settings.
- Added portable packages for Windows, macOS, and Linux, checksums, package
  smoke tests, and stable-tag signing requirements.

### Changed

- Made the browser workspace the default V2 experience. The native desktop
  workspace is available only through `--legacy-desktop` or the launcher's
  fallback action.
- Reorganized the browser navigation around four user tasks and removed account
  and sign-out noise from password-free Personal mode.
- Rebuilt the browser visual system as a restrained dark interface with compact
  controls, responsive layouts, accessible icon buttons, fixed table geometry,
  and a Base Search application mark.
- Changed new database locations to stable per-user platform folders. Existing
  portable `data/base_search.db` workspaces still take precedence, and sibling
  version workspaces are selected only after confirmation.
- Changed analytics totals to preserve currency and weight-unit meaning.
  Mixed currencies are never presented as one USD amount, and weight is
  normalized only for known compatible units.
- Changed DuckDB selection to a guarded fallback model: the projection is used
  only when current, compatible with the active query, and consistent with
  SQLite totals.
- Changed broad empty-query analytics and price scans to require explicit user
  consent.
- Changed imports and schema mapping so fixed currency or unit values remain
  associated with the correct source schema.
- Changed search responses to include a snapshot and total, allowing page and
  count requests to stay consistent while imports add rows.

### Fixed

- Fixed the company dossier "row share" column overstating shares: it now uses
  the company's full row count as the denominator instead of only the visible
  top rows.
- Fixed the price metrics table, company dossier, and advanced query builder
  showing English text regardless of the selected interface language.
- Fixed the workspace sidebar showing a hardcoded version instead of the
  server's actual version.
- Fixed notarized macOS packages failing layout verification because the
  stapled notarization ticket was not part of the expected package layout.
- Fixed stale search, analytics, pivot, and comparison responses overwriting a
  newer query or surviving after the query was cleared.
- Fixed mixed-currency data being displayed as a false USD total in SQLite,
  DuckDB, pivots, reports, and browser analytics.
- Fixed unknown or mixed weight units being labelled as kilograms.
- Fixed malformed numeric text such as arithmetic fragments, repeated decimal
  points, broken exponents, and invalid grouping from being concatenated into a
  plausible number.
- Fixed import cancellation reporting success after committing an unfinished
  partial batch; the active partial batch is now rolled back.
- Fixed late columns being silently discarded and one-column CSV/TSV files
  being rejected.
- Fixed semantic mapping changes leaving a stale DuckDB projection marked as
  current.
- Fixed Clear Database retaining old import source, source-column, or schema
  history.
- Fixed FTS repair accepting a stale schema watermark instead of rebuilding the
  complete index.
- Fixed large migration fingerprint rebuilds materializing every row in memory;
  they now run in bounded, checkpointed batches.
- Fixed first-run LAN ownership ambiguity and prevented removal, disabling, or
  demotion of the last active owner.
- Fixed login throttling, unbounded concurrent password checks, unlimited old
  sessions, per-request session database writes, malformed-token database work,
  and wildcard peer validation based only on the Host header.
- Fixed multipart metadata being allocated before its limit and ensured failed
  metadata parsing cleans up already uploaded temporary files.
- Fixed command-line password entry echo; explicit automation may use
  `--password-stdin`.

### Upgrade And Release Safety

- Added a pre-upgrade SQLite quick check, conservative free-space preflight,
  verified `VACUUM INTO` backup, retained recovery marker, visible migration
  progress, and a final integrity check for structure-changing upgrades.
- Required free space is approximately twice the database plus WAL footprint,
  with 1 GiB additional headroom. A failed preflight leaves the original
  database unchanged.
- Clean-clone start and packaging scripts now build the locked browser assets
  before compiling the embedded Rust application.
- Stable release tags require canonical versions, Authenticode on Windows, and
  Developer ID signing plus notarization and stapling on macOS. Local developer
  packages may remain unsigned and are labelled accordingly.
- Release archives exclude databases, spreadsheets, imports, exports,
  credentials, signing material, and private test data.

### Known Boundaries

- Trusted LAN traffic is unencrypted HTTP. It is intended only for a trusted
  private LAN or an already secured VPN, never direct public-internet exposure.
- DuckDB is an optional local analytical projection, not the text-search source
  of truth.
- PostgreSQL, ClickHouse, OpenSearch, Elasticsearch, cloud hosting, and public
  multi-tenant service operation are not production V2 engines.
- Price-risk output is a review signal and not professional valuation,
  accounting, legal, or compliance advice.

## 1.6.5

- Added DuckDB-enabled packaging and repeatable OLAP benchmarks while keeping
  SQLite and FTS5 as the source of truth and search engine.
- Preserved compatibility with existing local SQLite databases.

## 1.6.0

- Materialized typed and normalized values at import for faster analytics.
- Added normalized country keys and a migration for existing databases.

## 1.5.1

- Added import quality reports with layout, header, source-column, semantic
  mapping, fill-rate, and warning details.

## 1.5.0

- Added universal table import for arbitrary spreadsheet structures.
- Preserved dynamic source columns in search, results, details, and exports.

## 1.4.1

- Optimized startup migration and FTS reuse for large compatible databases.
- Added storage statistics and safe WAL/VACUUM maintenance commands.

## 1.4.0

- Added nested Advanced Search rules and a universal structured query model.
- Added saved and recent advanced queries and a combined source-field catalog.

## 1.3.0

- Preserved columns beyond the known schema and exposed them in full-text
  search and row details.

## 1.2.0

- Added question-driven analytics navigation, expanded column explanations,
  printable reports, comparison mode, and company dossier improvements.

## 1.1.1

- Added guided macOS/Linux first-run scripts, eleven interface languages, and
  broader font fallback.

## 1.1

- Added Windows, macOS, and Linux support and expanded desktop analytics.

## 1.0

- Initial release with Excel import, SQLite/FTS5 search, filters, analytics,
  duplicate protection, CSV/XLSX export, and light/dark themes.
