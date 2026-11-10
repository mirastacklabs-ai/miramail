#!/usr/bin/env bash
# MiraMail release build: build -> sign (Developer ID, hardened runtime) -> notarise -> staple -> dmg.
# UNTESTED until a "Developer ID Application" certificate and a notarytool profile exist on this Mac.
# Configuration (env vars or a gitignored .env.local at the repo root; never commit secrets):
#   SIGN_IDENTITY   e.g. "Developer ID Application: Your Name (TEAMID)"   (required)
#   NOTARY_PROFILE  name given to `xcrun notarytool store-credentials`      (default: miramail-notary)
#   SKIP_NOTARIZE=1 sign only (for a dry run)
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"
[ -f .env.local ] && set -a && . ./.env.local && set +a
: "${SIGN_IDENTITY:?Set SIGN_IDENTITY to your 'Developer ID Application: ...' identity}"
NOTARY_PROFILE="${NOTARY_PROFILE:-miramail-notary}"
APP="target/release/bundle/macos/MiraMail.app"
VERSION="$(python3 -I -c 'import json;print(json.load(open("app/src-tauri/tauri.conf.json"))["version"])')"
OUT="target/release/bundle/MiraMail-$VERSION.dmg"
STAGE="target/release/bundle/dmg-stage"

security find-identity -v -p codesigning | grep -F "$SIGN_IDENTITY" >/dev/null \
  || { echo "Signing identity not found in keychain: $SIGN_IDENTITY" >&2; exit 1; }

(cd app && npm run tauri -- build --bundles app)

codesign --force --deep --options runtime --timestamp \
  --entitlements scripts/entitlements.plist -s "$SIGN_IDENTITY" "$APP"
codesign --verify --deep --strict --verbose=2 "$APP"

# Package.
rm -rf "$OUT" "$STAGE"   # our own build output inside the repo
mkdir -p "$STAGE" && ditto "$APP" "$STAGE/MiraMail.app" && ln -s /Applications "$STAGE/Applications"
hdiutil create -volname MiraMail -srcfolder "$STAGE" -ov -format UDZO "$OUT"
rm -rf "$STAGE"
codesign --force --timestamp -s "$SIGN_IDENTITY" "$OUT"

if [ "${SKIP_NOTARIZE:-0}" != "1" ]; then
  xcrun notarytool submit "$OUT" --keychain-profile "$NOTARY_PROFILE" --wait
  xcrun stapler staple "$OUT"
  xcrun stapler validate "$OUT"
  spctl --assess --type open --context context:primary-signature -v "$OUT"
fi
echo "Done: $OUT"
