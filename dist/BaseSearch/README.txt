Base Search
===========

How to run
----------
Desktop app: double-click "Open Desktop App.cmd" or BaseSearch.exe directly.

Questions menu
--------------
Use the Questions button after entering a product, company, EDRPOU, year, or
country. It jumps straight to useful analytics: who imported it, what goods
dominate, which countries/routes are involved, how prices look, monthly
dynamics, pivots, full company lists, or a company dossier.

Column hints
------------
Hover table headers to decode abbreviated customs fields such as 43, 43_01,
FV, RFV, RMV, Vaga po MD, Umovy post., Mistse post, 3001, 3002, and 9610.

What this folder contains
-------------------------
- BaseSearch.exe: the desktop application.
- base-search-cli.exe: optional command-line diagnostics.
- Open Desktop App.cmd: starts the desktop application.
- data/: local database folder. It is created and used on the user's computer.

Database maintenance
--------------------
If data/base_search.db becomes much larger after big imports, close other
Base Search windows and run:

base-search-cli.exe stats data\base_search.db
base-search-cli.exe compact data\base_search.db

The compact command safely truncates the SQLite WAL file. For deeper
compaction, run:

base-search-cli.exe compact data\base_search.db --vacuum

The --vacuum mode keeps records but rewrites the database file. It can take a
long time on multi-gigabyte databases.

Benchmark / OLAP baseline
-------------------------
Use the benchmark command for repeatable search and analytics measurements:

base-search-cli.exe benchmark data\base_search.db Apple --year 2024 --repeat 3

It measures search count, first result page, analytics overview, company/goods/
country/price aggregations, pivot, and possible undervaluation checks on the
current SQLite backend. Use --json for machine-readable output.

Basic workflow
--------------
1. Open BaseSearch.exe.
2. Click Import Excel and select .xlsx, .xlsb, or .xls files.
3. Search by product, company, product code, declaration number, country,
   trademark, or any imported source column.
4. Use filters for year, code, EDRPOU, company, and country fields when those
   semantic fields exist.
5. Use + Filter and Advanced when a search needs several rules, any/all logic,
   excluded rules, ranges, empty/not-empty checks, or extra imported columns.
6. Use Questions when you want the app to choose the right analytics view.
7. Open Analytics to understand the current search: rows, declarations,
   companies, value, net/gross weight, average value per kg, product codes,
   brands, countries, and price indicators.
8. Double-click a row to see all imported fields; right-click for quick
   filters and the company profile.
9. Export matching rows to CSV or XLSX when needed.

Universal tables
----------------
Base Search can import regular Excel tables even when they do not follow the
customs schema. Unknown columns are preserved as dynamic fields, included in
full-text search, shown in the desktop result table, available in Advanced
Search, listed on the row card, and exported to CSV/XLSX.

Privacy
-------
Base Search works locally. It does not upload Excel files or databases to a
cloud service. Imported data is stored in data/base_search.db next to the
program.
