#!/usr/bin/env bash
# Regenerates THIRD_PARTY_LICENSES.txt and THIRD_PARTY_NOTICES.md. Offline and deterministic.
# Needs: node_modules installed (app/), ~/.cargo registry populated (cargo metadata --offline), python3.
# Does not run cargo builds. Rerun whenever Cargo.lock or app/package-lock.json change.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
TMP="$ROOT/scripts/.lic-maps"   # inside the repo, removed afterwards
rm -rf "$TMP"
(cd "$ROOT/app" && npx vite build --sourcemap --outDir "$TMP" --emptyOutDir >/dev/null)
python3 -I "$ROOT/scripts/licenses_collect.py" "$TMP" "$ROOT"
rm -rf "$TMP"
