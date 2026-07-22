Base Search 2.0
===============

Windows first start
-------------------
1. Keep every file in this folder together.
2. Double-click BaseSearch.exe.
3. Leave "Personal workspace" selected.
4. Click "Start workspace" and keep the launcher open.
5. Wait for "Ready". Base Search opens in your default browser.
6. Open Data, then Imports, and choose a supported table file.
7. Wait for the import job to finish, open Search, and enter a value.

Personal mode is password-free and available only on this computer. The browser
is the main interface, but the server and database remain on this computer.

If the browser cannot connect, return to the launcher, wait for Ready, and click
"Open workspace". Use the exact Local URL shown there instead of an old
bookmark.

Normal workflow
---------------
  Import -> Search and filter -> Inspect rows -> Analyze -> Export

Supported imports: XLSX, XLSB, XLS, XLSM, ODS, CSV, and TSV.
Supported exports: CSV and XLSX.

Base Search does not require one fixed table layout. It preserves source
columns for search, row details, filters, and export. Optional column meanings
enable specialized analytics for dates, companies, products, countries,
values, currencies, weights, and units.

Imports and large exports run in the background. Open Jobs to see progress or
the exact error. Use CSV when a result may exceed Excel worksheet limits.

Data and backups
----------------
The launcher shows the exact database path. A new Windows workspace normally
uses:

  %LOCALAPPDATA%\Base Search\data\base_search.db

If data\base_search.db already exists beside BaseSearch.exe, Base Search keeps
using it as a portable workspace. When an existing workspace is found in one
older sibling package, Base Search asks before using it and does not move or
delete it.

To make a manual backup, stop Base Search and copy the complete folder that
contains base_search.db. Keep any base_search.auth.db, base_search.duckdb, WAL,
SHM, and pre-upgrade backup files with it.

Before a structure-changing database upgrade, Base Search checks the database
and free space, creates and verifies a backup beside it, applies the upgrade
with visible progress, and verifies the result. It needs about twice the
database plus WAL footprint and 1 GiB extra headroom. If space is insufficient,
the original database is not changed.

Trusted LAN mode (optional)
---------------------------
Use Trusted LAN only when authorized people on the same trusted private LAN or
VPN need one shared workspace. Select a private interface in the launcher,
create the first owner account, confirm the network is trusted, and share the
displayed LAN URL.

LAN traffic is unencrypted HTTP. Never expose the Base Search port directly to
the public internet and never use LAN mode on an untrusted network. Secure
remote access requires a separately administered trusted VPN or TLS reverse
proxy.

Command line (advanced)
-----------------------
base-search-cli.exe is included for diagnostics, maintenance, and automation.
Run it with no arguments to see the complete current usage.

Common examples:

  base-search-cli.exe stats "C:\path\to\base_search.db"
  base-search-cli.exe compact "C:\path\to\base_search.db"
  base-search-cli.exe peek "C:\path\to\table.xlsx"
  base-search-cli.exe search "C:\path\to\base_search.db" example
  base-search-cli.exe export "C:\path\to\base_search.db" result.csv example

Use compact --vacuum only after closing other Base Search windows. It rewrites
the database to return unused pages to disk and can take a long time.

Privacy and limits
------------------
Base Search does not upload source files, imported records, searches, or the
database to a Base Search cloud service. Protect the database, backups, browser
profile, downloaded exports, and Windows account as sensitive data.

Base Search is not a spreadsheet editor and does not replace legal,
accounting, compliance, or valuation review. Price-risk output is a signal for
human review, not a professional conclusion.
