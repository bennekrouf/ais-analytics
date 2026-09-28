# Changelog

What changed in each release of **AIS Analytics**, the desktop app that follows
one correlation id across an Azure Log Analytics workspace.

The public version of this page — with the download for each release — lives at
<https://mayorana.ch/en/apps/ais-analytics/releases>. It is generated from this
file by `scripts/changelog_to_json.py`, so this file is the only place a
release note is written.

Format: [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).
Versioning: [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

Each heading is dated on the day its tag was pushed. Releases that carried only
build or packaging work say so rather than being hidden: the version numbers a
user sees in the update prompt should all be accounted for.

## [0.1.32] - 2026-09-25

### Changed

- Packaging only — no user-visible change.

## [0.1.31] - 2026-09-15

### Fixed

- Queries no longer fail on columns a table only appeared to have. The
  workspace scan samples tables together, and each one picked up its
  neighbours' columns as empty values: `AzureMetrics` looked like it had
  `OperationId`, `Level` and `Host`, and any query naming them was rejected.
  A column now counts only when the workspace declares it for that table.
  Scans saved by earlier versions are cleaned the same way when they load, so
  no rescan is needed.
- The Dependencies tab groups calls by their real target. A column that only
  mentions a host, such as `HostInstanceId`, no longer outranks `Target` or
  `RemoteHost` just because it appears in every table.
- A timestamp or id column is no longer mistaken for the message column.

## [0.1.30] - 2026-09-15

### Fixed

- Signals, Logs and Dependencies queries no longer fail on how a column name
  is capitalised. KQL rejects `operationname` against `OperationName`; every
  column is now queried with the table's own spelling.
- The severity filter in Logs no longer queries a table that lacks the
  severity column, for example `Level` from `FunctionAppLogs` being asked of
  `AppTraces`.
- An error rule that refers to a column a table does not have is left out of
  that table's failure count instead of failing the whole query.

## [0.1.29] - 2026-09-14

### Changed

- Packaging only — no user-visible change.

## [0.1.28] - 2026-09-14

### Added

- A banner at startup for occasional messages from us, such as a request for
  feedback. It is fetched once from mayorana.ch, stays until you dismiss it and
  is not shown again after that. If the notice cannot be fetched, no banner
  appears and startup is not slowed. Setting `DISABLE_UPDATE_CHECK` turns it off
  along with the update check.

## [0.1.27] - 2026-09-09

### Added

- A Signals tab: request rate, failures and latency over the selected window,
  with operations ranked by failures. Every row opens its run in Trace.
  Failures come from your own error rules. With no rules defined, or no
  duration column in the workspace, the tab says so next to the setting that
  fixes it instead of guessing.
- A Logs tab: the log stream filtered by severity and text, with every line
  leading to the run it belongs to.
- A Dependencies tab: the same view as Signals, grouped by call target —
  what the app calls out to, how often it fails and how long it takes.

### Changed

- The app now opens on Signals instead of Trace, so you start from what is
  going wrong rather than an empty search box.
- The notes for every release, with its download, are now published at
  [mayorana.ch/en/apps/ais-analytics/releases](https://mayorana.ch/en/apps/ais-analytics/releases).

## [0.1.26] - 2026-09-06

### Added

- Each running copy of the app gets its own webview data directory, so two
  windows opened at once no longer fight over one profile — on Windows the
  engine takes an exclusive lock on it, and the second instance would fail to
  start. Directories from past runs are pruned on launch, using a lock file
  each instance holds open rather than the directory's timestamp: a session
  left open overnight looks untouched from the outside.

### Fixed

- Signing in no longer leaves a zombie `az login` process behind on every
  attempt. The child is reaped, and its exit is also what tells the app the
  browser flow finished — or was abandoned.
- A corrupt cache file costs a cold start and nothing else, instead of a
  nonsensical "scanned N ago".

## [0.1.25] - 2026-09-03

### Changed

- CI pipeline only — no user-visible change.

## [0.1.24] - 2026-09-03

### Added

- Windows support: an installer, and CI that builds and publishes it with
  every release.

## [0.1.23] - 2026-09-02

### Changed

- The update check identifies itself with a versioned User-Agent and marks the
  request as coming from the updater, which is what separates a new install
  from an existing user updating in the download logs.

## [0.1.22] - 2026-09-02

### Fixed

- Long lane names are truncated with a tooltip carrying the full name, instead
  of overflowing the trace view.

## [0.1.21] - 2026-09-02

### Fixed

- A correlation id is bound to the field it was found in. Matching the same
  value in an unrelated column used to pull steps into a trace that were never
  part of it.

## [0.1.20] - 2026-09-02

### Added

- The update check resolves the artifact for the platform it is running on, so
  the banner offers the macOS, Windows or Linux build directly instead of the
  download page.

## [0.1.19] - 2026-08-31

### Changed

- Internal: Azure sign-in consolidated into one `start_login` path, so the
  welcome screen and the session check no longer implement it twice.

## [0.1.18] - 2026-08-31

### Changed

- The Windows installer runs with the lowest privileges by default, so it
  installs per-user without a UAC prompt. Machine-wide installation stays
  available through a command-line option.

## [0.1.17] - 2026-08-31

### Changed

- Packaging only — no user-visible change.

## [0.1.16] - 2026-08-29

### Fixed

- The app adopts the login shell's `PATH`, so `az` is found when it is launched
  from Finder or the Start menu rather than from a terminal.

## [0.1.15] - 2026-08-29

### Changed

- Packaging only — no user-visible change.

## [0.1.14] - 2026-08-29

### Changed

- Packaging only — no user-visible change.

## [0.1.13] - 2026-08-28

### Added

- Subscriptions that cannot be read, and sessions that have expired, are
  reported as what they are instead of showing as an empty workspace list.

---

Releases before 0.1.13 predate this file. Their tags remain on
[GitHub](https://github.com/bennekrouf/ais-analytics/releases).
