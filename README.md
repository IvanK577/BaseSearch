# Base Search 2.0.0

[![CI](https://github.com/IvanK577/BaseSearch/actions/workflows/ci.yml/badge.svg)](https://github.com/IvanK577/BaseSearch/actions/workflows/ci.yml)

Base Search is a local desktop application for fast search, filtering,
analytics, and export across large Excel datasets.

It is built for people who have many large spreadsheet files and need to work
with them as one searchable database instead of opening heavy workbooks one by
one in Excel. Base Search is document-neutral: ordinary tabular Excel files are
imported with their real source columns as first-class fields, and optional
semantic profiles add better analytics when familiar business fields are
recognized.

Base Search runs locally. It does not upload spreadsheets, search results, or
the database to a cloud service.

## What It Does

- Imports `.xlsx`, `.xlsb`, and `.xls` files into one local SQLite database.
- Shows an import quality report with detected layout, header row, recognized
  semantic columns, preserved source columns, table fill rate, and warnings.
- Preserves every source column from the spreadsheet.
- Builds a full-text search index for fast repeated searches.
- Searches across products, companies, codes, invoice/order numbers, countries,
  brands, and any imported source columns.
- Supports advanced search rules: all/any groups, excluded rules, nested
  groups, ranges, empty/not-empty checks, and filters over imported source
  columns.
- Shows paged results instead of trying to render millions of rows at once.
- Opens a full details card for any row.
- Provides analytics for the current query and filters.
- Exports results to CSV or XLSX.
- Works offline on Windows, macOS, and Linux.

## Typical Use Cases

- Search across many Excel exports as one dataset.
- Find all rows related to a product, brand, SKU/code, company, country, or year.
- Compare which companies, SKUs/codes, brands, or countries dominate a selected
  result set.
- Inspect suspicious prices or unusual value-per-weight patterns.
- Prepare filtered CSV/XLSX extracts for further work in Excel, BI tools, or
  reports.
- Use generic Excel tables as searchable local data without writing SQL.

## Quick Start

### Windows

Run the prebuilt application from the distribution folder:

```text
dist\BaseSearch\BaseSearch.exe
```

A small launcher window opens, starts the local server, and opens the
workspace in your default browser. Two modes are offered:

- **Personal** (default) — binds `127.0.0.1` only; no sign-in, nothing is
  reachable from other machines.
- **Trusted LAN** — binds a private LAN/VPN address the launcher discovered, so
  colleagues on the same network can open the shown URL. Sign-in becomes
  mandatory and the server refuses to start until at least one account exists.

### macOS

Build and run from source:

```bash
xcode-select --install 2>/dev/null || true
git clone https://github.com/IvanK577/BaseSearch.git
cd BaseSearch
./start.sh
```

The `start.sh` script checks the environment, installs missing Rust tooling
when needed, builds the app, and launches it.

### Linux

Install Git first, then run the guided setup:

```bash
sudo apt-get update && sudo apt-get install -y git
git clone https://github.com/IvanK577/BaseSearch.git
cd BaseSearch
./start.sh
```

On Fedora use `sudo dnf install -y git`. On Arch use
`sudo pacman -S --needed git`.

## Browser Workspace (Local)

Base Search also ships a local browser workspace: the same import, search,
analytics, and export engine behind a modern web UI, served by a small server
that binds `127.0.0.1` only. Nothing is sent to a cloud.

Start it from the desktop binary or the CLI:

```text
BaseSearch --browser
base-search-cli browser path\to\base_search.db --port 7833
```

Options: `--host` (defaults to loopback; anything else explicitly opts in to
LAN exposure), `--port` (default `7833`), and `--no-open` to skip launching the
browser. The workspace covers search with filters and advanced rules, a
many-column results grid with a column picker, record cards, analytics
(overview, monthly dynamics, companies, goods, countries, prices, pivot, a
printable report, and side-by-side compare), a price-risk screen, per-company
dossiers, imports with a pre-import preview plus progress and history, CSV/XLSX
export, column mapping, and maintenance — all backed by the real API. Long
operations run as background jobs.

The frontend lives in `web-ui/` (Vite + React + TypeScript). Build it with
`npm install && npm run build` in `web-ui/`; the compiled assets are embedded
into the release binary.

### Sharing on a local network (optional)

The easiest path is the launcher's **Trusted LAN** mode. From the CLI, binding
any non-loopback host turns sign-in on. Create the first administrator locally
first, then start the server:

```text
base-search-cli user-add path\to\base_search.db admin --role admin
base-search-cli browser path\to\base_search.db --host 192.168.1.10
```

A networked server refuses to start with zero accounts. Accounts are
argon2-hashed and stored beside the database; sessions use HttpOnly cookies
with CSRF protection, and sign-in is rate-limited. Roles: **owner/admin**
(everything, including accounts and maintenance), **editor** (import and column
mapping), **viewer** (search, analytics, export — read-only). The last enabled
administrator can never be removed by accident.

The connection is **not encrypted** — keep it on a trusted LAN or behind a TLS
reverse proxy, and never expose it to the internet. Loopback use stays
password-free.

### Optional DuckDB OLAP

Build with `--features browser,duckdb-olap` to enable the columnar analytics
engine. Build a projection (from the Analytics screen or `base-search-cli
olap-build <db>`) and analytics run on DuckDB when the projection is fresh and
matches the SQLite totals, falling back to SQLite otherwise.

## Data Location

Base Search stores its database outside the executable.

Default locations:

- distribution folder: `data/base_search.db`
- fallback home folder: `~/.base-search/base_search.db`

Large real-world databases can grow to many gigabytes. Keeping the database
outside the executable makes updates and backups simpler.

## Basic Workflow

1. Open Base Search.
2. Click **Import Excel** and choose one or more files.
3. Wait until import and indexing finish.
4. Type a search query or add filters.
5. Use **Advanced** for structured search logic.
6. Review the result table.
7. Open row details when needed.
8. Use **Analytics** to understand companies, goods, countries, prices, pivots,
   reports, and comparisons for the current result set.
9. Export matching rows to CSV or XLSX when needed.

## Universal Tables

Base Search can import regular Excel tables even when they do not follow a
customs schema. Unknown columns are preserved as dynamic fields, included in
full-text search, shown in the result table, available in Advanced Search,
listed on the row card, and exported to CSV/XLSX.

## Analytics

Analytics are calculated from the same query and filters as the result table.

| Area | Purpose |
|---|---|
| Overview | Rows, declarations, companies, value, weight, average value per kg, codes, brands, and countries. |
| Companies | Recipients, senders, identifiers, totals, shares, and full lists. |
| Goods | Product codes, trademarks, product groups, values, weights, and participating companies. |
| Countries | Origin, dispatch, and trade countries counted separately. |
| Prices | Average and weighted price metrics, medians, quartiles, and possible undervaluation checks. |
| Pivot | Cross-tab analysis by company, code, country, month, year, or other supported dimensions. |
| Report | A compact working report that can be copied or saved as print-ready HTML. |
| Compare | Compare the current result set with another query or year. |

For very broad data, Base Search avoids running heavy analytics on an empty
global query by accident. Add a query or filter first.

## Export

Base Search can export the current result set to:

- CSV for large exports and compatibility with most tools;
- XLSX for smaller Excel-friendly exports.

XLSX export is limited by Excel worksheet limits. CSV is recommended for very
large result sets.

## Command-Line Tool

The distribution includes `base-search-cli` for diagnostics, maintenance, and
automation:

```powershell
base-search-cli stats  <db>
base-search-cli compact <db> [--vacuum]
base-search-cli peek   <file.xlsx|file.xlsb>
base-search-cli import <db> <file.xlsx|file.xlsb> [...]
base-search-cli search <db> [query...] [--limit N] [--year Y] [--code C]
base-search-cli analytics <db> [query...] [--year Y] [--code C] [--origin C]
base-search-cli benchmark <db> [query...] [--year Y] [--code C] [--origin C] [--repeat N]
base-search-cli olap-build <db> [projection.duckdb]
base-search-cli olap-benchmark <projection.duckdb> [query...] [--year Y] [--origin C]
base-search-cli export <db> <out.csv|out.xlsx> [query...]
```

The desktop app is the primary interface. The CLI is mainly for verification,
batch work, troubleshooting, and database maintenance.

`benchmark` runs practical scenarios for future OLAP and database-backend
decisions: search count, first result page, analytics overview,
company/product/country/price aggregations, pivot, and possible undervaluation
checks. Use `--json` for machine-readable output and `--allow-empty` only when
a full-database benchmark is intentional.

Optional DuckDB OLAP support can be enabled for technical comparisons and heavy
aggregate experiments:

```powershell
cargo build --features duckdb-olap --bin base-search-cli
base-search-cli olap-build data/base_search.db data/base_search.duckdb
base-search-cli olap-benchmark data/base_search.duckdb --year 2026 --json
```

SQLite remains the primary database and full-text search engine. DuckDB is used
as a separate analytical projection for columnar scans, grouping, pivots, and
repeatable backend comparisons.

## Database Maintenance

SQLite can temporarily use extra disk space after large imports, cancelled
imports, deletes, or migrations. This is normal.

Useful commands:

```powershell
base-search-cli stats data/base_search.db
base-search-cli compact data/base_search.db
base-search-cli compact data/base_search.db --vacuum
```

`compact` checkpoints and truncates the WAL file. `compact --vacuum` rewrites
the database to return unused pages to the filesystem. Vacuuming a large
database can take a long time and should be done after closing other Base
Search windows.

## Build From Source

Requirements:

- Rust stable
- Windows: MSVC toolchain
- macOS: Xcode Command Line Tools
- Linux: build tools, `pkg-config`, `libxkbcommon-dev`, and Wayland/X11 GUI
  libraries

Build and test:

```bash
cargo test
cargo build --release
cargo build --release --features duckdb-olap --bin BaseSearch --bin base-search-cli
```

Release binaries are created in `target/release/`:

- `BaseSearch` / `BaseSearch.exe`
- `base-search-cli` / `base-search-cli.exe`

Helper scripts for macOS and Linux:

```bash
./start.sh
./run.sh
./run.sh cli stats data/base_search.db
```

## Architecture

Base Search is built with:

- Rust for the application core and native executables;
- egui/eframe for the desktop interface;
- calamine for reading Excel files;
- SQLite for local storage;
- SQLite FTS5 for full-text search;
- SQLite aggregate queries for analytics;
- a benchmark command for repeatable search and OLAP baseline measurements;
- optional DuckDB projections for analytical backend experiments;
- xxhash for duplicate detection;
- CSV and XLSX writers for export.

The application is local-first: selected files are imported into a local
database, searched locally, analyzed locally, and exported locally.

## Privacy

Base Search has no cloud backend. It reads selected local files and writes a
local database. Users are responsible for protecting the files, exported
reports, and database on their own machine.

## Limitations

- Base Search is not a spreadsheet editor.
- It does not replace legal, accounting, compliance, or domain-expert review.
- Generic tables are searchable and exportable, but semantic analytics require
  recognizable fields such as dates, values, weights, companies, codes, or
  countries.
- Very large databases still need enough disk space and a reasonably fast SSD.

## Changelog

See [CHANGELOG.md](CHANGELOG.md) for release history.

## License

Base Search is released under the MIT License. You can use, copy, modify, and
redistribute the application and source code as long as the copyright notice
and license text are included.
