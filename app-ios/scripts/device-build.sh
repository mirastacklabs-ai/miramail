#!/bin/bash
# Device build/install for the iOS spike (iPhone over USB).
# Same manual path as sim-build.sh (cargo staticlib -> Externals, xcodegen, xcodebuild), but for iphoneos
# with automatic development signing (team from DEVELOPMENT_TEAM). Uses a separate spike bundle id so the real
# MiraMail App ID (ai.mirastacklabs.miramail) is never modified by Xcode's automatic provisioning.
# -allowProvisioningUpdates lets Xcode register the device and create a development profile for the spike id.
set -euo pipefail
BUNDLE_ID="${BUNDLE_ID:-ai.mirastacklabs.miramail.spike}"
BUNDLE_ID="$(printf '%s' "$BUNDLE_ID" | tr '[:upper:]' '[:lower:]' | tr -d '[:space:]')"
# allowlist: only the development ids .spike or .dev; the shipping App ID itself never matches
BUNDLE_RE='^ai\.mirastacklabs\.miramail\.(spike|dev)$'
if ! [[ "$BUNDLE_ID" =~ $BUNDLE_RE ]]; then
  echo "refusing BUNDLE_ID '$BUNDLE_ID': must match $BUNDLE_RE (never the shipping App ID ai.mirastacklabs.miramail)"; exit 1
fi
cd "$(dirname "$0")/../.."          # repo root
export PATH="/opt/homebrew/opt/rustup/bin:$PATH" RUSTUP_TOOLCHAIN=1.99.1
UDID="${UDID:?set UDID to the iPhone UDID (xcrun devicectl list devices)}"
# Team id: env DEVELOPMENT_TEAM, else the gitignored .env.local (DEVELOPMENT_TEAM=...). Never hardcoded in tracked files.
TEAM="${DEVELOPMENT_TEAM:-$(sed -n 's/^DEVELOPMENT_TEAM=//p' .env.local 2>/dev/null | tail -1 | tr -d "\"' ")}"
[ -n "$TEAM" ] || { echo "set DEVELOPMENT_TEAM (env or .env.local at the repo root)"; exit 1; }
OUT=target/ios-spike
APPLE=app-ios/src-tauri/gen/apple

CARGO_TARGET_DIR=$OUT/cargo cargo build --release -p miramail-ios --lib --features tauri/custom-protocol --target aarch64-apple-ios
mkdir -p $APPLE/Externals/arm64/release
cp $OUT/cargo/aarch64-apple-ios/release/libmiramail_ios_lib.a $APPLE/Externals/arm64/release/libapp.a

python3 - <<'PY'
s = open("app-ios/src-tauri/gen/apple/project.yml").read()
s = s[: s.index("    preBuildScripts:")].rstrip() + "\n"
s = s.replace("targetTemplates:", "packages:\n  tauri-plugin-bgrefresh:\n    path: ../../plugins/bgrefresh/ios\ntargetTemplates:", 1)
s = s.replace("      - sdk: CoreGraphics.framework", "      - package: tauri-plugin-bgrefresh\n        product: tauri-plugin-bgrefresh\n      - sdk: CoreGraphics.framework", 1)
s = s.replace("        LSRequiresIPhoneOS: true", "        LSRequiresIPhoneOS: true\n        UIBackgroundModes: [fetch]\n        BGTaskSchedulerPermittedIdentifiers: [ai.mirastacklabs.miramail.refresh]", 1)
open("app-ios/src-tauri/gen/apple/project.device.yml", "w").write(s)
PY
(cd $APPLE && xcodegen -q -s project.device.yml -p .)
xcodebuild -project $APPLE/miramail-ios.xcodeproj -scheme miramail-ios_iOS -configuration release \
  -sdk iphoneos -derivedDataPath $OUT/dd-device -allowProvisioningUpdates -allowProvisioningDeviceRegistration \
  -destination "id=$UDID" \
  DEVELOPMENT_TEAM="$TEAM" CODE_SIGN_STYLE=Automatic PRODUCT_BUNDLE_IDENTIFIER="$BUNDLE_ID" build 2>&1 | tail -30
APP=$(ls -d $OUT/dd-device/Build/Products/release-iphoneos/*.app | head -1)
echo "APP=$APP"
codesign -dvv "$APP" 2>&1 | grep -E 'Identifier=|Authority=Apple Development|TeamIdentifier'
# never install a build whose signed identifier is not the id we meant (e.g. the shipping App ID)
SIGNED_ID="$(codesign -dv "$APP" 2>&1 | sed -n 's/^Identifier=//p' | head -1)"
if [ "$SIGNED_ID" != "$BUNDLE_ID" ]; then
  echo "ABORT: signed Identifier '$SIGNED_ID' != BUNDLE_ID '$BUNDLE_ID'; not installing"; exit 1
fi
if [ "${INSTALL:-1}" = "1" ]; then
  xcrun devicectl device install app --device "$UDID" "$APP"
fi
