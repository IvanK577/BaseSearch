# Changelog

All notable changes to Base Search are documented in this file.

## 2.2.0 - 2026-08-16

The release that finally makes a second import fast. 2.1.0 claimed to fix the
slow-import report and did not: it fixed reading the file and the first bulk
load, while the cause sat elsewhere and only showed up once a database had
something in it.

### Fixed

- **Importing into a database that already holds data is dozens of times
  faster.** Every batch of rows was wrapped in a savepoint, and inside one
  SQLite has to journal the original content of every page the batch touches —
  so the cost of a row grew with the number of rows already in the batch.
  Measured end to end by alternating the old and new builds on one machine,
  each import into a fresh copy of the same 300 000-row database: a 50 400-row
  file took **506 seconds before and 5.6 seconds after**; separate 8 000-row
  files took 152 s and 106 s before against 2.2 s and 3.1 s after. The savepoint
  was never what undid a failed batch — the whole file is one transaction and
  any error rolls all of it back, which is unchanged and still covered by its
  tests.
- **The currency is worked out from the file, without being asked.** A customs
  export names no currency in any column, so every total used to read "unknown
  currency", the monthly value chart drew nothing at all, and value shares
  quietly fell back to counting rows. But the file states the same amount
  twice: `ФВ вал.контр` is the invoice value in the contract currency, and
  `РФВ` is that same value in dollars per kilogram — so multiplying `РФВ` by
  the weight reproduces the amount exactly when the contract is in dollars, and
  misses by the exchange rate when it is not. After each import that comparison
  is run over the imported rows, and the currency is recognized only when at
  least thirty rows can be compared and at least 95 % of them agree. Anything
  short of that stays unknown rather than guessing.

  A currency can still be stated by hand — Data → Columns, next to the column
  meanings — and a stated answer outranks the recognized one. Each imported file
  keeps its own answer, so a workspace holding several sources no longer merges
  their money into one figure.
- **The desktop stops adding money across currencies.** It printed a plain sum
  over every currency with no label at all. Every money figure it shows now
  carries its currency, or says the rows span several: the overview cards and
  the KPI tiles beside them, the ranking bars and their tooltips, the group
  table and the Excel clipboard export, the monthly chart's hover figures,
  Compare — including its difference column, which no longer subtracts euros
  from dollars — the company dossier, the report preview, the exported HTML and
  Markdown reports, and `base-search-cli analytics`.
- **A workspace holding two currencies still shows you numbers.** Group rows
  and months are now grouped by currency as well, so a company that has only
  ever traded in euros reports a euro total even when the workspace around it
  also holds dollars. A row that really does hold both now reports both, where
  it used to print a plain `0` — indistinguishable from having no money at all,
  and the figure a top-level row like a buyer or a product code would show.
  Before this, one mixed source emptied the money column of every row beside it.
- **And where a figure genuinely spans several, it says which.** "Several
  currencies" on its own is a refusal, not an answer, so the per-currency
  figures are now shown beside it: under the analytics header, in every ranking
  row's hover, and in the exported HTML and Markdown reports, which are read
  where nothing can be hovered.
- **A share of the total value is no longer a share of a meaningless sum.**
  Ranking rows divided each row's value by the sum over every currency. When a
  query spans more than one, the share is now computed on net weight, then on
  row count — the same ladder the query already used when there was no money at
  all.

- **An existing database is never mistaken for a workspace.** Choosing one only
  checked for a table named `records`, which other applications have too, and
  the choice was written down before the file was opened — so one wrong pick
  modified an unrelated database and then reopened it, failing, on every later
  start with no way to change it from inside the app. The choice is now recorded
  only after the database opens, and a file has to carry Base Search's own
  columns to be offered at all.
- **An existing workspace is found by what is in it, not what its folder is
  called.** Only folders named `BaseSearch-X.Y.Z` were recognised, so a version
  1 database in a folder of your own naming was invisible and Base Search
  started empty without a word. A database somewhere else entirely can now be
  chosen by hand.
- Duplicate rows always point back at the file that brought the row in first,
  and the lookup that decides this no longer sorts rows it discards.

### Changed

- **A first-open upgrade says what it is doing.** Opening a database from an
  older version rebuilds it, which is minutes of work on a large one, and the
  window used to show only a spinner and a rising seconds counter. It now names
  each step — backing up, verifying, upgrading, recomputing — with a progress
  bar for the two long ones and a note that this runs once.
- The startup screen and both workspace prompts are translated into all eleven
  languages. They were English only, and they are where you decide what happens
  to your data.
- **The launcher window is translated too.** It is the first thing that opens
  and, in the default browser mode, the only window with any settings in it —
  and all of it was English: the two workspace modes, the port, the trusted-
  network warning, the network interface picker, creating the first owner
  account, the addresses, every button, and every refusal it can give you.
  Its refusals are now values rather than sentences, so they are worded in the
  reader's language and the tests name the case instead of matching English
  prose — a test that checks the wording keeps passing after the wording stops
  being English.
