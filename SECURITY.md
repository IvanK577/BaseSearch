# Security policy

## Supported version

Base Search 1.0.0 is the supported desktop line. Report security issues privately using the repository's GitHub security advisory form.

## Data handling

Base Search stores its workspace in the operating system's application data directory. Imports, indexing, search, summaries and export run locally. Only share workspaces, source sheets and exports with people who may access their contents.

The import operates on the selected file, and the migration reads the 2.2.0 workspace from a read-only SQLite transaction. Use distinct locations for the original and migrated workspace. Back up the application data directory while the application is closed.

Download packages and their SHA-256 checksums from the repository release. Keep private data out of bug reports and use a small synthetic workbook to show the issue.
