Base Search 2.2.0
==================

Package: windows / x86_64

First start
-----------
1. Extract the complete archive before starting Base Search.
2. Run Open Base Search.cmd or BaseSearch.exe.
3. In the launcher, leave "Personal workspace" selected.
4. Click "Start workspace" and keep the launcher open.
5. Wait for "Ready". The workspace opens in your default browser.
6. Open Data, then Imports, and choose a supported table file.
7. Wait for the import job to finish, then open Search and enter a value.

Personal mode is password-free and works only on this computer. The browser is
the primary Base Search 2 interface; it is connected to a server running on the
same computer. No Base Search cloud service is involved.

If the browser does not open, wait for Ready and click "Open workspace" in the
launcher. Use the exact Local URL shown there. A connection-error page normally
means the launcher is closed, the server is still starting, or an old URL was
opened.

Supported data
--------------
Base Search imports XLSX, XLSB, XLS, XLSM, ODS, CSV, and TSV tables. It does not
require one fixed schema. Source columns are preserved for search, row details,
and export. Optional column meanings enable specialized analytics for dates,
companies, products, countries, values, currencies, weights, and units.

The normal workflow is:

  Import -> Search and filter -> Inspect rows -> Analyze -> Export

Imports and large exports run as background jobs. Use Jobs to see progress or
the exact error. CSV is recommended for exports that may exceed Excel worksheet
limits.

Data location and backups
-------------------------
New workspaces use a stable per-user database location:

  Windows: %LOCALAPPDATA%\Base Search\data\base_search.db
  macOS:   ~/Library/Application Support/Base Search/base_search.db
  Linux:   $XDG_DATA_HOME/base-search/base_search.db when XDG_DATA_HOME is set;
           otherwise ~/.local/share/base-search/base_search.db

The launcher always shows the exact path. If data/base_search.db already exists
beside this package, Base Search keeps using that portable database. If one
older sibling package contains a compatible workspace, Base Search asks before
using it and never moves or deletes it.

To make a manual backup, stop Base Search and copy the entire folder containing
the database. This also preserves LAN accounts and related state.

A structure-changing upgrade checks the database and free space, creates and
verifies a backup beside the database, applies the upgrade with visible
progress, verifies the result, and retains the backup. The preflight requires
about twice the database plus WAL footprint and 1 GiB of additional headroom.
If space is insufficient, the original database is left unchanged.

Trusted LAN mode (optional)
---------------------------
LAN mode lets authorized people on the same trusted private LAN or VPN use one
workspace. Select Trusted LAN in the launcher, choose a private interface,
create the first owner account, confirm the network is trusted, and share the
displayed LAN URL.

LAN traffic is unencrypted HTTP. Never expose the Base Search port directly to
the public internet or use it on an untrusted network. Internet access requires
a separately administered trusted VPN or TLS reverse proxy; Base Search does
not configure that security layer.

Analytics engines
-----------------
SQLite and FTS5 are the source of truth, the text-search engine, and the engine
every number in this package is computed by. There is no second analytics
engine: an optional DuckDB projection exists in the source tree but is not
built into a release, because it answered only queries carrying no free text
and went stale the moment anything was imported.

Command line (advanced)
-----------------------
Run base-search-cli.exe from PowerShell or Command Prompt.

Run the command-line tool with no arguments to see its current usage. Common
commands are:

  base-search-cli stats <db>
  base-search-cli compact <db> [--vacuum]
  base-search-cli peek <table-file>
  base-search-cli import <db> <table-file> [...]
  base-search-cli search <db> [query...]
  base-search-cli export <db> <out.csv|out.xlsx> [query...]

On Windows, use base-search-cli.exe. Password entry for account commands is
hidden; --password-stdin is reserved for explicit automation.

Privacy and limits
------------------
Base Search does not upload source files, imported records, searches, or the
database to a Base Search cloud service. Protect the database, backups, browser
profile, downloaded exports, and operating-system account as sensitive data.

Base Search is not a spreadsheet editor and does not replace legal,
accounting, compliance, or valuation review. Price-risk output is a signal for
human review, not a professional conclusion.

Release provenance
------------------
Version: 2.2.0
Source revision: 2afff7ca0252
Features: browser
Source date epoch: 1787151218

This portable local Windows build is unsigned and does not require installation.
