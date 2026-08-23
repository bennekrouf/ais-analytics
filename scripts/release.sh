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

CURRENT=$(grep '^version' "$CARGO" | head -1 | sed 's/version = "\(.*\)"/\1/')
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
echo "  Cargo.toml : $CURRENT -> $NEW"
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

$DRY_RUN && { echo "Dry run — nothing done."; exit 0; }

read -r -p "Proceed? [y/N] " CONFIRM
[[ "$CONFIRM" =~ ^[Yy]$ ]] || { echo "Aborted."; exit 0; }

# ── Bump + commit + tag ───────────────────────────────────────────────────────
if [[ "$OSTYPE" == "darwin"* ]]; then
    sed -i '' "s/^version = \"$CURRENT\"/version = \"$NEW\"/" "$CARGO"
else
    sed -i    "s/^version = \"$CURRENT\"/version = \"$NEW\"/" "$CARGO"
fi

git add "$CARGO"
git commit -m "chore: release $TAG"
git tag "$TAG"

git push origin HEAD
git push origin "$TAG"

echo ""
echo "  $TAG pushed — CI is running."
echo "  https://github.com/bennekrouf/ais-analytics/actions"