- **The repository no longer carries a prebuilt Windows folder.** The copy in
  `dist\` had drifted to an older build while the documented quick start still
  pointed at it, and mixing the two rebuilt the whole search index on every
  import. Use a release package or `scripts/package-release.ps1`.

## 2.1.1 - 2026-08-09

An interface release. Nothing about importing, searching, or analysing changed
— the engine, the database format, and every number are identical to 2.1.0.

### Changed

- **The light theme is now the default.** It is also the base the interface is
  built on rather than a variant applied afterwards, so the first frame is
  already correct and no longer flashes the other theme while starting. The
  dark theme is unchanged and still one click away in Settings; if you had
  already chosen it, your choice is kept.
- The interface is drawn with rules instead of floating cards. Sections on
  Search and Analyze now run the full width of the window and share a dividing
  line with the section below, which puts more rows on screen. Settings, Data,
  and the job pages keep their narrower centred column.
- Corners are square throughout, borders are graded into three weights, and
  section labels, stat labels, and chart axes are set in a monospaced face, so
  a label is distinguishable from a value at a glance.
- The smallest text rises from 9px to 11px. Nothing you have to read is set
  below that any more.
- Growth and difference columns are green for up and red for down. They were
  amber and red, two shades of the same warning colour.

### Fixed

- A switch in the "on" position no longer shows green. Green means an action
  succeeded; an enabled switch only reports the current state, so every switch
  in the app — including the theme toggle in Settings — looked like a status
  light reporting success.
- Bars in the monthly chart grow from the axis instead of from their own
  middle, so a bar no longer briefly extends below the baseline it is measured
  against.
- The import progress bar and the loading placeholders no longer recalculate
  the page layout on every frame they animate. This was running for the whole
  duration of an import.
- Controls that changed appearance instantly on hover or click now do so
  consistently; twenty-one of them had no transition at all.
- The "reduce motion" system setting is honoured more precisely: movement and
  looping animation stop, colour feedback stays, and loading indicators keep
  turning — a frozen spinner reads as a hang, not as reduced motion.

## 2.1.0 - 2026-08-02

A correctness and speed release driven by two standing reports: that importing
is very slow, and that search and analytics do not show all the data.

### Fixed

- Money, weight, and value per kilogram now appear in every analytics table.
  The totals were being computed and then dropped on the way to the browser, so
  all nine ranking sections, the monthly table, Compare, the report preview, and
  the company dossier showed an em dash while the overview card showed numbers.
- The importer is no longer lost when a file names its recipients twice. Two
  columns that both mean "recipient" used to cancel each other out, which left
  the "Recipients / importers" section empty and the company dossier without a
  name while the EDRPOU section kept working.
- The Overview company card now previews importers by name instead of listing
  registration codes.
- Search finds a row by its EDRPOU code, contract number, delivery place,
  customs office, or ZED purpose. Those columns were absent from the index, and
  an eight-digit company code was additionally being read as a product code.
- A value column written as "1200.75 USD" or "1.234,56" is recognized at import.
  Detection judged numbers with its own parser, stricter than the one the query
  engine uses, and refused the column outright.
- Monthly dynamics covers the whole archive rather than the last four years.
- Excel exports carry numbers as numbers, so SUM() works; product codes stay
  text so leading zeros survive; negative values are no longer quoted into
  unreadable text in CSV.
- The desktop export writes the columns shown on screen, and its file dialog
  offers every format the importer accepts.
- Analytics no longer shows the previous query's numbers when a request fails.
- The desktop window no longer freezes when the help window is open during an
  import, and a failed background task releases the toolbar instead of leaving
  it disabled until restart.

### Changed

- A first import into an empty database builds its indexes once at the end
  instead of maintaining them row by row, and five indexes no query ever read
  were removed.
- A workbook is opened once per file rather than once per sheet, a delimited
  file is hashed as it is parsed, and a previewed file is no longer uploaded a
  second time to import it.
- Import progress reports the file hashing phase and knows how many rows a
  delimited file holds, so the bar is no longer stuck at an unknown total.
- Analytics computes its overview and month series once per request instead of
  once per scope, the Overview preview cards arrive in one request, and search
  no longer runs a second count or scans the table on every page.
- Saved column mappings are applied to a multi-file import by matching each
  sheet's column signature.
- `base-search-cli version` reports the features the binary was built with, and
  the packaging smoke tests refuse a binary that was not built as a release.
- The DuckDB analytics projection is no longer part of the shipped feature set.
  It only answered queries without free text and went stale on the first import,
  with nothing to rebuild it automatically. The feature and its parity tests
  remain in the source.

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
