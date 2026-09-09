#!/usr/bin/env bash
# release.sh — cut a release for ais-analytics
#
# Usage:
#   ./scripts/release.sh            # auto-bump patch
#   ./scripts/release.sh 0.2.0      # explicit version
#   ./scripts/release.sh --minor    # bump minor
#   ./scripts/release.sh --major    # bump major
#   ./scripts/release.sh --dry-run  # show plan only

set -euo pipefail

CARGO="Cargo.toml"
DRY_RUN=false

CARGO_VERSION=$(grep '^version' "$CARGO" | head -1 | sed 's/version = "\(.*\)"/\1/')

# Base the bump on the highest tag ever pushed, not on Cargo.toml's version on
# this branch — a release cut elsewhere (a workflow_dispatch run, a branch
# that never merged back) leaves Cargo.toml stale here, and bumping off it
# recomputes a tag that already exists.
LATEST_TAG=$(git tag -l 'v*' | sed 's/^v//' | sort -V | tail -1)
CURRENT="${LATEST_TAG:-$CARGO_VERSION}"
IFS='.' read -r MAJOR MINOR PATCH <<< "$CURRENT"

case "${1:-}" in
    --dry-run)            DRY_RUN=true; NEW="$MAJOR.$MINOR.$((PATCH + 1))" ;;
    --patch|"")           NEW="$MAJOR.$MINOR.$((PATCH + 1))" ;;
    --minor)              NEW="$MAJOR.$((MINOR + 1)).0" ;;
    --major)              NEW="$((MAJOR + 1)).0.0" ;;
    [0-9]*.[0-9]*.[0-9]*) NEW="$1" ;;
    *)
        echo "Usage: $0 [--patch|--minor|--major|--dry-run|<version>]"
        exit 1
        ;;
esac

TAG="v$NEW"

# ── Pre-flight checks ─────────────────────────────────────────────────────────
ERRORS=()

if [[ ! "$NEW" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
    ERRORS+=("Version must be semver (e.g. 1.2.3), got: $NEW")
fi

if git rev-parse "$TAG" &>/dev/null; then
    ERRORS+=("Tag $TAG already exists")
fi

if [[ -n "$(git status --porcelain)" ]]; then
    ERRORS+=("Working tree is dirty — commit or stash first")
fi

if [[ ${#ERRORS[@]} -gt 0 ]]; then
    for e in "${ERRORS[@]}"; do echo "  $e"; done
    exit 1
fi

# ── Show release plan ─────────────────────────────────────────────────────────
echo ""
echo "  Release plan"
echo "  ============"
echo "  Cargo.toml : $CARGO_VERSION -> $NEW"
echo "  Git tag    : $TAG"
echo "  Branch     : $(git branch --show-current)"
echo ""

LAST_TAG=$(git describe --tags --abbrev=0 2>/dev/null || echo "")
if [[ -n "$LAST_TAG" ]]; then
    COUNT=$(git log "$LAST_TAG"..HEAD --oneline | wc -l | tr -d ' ')
    echo "  Commits since $LAST_TAG: $COUNT"
    git log "$LAST_TAG"..HEAD --oneline --no-decorate | sed 's/^/    /'
else
    echo "  (no previous tag — first release)"
fi
echo ""

# ── Release notes ─────────────────────────────────────────────────────────────
# CHANGELOG.md is what the GitHub Release body and the public releases page are
# both built from, so a release with nothing written in it ships a version
# number and no explanation. Warn, don't block: a build-only release is a real
# thing and the entry can also be written after the fact.
CHANGELOG="CHANGELOG.md"
NOTES_STATE="missing"
if [[ -f "$CHANGELOG" ]]; then
    if grep -q "^## \[$NEW\]" "$CHANGELOG"; then
        NOTES_STATE="dated"
    elif awk '/^## \[[Uu]nreleased\]/{f=1;next} /^## /{f=0} f && /^- /{found=1} END{exit !found}' "$CHANGELOG"; then
        NOTES_STATE="unreleased"
    fi
fi

case "$NOTES_STATE" in
    dated)      echo "  Release notes: CHANGELOG.md already has a [$NEW] section" ;;
    unreleased) echo "  Release notes: [Unreleased] -> [$NEW] (dated $(date +%F))" ;;
    missing)    echo "  Release notes: NOTHING under [Unreleased] — $TAG will ship with no notes" ;;
esac
echo ""

$DRY_RUN && { echo "Dry run — nothing done."; exit 0; }

if [[ -t 0 ]]; then
    read -r -p "Proceed? [y/N] " CONFIRM
    [[ "$CONFIRM" =~ ^[Yy]$ ]] || { echo "Aborted."; exit 0; }
else
    echo "No TTY on stdin — proceeding without confirmation."
fi

# ── Bump + commit + tag ───────────────────────────────────────────────────────
# Match against CARGO_VERSION (what's literally in the file), not CURRENT
# (which may come from a tag and differ from this branch's Cargo.toml).
if [[ "$OSTYPE" == "darwin"* ]]; then
    sed -i '' "s/^version = \"$CARGO_VERSION\"/version = \"$NEW\"/" "$CARGO"
else
    sed -i    "s/^version = \"$CARGO_VERSION\"/version = \"$NEW\"/" "$CARGO"
fi

if ! grep -q "^version = \"$NEW\"" "$CARGO"; then
    echo "Failed to bump Cargo.toml version ($CARGO_VERSION -> $NEW) — aborting before commit/tag."
    exit 1
fi

# Refresh Cargo.lock so its recorded version matches the bump, and commit it
# with the manifest — otherwise CI's `cargo test --locked` fails on master
# for every release.
cargo metadata --format-version 1 --quiet >/dev/null

# Stamp the notes with the version they are shipping in. CI can do this for
# itself when generating the feed, but only the file in the repository is what
# the next release reads, so the heading is settled here once.
if [[ "$NOTES_STATE" == "unreleased" ]]; then
    if [[ "$OSTYPE" == "darwin"* ]]; then
        sed -i '' "s/^## \[[Uu]nreleased\].*$/## [$NEW] - $(date +%F)/" "$CHANGELOG"
    else
        sed -i    "s/^## \[[Uu]nreleased\].*$/## [$NEW] - $(date +%F)/" "$CHANGELOG"
    fi
    git add "$CHANGELOG"
fi

git add "$CARGO" Cargo.lock
git commit -m "chore: release $TAG"
git tag "$TAG"

git push origin HEAD
git push origin "$TAG"

echo ""
echo "  $TAG pushed — CI is running."
echo "  https://github.com/bennekrouf/ais-analytics/actions"
echo "  Releases page  -> https://mayorana.ch/en/apps/ais-analytics/releases"
