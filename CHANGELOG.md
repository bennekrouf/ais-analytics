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
