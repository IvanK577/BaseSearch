# Base Search Security Policy

Base Search is local-first software that can contain sensitive business data.
This document explains which releases receive security fixes, how to report a
vulnerability, and the security boundary of Personal and Trusted LAN modes.

## Supported Versions

Security fixes are provided for the latest published Base Search 2.x release.
Users should update to the newest available 2.x patch before reporting a
problem that may already be fixed.

| Version | Security support |
| --- | --- |
| 2.0.x | Supported |
| Older 2.x release | Update to the latest 2.x release |
| 1.x and earlier | Unsupported |
| Development snapshots and modified builds | No guaranteed support |

## Reporting A Vulnerability

Do not publish exploit details, credentials, private databases, imported rows,
logs containing business data, or signing material in a public issue.

1. Open this repository's **Security** tab and choose **Report a
   vulnerability** when GitHub private vulnerability reporting is available.
2. If private reporting is unavailable, open a minimal public issue that asks
   the maintainer for a private reporting channel. Do not include technical
   vulnerability details in that issue.
3. Include the affected Base Search version and operating system, whether the
   issue affects Personal or Trusted LAN mode, reproduction steps using only
   synthetic data, and the expected security impact.

There is no guaranteed response SLA. Reports will be assessed as soon as
practical, and maintainers may need time to reproduce the issue, prepare a fix,
and validate release packages before public disclosure.

## Deployment Boundary

### Personal mode

Personal mode binds to `127.0.0.1` and does not require an application account.
It is intended for one person using one computer. It is not reachable directly
from other devices, but it is not a security boundary against another process
already running under the same operating-system user.

Use normal operating-system protections for sensitive data:

- a separate user account and screen lock;
- restricted file permissions;
- full-disk encryption;
- trusted browser extensions and profiles;
- protected backups and exports.

### Trusted LAN mode

Trusted LAN mode is optional. It requires a local account and accepts only a
selected private LAN or VPN interface. Accounts use Argon2 password hashing,
server-side expiring sessions, HttpOnly session cookies, SameSite cookies, CSRF
checks, login throttling, bounded password verification, and role-based access.

The transport is still **unencrypted HTTP**. Do not expose the Base Search port
directly to the public internet, do not configure router port forwarding, and
do not treat an untrusted Wi-Fi network as a trusted LAN. Credentials and data
can be observed by an attacker who can inspect unencrypted network traffic.

Remote access requires a separately administered secure layer, such as a
trusted VPN or a correctly configured TLS reverse proxy. Base Search does not
configure TLS certificates, public DNS, firewall policy, identity federation,
or internet-facing hardening for you.

### Roles

- `owner`: full access, including owner-account management
- `admin`: workspace and non-owner account administration
- `editor`: search, analytics, import, mappings, saved queries, and export
- `viewer`: read-only search, analytics, and export

The last active owner cannot be removed, disabled, or demoted. Changing a
password, role, or account status invalidates that user's existing sessions.

## Local Data

The main SQLite database stores imported records. A companion
`base_search.auth.db` file stores LAN accounts and sessions. Optional DuckDB
projections, SQLite WAL/SHM files, temporary uploads, generated exports, and
pre-upgrade backups can exist in the same data folder.

Treat the whole data folder as sensitive. Stop Base Search before making a
manual backup, and protect or securely remove old copies according to your own
retention policy. Deleting rows inside Base Search does not erase copies that
already exist in backups, exported files, browser downloads, filesystem
snapshots, or another user's device.

Base Search does not upload the database or imported files to a Base Search
cloud service. The user's browser and operating system can still have their own
sync, extension, telemetry, download, indexing, or backup behavior.

## Release Integrity

Use release archives and SHA-256 checksum files published by the project. The
stable tag pipeline is configured to require:

- Authenticode signing for Windows executables;
- Developer ID signing, hardened runtime, notarization, and stapling for the
  macOS application;
- package content checks and platform smoke tests;
- deterministic archive metadata where the platform permits it.

Local developer packages may be unsigned and must be labelled as such. Never
distribute a package that contains `.db`, WAL/SHM, DuckDB, spreadsheet, CSV,
credential, private-key, certificate, import, export, or private test-data
files.

## Safe Bug Reports

Use a small synthetic fixture that reproduces the behavior. Before attaching a
file, verify that it contains no real company names, personal data, identifiers,
prices, declarations, access tokens, usernames, passwords, cookies, file paths,
or other confidential metadata.

The project cannot guarantee the confidentiality of information posted in a
public issue, discussion, pull request, or CI log.
