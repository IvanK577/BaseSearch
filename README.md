# Base Search 2.2.0

**English** | [Українська](README.uk.md)

[![CI](https://github.com/IvanK577/BaseSearch/actions/workflows/ci.yml/badge.svg)](https://github.com/IvanK577/BaseSearch/actions/workflows/ci.yml)

Base Search turns folders of spreadsheets into one fast, searchable local
workspace. It imports tables, preserves their source columns, searches across
all values, builds analytics, and exports filtered results. Your data stays on
the computer running Base Search unless you deliberately share it.

The Windows download is ready to run. You do **not** need Rust, Node.js, npm,
an installer, or a GitHub Release to use it.

## Windows: download and run

1. On the [Base Search repository](https://github.com/IvanK577/BaseSearch),
   click **Code**, then **Download ZIP**.
2. Extract the complete ZIP. Do not run the program from inside WinRAR, 7-Zip,
   or the Windows archive preview.
3. Open the extracted `BaseSearch-master\dist\BaseSearch` folder.
4. Double-click **Open Base Search.cmd**. You can also run `BaseSearch.exe`
   directly.
5. Leave **Personal workspace** selected and click **Start workspace**. Keep
   the launcher open while you work.
6. Wait until the launcher says **Ready**. The workspace opens in your default
   browser. If it does not, click **Open workspace** in the launcher.

That is the complete normal installation: download, extract, and open. The
application is portable and does not register an installer or Windows service.

The bundled Windows executable is currently unsigned. Windows may therefore
show a SmartScreen warning. Confirm that the ZIP came from this repository; if
you trust the download, choose **More info**, then **Run anyway**. The package
manifest records the SHA-256 hash of every shipped file.

Do not move only the `.exe` away from the package folder. Keep all six files
together. Run one launcher at a time; a second copy cannot use the same local
port.

## First import and search

1. Start a **Personal workspace**. Personal mode needs no account or password
   and accepts connections only from this computer.
2. In the browser, open **Data**, then **Imports**.
3. Select one or more supported table files. Review the detected sheets,
   columns, and preview when available.
4. Queue the import. Open **Jobs** to see progress or the exact failure message.
5. Open **Search**, enter a word, code, company, identifier, or other value,
   and press **Search**.
6. Add filters when needed. Open a result to inspect every imported source
   field.
7. Use **Analytics** for summaries, rankings, trends, pivots, comparisons, and
   price checks supported by the imported columns.
8. Open **Data**, then **Exports**, to create CSV or XLSX output from the current
   query.

Base Search supports these input formats:

- Excel: `.xlsx`, `.xlsb`, `.xls`, and `.xlsm`
- OpenDocument: `.ods`
- delimited text: `.csv` and `.tsv`

There is no required customs, sales, inventory, or other fixed template. A
one-column table is valid. Macro-enabled workbooks are read as tables; Base
Search does not execute macros.

The interface starts in the operating-system language when it is supported.
You can select English, Ukrainian, German, Spanish, French, Polish, Portuguese,
Romanian, Hungarian, Bulgarian, or Chinese under **Settings > Language**.

## What the GitHub ZIP contains

The ready-to-run Windows folder contains exactly these public files:

| File | Purpose |
| --- | --- |
| `Open Base Search.cmd` | Recommended double-click launcher |
| `BaseSearch.exe` | Native launcher, local server, and embedded browser interface |
| `base-search-cli.exe` | Optional command-line diagnostics, import, search, export, and maintenance |
| `README.txt` | Offline package instructions |
| `release-manifest.json` | Version, source revision, feature set, signing state, and SHA-256 hashes |
| `LICENSE` | MIT license |

No database or customer data is included. Base Search creates its data folder
when it first starts.

The repository also contains the source needed to audit and rebuild the same
application:

- `src/`: Rust application, server, import, search, analytics, and CLI
- `web-ui/`: React browser workspace
- `scripts/`: reproducible packaging and package verification
- `tests/`: Rust integration tests
- `.github/workflows/ci.yml`: build, test, package, and smoke-test workflow

Local build caches, old package versions, preview databases, audit drafts,
private datasets, exports, logs, `node_modules`, `target`, and
`release_packages` are intentionally excluded from GitHub. They are not needed
to run the bundled program and may contain machine-specific or private data.

## Search, analytics, and export

- SQLite is the source of truth and FTS5 provides full-text search across
  indexed source values.
- Advanced rules support nested all/any groups, exclusions, ranges, empty
  values, and direct source-column conditions.
- Results are paged; the browser does not try to render an entire large
  database at once.
- Re-imported identical rows are skipped by duplicate detection.
- Analytics use the same query and filters as Search.
- Available analysis includes overview metrics, monthly dynamics, companies,
  products, countries, prices, pivots, comparisons, reports, and company views
  when suitable columns are mapped.
- Currency totals remain separated when currencies are incompatible. Base
  Search does not silently present mixed currencies as one USD figure.
- CSV is recommended for very large exports. XLSX output is limited by Excel's
  worksheet limits.

Production packages use SQLite + FTS5. An experimental DuckDB OLAP projection
exists in the source tree behind the `duckdb-olap` feature, but it is not part
of the bundled Windows application.

## Where your data lives

The default database for a new personal workspace is:

| Platform | Default location |
| --- | --- |
| Windows | `%LOCALAPPDATA%\Base Search\data\base_search.db` |
| macOS | `~/Library/Application Support/Base Search/base_search.db` |
| Linux | `$XDG_DATA_HOME/base-search/base_search.db`, or `~/.local/share/base-search/base_search.db` |

The launcher always displays the exact database path. If a valid
`data/base_search.db` already exists beside the application, Base Search can
reuse that portable database for compatibility. The downloaded bundle itself
contains no `data` directory and no database.

Related runtime files may include:

- `base_search.db-wal` and `base_search.db-shm`: normal SQLite working files
- `base_search.auth.db`: accounts and sessions for Trusted LAN mode
- `uploads/` and `exports/`: background-job input and output
- `base_search.db.pre-upgrade-...bak`: a verified pre-upgrade backup

To make a manual backup, stop Base Search and copy the entire folder containing
the database. Do not copy a live database while the launcher is still running.

Before a structure-changing upgrade, Base Search checks disk space, creates and
verifies a backup, applies the migration, verifies the upgraded database, and
retains the backup. Keep the previous application and data until you have
confirmed that the upgraded workspace opens and searches correctly.

## Personal and Trusted LAN modes

**Personal workspace** is the recommended mode. It binds only to `127.0.0.1`,
needs no account, and is reachable only from the same computer. The browser is
just the interface for a local process; it does not mean the database is hosted
online.

**Trusted LAN workspace** is optional for several people on the same private
LAN or trusted VPN. LAN users must sign in. Available roles are `owner`,
`admin`, `editor`, and `viewer`.

LAN traffic is ordinary **unencrypted HTTP**. Never port-forward Base Search,
bind it for direct public access, or expose it to an untrusted network. Public
internet access requires a separately administered trusted VPN or TLS reverse
proxy; Base Search does not configure that layer.

## Troubleshooting

### Windows cannot find `BaseSearch.exe`

You are probably looking at the repository root instead of the runnable folder,
or the ZIP was only partially extracted. Open:

```text
BaseSearch-master\dist\BaseSearch\
```

That folder must contain all six files listed above. If `dist` is missing,
download the ZIP again from the `master` branch after this distribution change
and extract the whole archive.

### The browser cannot connect

- Keep the launcher open and wait for **Ready**.
- Click **Open workspace** and use the exact Local URL displayed there.
- If the port is busy, close the other Base Search launcher or select another
  preferred port and restart.
- Do not reuse an old bookmarked URL after the port has changed.

### Import did not finish

- Open **Jobs** and read the complete error.
- Confirm that the file extension is supported and the file opens normally in
  its spreadsheet application.
- Ensure that the disk containing the database has free space for the database,
  SQLite working files, and the uploaded copy.
- Retry only after the previous job reaches a final state.

### Search or analytics are incomplete

- Clear both the text query and visible filters.
- Open **Data > Columns** and check the imported source field and its meaning.
- Map date, company, product, country, value, currency, weight, and unit fields
  when specialized analytics need them.
- Generic columns remain searchable and exportable even without a specialized
  meaning.

### An update opens an empty workspace

Check the database path displayed by the launcher and do not delete the older
folder. An advanced user can start a specific database directly:

```text
BaseSearch.exe --browser --db "D:\path\to\base_search.db"
```

## Command line

The bundled `base-search-cli.exe` is optional. Run it from PowerShell or Command
Prompt inside `dist\BaseSearch`. Run it with no arguments for the complete
current usage.

```text
base-search-cli.exe stats <db>
base-search-cli.exe compact <db> [--vacuum]
base-search-cli.exe peek <table-file>
base-search-cli.exe import <db> <table-file> [...]
base-search-cli.exe search <db> [query...] [--limit N]
base-search-cli.exe analytics <db> [query...]
base-search-cli.exe export <db> <out.csv|out.xlsx> [query...]
base-search-cli.exe browser <db> [--host 127.0.0.1] [--port 7833] [--no-open]
base-search-cli.exe version
```

## macOS and Linux

The repository ZIP currently includes a ready-to-run build for Windows. macOS
and Linux users build from source with the guided launcher:

```bash
git clone https://github.com/IvanK577/BaseSearch.git
cd BaseSearch
./start.sh
```

`start.sh` checks prerequisites, installs locked frontend dependencies, builds
the browser assets and Rust application, and launches Base Search. `./run.sh`
is the quieter equivalent after prerequisites are installed.

## Build from source

The tested release toolchain is Rust `1.96.0` and Node.js `22.22.0`.
Compatible newer stable versions may work. A clean build needs Git, npm, the
Rust toolchain, and platform GUI development libraries.

On Windows, Visual Studio Build Tools with the C++ workload are required. To
build the same portable folder used by CI:

```powershell
pwsh scripts/package-release.ps1
```

The output appears under
`release_packages\BaseSearch-<version>-windows-<architecture>\`. This developer
command is not needed when using the checked-in `dist\BaseSearch` bundle.

Manual build:

```bash
npm --prefix web-ui ci
npm --prefix web-ui run build
cargo build --locked --release --no-default-features --features release-package --bin BaseSearch --bin base-search-cli
```

Quality checks:

```bash
npm --prefix web-ui ci
npm --prefix web-ui run typecheck
npm --prefix web-ui run test:unit
node web-ui/validate-i18n.mjs
npm --prefix web-ui run build
node --test scripts/release-package.test.mjs scripts/bundled-windows.test.mjs
cargo fmt --all -- --check
cargo check --locked --all-targets --no-default-features --features release-package
cargo test --locked --all-targets --no-default-features --features release-package
cargo clippy --locked --all-targets --no-default-features --features release-package -- -D warnings
```

## Privacy and security limits

- Base Search does not upload imported records, searches, or the local database
  to a Base Search cloud service.
- Your browser, extensions, synchronized profile, operating-system account, and
  exported files have their own security behavior. Use trusted software and
  disk encryption for sensitive data.
- Anyone with sufficient access to the computer or database files may be able
  to read or copy the data.
- Personal mode is not a security boundary against another process already
  running as the same operating-system user.
- Price-risk results are signals for human review, not legal, accounting,
  compliance, or valuation advice.

See [SECURITY.md](SECURITY.md) for vulnerability reporting and supported-version
policy.

## License

Base Search is available under the [MIT License](LICENSE).
