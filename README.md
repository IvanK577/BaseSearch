# Base Search 2.2.0

[![CI](https://github.com/IvanK577/BaseSearch/actions/workflows/ci.yml/badge.svg)](https://github.com/IvanK577/BaseSearch/actions/workflows/ci.yml)

Base Search turns spreadsheet folders into one fast, searchable local
workspace. Import tables, search every preserved source column, inspect rows,
build analytics, and export the result without sending the dataset to a cloud
service.

Base Search 2 is browser-first: the native launcher starts a server on your
computer and opens the workspace in your normal browser. Personal mode is the
default and needs no account. Trusted LAN mode is optional for a small team on
the same trusted network.

The Personal workspace is the recommended path for everyday use. Trusted LAN,
the command-line tool, and the legacy desktop workspace are advanced or
optional parts of the product.

## Start Here

Base Search is a portable folder — there is no installer and nothing to
register. Versioned ZIP packages appear on
[GitHub Releases](https://github.com/IvanK577/BaseSearch/releases) when a
release tag is published.

**The repository does not carry a prebuilt Windows folder.** It used to, and a
committed binary is exactly the thing that goes stale without anyone noticing:
the copy in `dist\` stayed at 2.0 while the source moved on, so the documented
quick start handed people an old build under a new version number. Worse, the
two builds disagree about the search-index version, and alternating between
them rebuilds the whole full-text index on every import. Take a release
package, or build the current source.

### Windows release package

1. Download the Windows ZIP from
   [GitHub Releases](https://github.com/IvanK577/BaseSearch/releases).
2. Extract the entire ZIP to a writable folder, keeping every file together.
3. Double-click `BaseSearch.exe` (or `Open Base Search.cmd`).
4. Leave **Personal workspace** selected and click **Start workspace**.
5. Wait for **Ready**. Base Search opens the workspace in your default browser
   at the Local URL shown by the launcher.

Keep the launcher open while you work. It shows the database path, the exact
local URL, startup progress, and any startup error. Run only one copy of
`BaseSearch.exe` at a time: a second launcher cannot share the port of the
first one, so close the previous window before starting again.

### Windows from source

When no release covers the commit you want, build the same package the release
pipeline builds:

```powershell
pwsh scripts/package-release.ps1
```

It writes `release_packages\BaseSearch-<version>-windows-x86_64\`, which is the
folder to run and the folder to copy to another machine. Prerequisites are in
[Build From Source](#build-from-source).

Every binary, from a release or from that script, runs on the SQLite + FTS5
engine. The optional DuckDB OLAP projection is not shipped; it is built only
from source with the `duckdb-olap` feature.

### macOS and Linux

Packaged `BaseSearch.app` and Linux `.tar.gz` builds are produced by the
release pipeline and attached to GitHub Releases together with checksums. Until
a release is published, build from source — see
[Build From Source](#build-from-source). `./start.sh` checks prerequisites,
builds everything, and launches Base Search at the end.

## Your First Search

1. Start a **Personal workspace**. Personal mode is for one person on this
   computer and does not ask you to register or sign in.
2. In the browser, open **Data**, then **Imports**.
3. Choose one or more table files. Review the detected sheets, columns, sample,
   and column meanings when a preview is available.
4. Queue the import. You may continue using existing data while the job runs.
   Open **Jobs** to see progress or an error.
5. Open **Search**, enter a word, code, company, identifier, or other value, and
   press **Search**. Leave the field empty only when you intentionally want an
   unfiltered result set.
6. Add simple filters or use the advanced rule builder for AND, OR, exclusions,
   ranges, empty values, and source-column conditions.
7. Open a row to inspect every imported field. Use **Analyze** for summaries,
   rankings, trends, pivots, comparisons, reports, company views, and price
   checks supported by the current data.
8. Open **Data**, then **Exports**, to create a CSV or XLSX file from the
   current query.

Base Search opens in the light theme. Use **Settings** to switch to the dark
theme or to choose one of the eleven available interface languages. The theme
is remembered per computer.

## Supported Data

Base Search accepts:

- Excel: `.xlsx`, `.xlsb`, `.xls`, and `.xlsm`
- OpenDocument spreadsheets: `.ods`
- delimited text: `.csv` and `.tsv`

There is no required customs, sales, inventory, or other fixed template. Base
Search preserves source columns, includes them in search, shows them in row
details, and keeps them available for export. A one-column text table is valid.
Macro-enabled workbooks are read as tables; Base Search does not execute
macros.

During import, Base Search detects table structure and records a quality report.
Optional column meanings tell analytics which fields represent a date, company,
product, country, value, currency, weight, or unit. Generic data remains fully
searchable and exportable when those meanings are not available; only the
specialized analytics that need them are limited.

The browser importer accepts up to 32 files in one request, 4 GiB per file,
and 16 GiB for the request. A workbook may contain up to 256 sheets; a table may
contain up to 16,384 columns. These are safety limits, not recommended working
sizes.

## Search And Results

- Full-text search uses SQLite FTS5 and searches all indexed source values.
- Advanced rules support nested all/any groups, exclusions, ranges, empty and
  non-empty checks, and direct source-column filters.
- Results are paged, so the browser never tries to draw millions of rows at
  once.
- Every result can open a full record card.
- All preserved source columns can be shown, filtered, sorted where supported,
  and exported.
- Re-imported identical rows are skipped by duplicate detection.
- Clearing or changing a query invalidates older in-flight browser results, so
  a slower previous request cannot replace the current one.

## Analytics

Analytics always use the same query and filters as Search, and the query can be
changed in place from a search box and filters at the top of the page — no need
to return to Search. Available views include an overview, monthly dynamics,
companies, products, countries, prices, pivots, comparisons, printable reports,
company dossiers, and possible undervaluation signals.

Each view is a working tool, not just a table:

- **Overview** shows headline metrics, a monthly chart, and top-5 previews of
  companies, goods, and countries that open the full tab.
- **Monthly dynamics** offers a metric and range selector, month-over-month and
  year-over-year change, a cumulative total, a totals row, and CSV export.
- **Companies, Goods, and Countries** are sortable, filterable, exportable
  ranking tables with a rank column, a row-share bar, a cumulative-share
  (Pareto) column and "top N = X%" callout, a minimum-share filter, copy to
  clipboard, and an adjustable result size.
- **Prices** reports median, average, weighted, percentiles, and the
  interquartile range with a compact distribution box-plot per metric.
- **Compare** puts the current query beside another product, company, or year
  and shows a signed difference table, with a control to swap the two sides.

Complex selections are supported directly: the advanced query builder matches
several products at once with the "is any of" operator and nested all/any (OR)
groups.

Base Search does not invent missing business meaning. A view is available only
when the dataset has suitable mapped columns. Currency totals are kept in
separate currency groups, and incompatible currencies are not presented as one
USD total; an unrecognized currency is labeled plainly rather than shown as an
internal placeholder. Weight units are normalized only when the unit is known.
Price-risk results are signals for review, not legal, accounting, or valuation
advice.

Production packages ship one local engine:

- **SQLite + FTS5** is the source of truth, the text-search engine, and the
  engine every number you see is computed by.

A second engine exists in the source: **DuckDB OLAP**, an analytical projection
for broad grouping, pivots, comparisons, and rollups. It is not part of a
release. It answers only queries that carry no free text, and its projection
goes stale as soon as anything is imported, with nothing rebuilding it
automatically — so it stopped helping almost immediately while adding a second
engine that has to agree with SQLite on every number. Build it from source with
the `duckdb-olap` feature if you want to experiment with it.

## Export

- Use CSV for very large result sets and broad compatibility.
- Use XLSX for an Excel-friendly workbook.
- XLSX output is subject to Excel worksheet limits. Choose CSV when the result
  may exceed them.
- Large exports run as background jobs and appear in **Jobs**.

## Personal And Team Use

### Personal workspace - recommended

Personal mode binds only to `127.0.0.1`. It is reachable from this computer,
does not require an account, and is the simplest choice for one person. The
launcher and browser are two parts of the same local application; the browser
does not mean the data is hosted online.

### Trusted LAN workspace - optional

Use LAN mode only when several people on the same trusted private LAN or VPN
must work with the same database:

1. In the launcher, stop the personal workspace if it is running.
2. Select **Trusted LAN workspace** and choose a private network interface.
3. Create the first **owner** account if the workspace has no accounts.
4. Confirm that the network is trusted, then start the workspace.
5. Share the displayed LAN URL with authorized people on that network.

LAN visitors must sign in. Roles are:

- `owner`: full control, including other owner accounts
- `admin`: workspace and account administration except owner management
- `editor`: search, analytics, imports, mappings, saved queries, and exports
- `viewer`: read-only search, analytics, and exports

LAN traffic uses ordinary **unencrypted HTTP**. Use it only on a trusted LAN or
inside a trusted VPN. Do not port-forward Base Search, bind it for public use,
or expose it directly to the internet. Public internet access requires a
separately administered TLS reverse proxy or another secure access layer; Base
Search does not configure that for you.

## Where Data Lives

For a new workspace, the default database is:

| Platform | Default location |
| --- | --- |
| Windows | `%LOCALAPPDATA%\Base Search\data\base_search.db` |
| macOS | `~/Library/Application Support/Base Search/base_search.db` |
| Linux | `$XDG_DATA_HOME/base-search/base_search.db` when set; otherwise `~/.local/share/base-search/base_search.db` |

The launcher always shows the exact database path. If a valid
`data/base_search.db` already exists beside the application, Base Search keeps
using that portable database for compatibility. When a new version finds one
older sibling package with an existing workspace, it asks before using it and
does not move or delete it.

Related files and folders can include:

- `base_search.db-wal` and `base_search.db-shm`: normal SQLite working files
- `base_search.auth.db`: LAN accounts and sessions
- `base_search.duckdb`: DuckDB analytical projection, only if you built one
  from a source build with the `duckdb-olap` feature
- `uploads/` and `exports/`: temporary job input and output
- `base_search.db.pre-upgrade-...bak`: verified backup retained after a
  structure-changing database upgrade

To make a manual backup, stop Base Search and copy the whole folder containing
the database. This keeps the database, LAN accounts, and related state together.

## Updates And Existing Databases

V2 opens compatible V1 SQLite databases in place; re-importing the original
files is not normally required. Keep your previous application folder and data
until the upgraded workspace has opened and your searches are verified.

Before a structure-changing upgrade, Base Search:

1. checks the source database;
2. requires free space equal to about twice the database plus WAL footprint,
   with 1 GiB extra headroom;
3. creates and verifies a backup beside the database;
4. applies the migration with visible progress;
5. verifies the upgraded database and retains the backup.

Large databases can take time to upgrade. Do not close the launcher while an
upgrade is active. If free space is insufficient, Base Search refuses to begin
the destructive step and leaves the original database unchanged.

## Troubleshooting

### The browser says it cannot connect

- Keep the launcher open.
- Wait until its status is **Ready**, then click **Open workspace**.
- If startup reports that the port is busy, stop the other Base Search process
  or choose another preferred port in the launcher and start again.
- Use the exact Local URL shown by the launcher instead of an old bookmark.

### Import did not finish

- Open **Jobs** and read the failed job message.
- Confirm that the extension is supported and that the file opens normally in
  its spreadsheet application.
- Make sure the disk containing the database has free space. Import needs room
  for the database, SQLite working files, and the uploaded copy.
- Retry only after the previous job reaches a final state. Cancelling an import
  rolls back its unfinished batch.

### Search finds too much or too little

- Clear visible filters as well as the text query.
- Check **Data > Columns** to confirm the imported source field and its meaning.
- Use quoted or more specific terms, or add an advanced rule for one column.
- Remember that specialized filters are unavailable until a suitable column is
  mapped.

### Analytics are missing or limited

- Import data first and apply a query or filter before requesting a very broad
  analysis.
- Map date, company, product, country, value, currency, weight, and unit fields
  under **Data > Columns** where appropriate.
- A first import into an empty database is the fastest way to load a large
  file: the read-side indexes are built once at the end instead of row by row.

### An update opens an empty workspace

Check the database path shown by the launcher. Do not delete the older folder.
If Base Search did not offer the correct sibling workspace, an advanced user
can start a specific database explicitly:

```text
BaseSearch.exe --browser --db "D:\path\to\base_search.db"
```

On macOS or Linux, use `BaseSearch` instead of `BaseSearch.exe`.

## Privacy And Security Limits

- Source files, imported records, searches, analytics, and exports stay on the
  machine running Base Search unless you deliberately share them.
- Base Search has no cloud database or cloud synchronization service.
- Your browser may have its own sync, extension, download, or history behavior;
  use a trusted browser profile for sensitive work.
- Anyone with sufficient access to the computer or database files may be able
  to read or copy the data. Use operating-system accounts, file permissions,
  disk encryption, and normal backup controls.
- Personal mode is not a security boundary against another process already
  running as the same operating-system user.
- LAN mode is not safe for direct public-internet exposure because it does not
  provide built-in TLS.

See [SECURITY.md](SECURITY.md) for the supported-release and vulnerability
reporting policy.

## Advanced Command Line

The release package includes `base-search-cli` for diagnostics, automation, and
maintenance. Run it with no arguments to see the complete current usage.

```text
base-search-cli stats <db>
base-search-cli compact <db> [--vacuum]
base-search-cli peek <table-file>
base-search-cli import <db> <table-file> [...]
base-search-cli search <db> [query...] [--limit N] [--year Y] [--code C]
base-search-cli analytics <db> [query...] [--year Y] [--code C] [--origin C]
base-search-cli export <db> <out.csv|out.xlsx> [query...]
base-search-cli browser <db> [--host 127.0.0.1] [--port 7833] [--no-open]
base-search-cli user-add <db> <username> --role owner
base-search-cli user-list <db>
base-search-cli user-remove <db> <username>
base-search-cli version
base-search-cli benchmark <db> [query...] [--repeat N] [--json]
```

Passwords entered interactively are hidden. `--password-stdin` exists for
explicit automation. `compact` checkpoints SQLite; `compact --vacuum` rewrites
the database to return unused pages to the filesystem and may take a long time.

## Build From Source

The tested release toolchain is Rust `1.96.0` and Node.js `22.22.0`. Compatible
newer stable versions may also work. A clean build needs Git, npm, the Rust
toolchain, and platform GUI development libraries.

- Windows: Visual Studio Build Tools with the C++ workload
- macOS: Xcode Command Line Tools
- Debian/Ubuntu Linux: a C/C++ toolchain, `pkg-config`, `libxkbcommon-dev`,
  Wayland development files, and the required XCB development packages

### Guided macOS or Linux build

```bash
git clone https://github.com/IvanK577/BaseSearch.git
cd BaseSearch
./start.sh
```

`start.sh` checks the platform, guides installation of missing Rust or Linux
GUI prerequisites, requires a current Node.js/npm installation, builds the
browser assets, builds Base Search, and launches it. `./run.sh` is the quieter
equivalent once prerequisites are installed.

### Manual build

```bash
npm --prefix web-ui ci
npm --prefix web-ui run build
cargo build --locked --release --no-default-features --features release-package --bin BaseSearch --bin base-search-cli
```

Run the application from `target/release/BaseSearch` or
`target\release\BaseSearch.exe`.

### Quality checks

```bash
npm --prefix web-ui ci
npm --prefix web-ui run typecheck
npm --prefix web-ui run test:unit
node web-ui/validate-i18n.mjs
npm --prefix web-ui run build
cargo fmt --all -- --check
cargo check --locked --all-targets --no-default-features --features release-package
cargo test --locked --all-targets --no-default-features --features release-package
cargo clippy --locked --all-targets --no-default-features --features release-package -- -D warnings
node --test scripts/release-package.test.mjs
```

Browser workflow tests use Playwright and the sample database preparation shown
in `.github/workflows/ci.yml`.

## Architecture

- Rust core for import, schema detection, query compilation, analytics, export,
  jobs, migrations, and native executables
- Axum/Tokio local HTTP server
- React 19, TypeScript, and Vite browser workspace
- SQLite as the local source of truth
- SQLite FTS5 for full-text search
- optional DuckDB projection for OLAP workloads, not part of released builds
- eframe/egui launcher and explicit legacy desktop fallback
- Calamine, the Rust CSV reader, and streaming XLSX/XLSB paths for input
- CSV and XLSX writers for export

## License

Base Search is available under the [MIT License](LICENSE). You may use, copy,
modify, and redistribute it as long as the copyright notice and license text
remain included.
