#!/bin/bash
# Build the REAL MiraMail app (app/src + app/src-tauri) for a physical iPhone and install it (phase 1, docs/IOS_SPIKE.md).
# Why not `tauri ios build`: it signs with the identifier from tauri.conf.json (ai.mirastacklabs.miramail, the shipping Mac App
# Store App ID) and Xcode's automatic provisioning would then touch that App ID. This script builds the same artefacts by hand
# and always signs with a separate development id. Same manual path as app-ios/scripts/device-build.sh.
# DO NOT run `tauri ios init/dev/build`: it signs with the shipping id. The app data dir derives from the config identifier
# (ai.mirastacklabs.miramail.spike on iOS via src-tauri/tauri.ios.conf.json); never change it, or the app starts with an empty database.
#   UDID=<iPhone UDID> app/scripts/ios-device.sh        (INSTALL=0 to only build)
set -euo pipefail
BUNDLE_ID="${BUNDLE_ID:-ai.mirastacklabs.miramail.spike}"
BUNDLE_ID="$(printf '%s' "$BUNDLE_ID" | tr '[:upper:]' '[:lower:]' | tr -d '[:space:]')"
# allowlist: only the development ids .spike or .dev; the shipping App ID itself never matches
BUNDLE_RE='^ai\.mirastacklabs\.miramail\.(spike|dev)$'
if ! [[ "$BUNDLE_ID" =~ $BUNDLE_RE ]]; then
  echo "refusing BUNDLE_ID '$BUNDLE_ID': must match $BUNDLE_RE (never the shipping App ID ai.mirastacklabs.miramail)"; exit 1
fi
cd "$(dirname "$0")/../.."          # repo root
export PATH="$PWD/target/podshim:/opt/homebrew/opt/rustup/bin:$PATH" RUSTUP_TOOLCHAIN=1.99.1
UDID="${UDID:?set UDID to the iPhone UDID (xcrun devicectl list devices)}"
# Team id: env DEVELOPMENT_TEAM, else the gitignored .env.local (DEVELOPMENT_TEAM=...). Never hardcoded in tracked files.
TEAM="${DEVELOPMENT_TEAM:-$(sed -n 's/^DEVELOPMENT_TEAM=//p' .env.local 2>/dev/null | tail -1 | tr -d "\"' ")}"
[ -n "$TEAM" ] || { echo "set DEVELOPMENT_TEAM (env or .env.local at the repo root)"; exit 1; }
OUT=target/ios-phase1
APPLE=app/src-tauri/gen/apple

# 0. no-op `pod` (the generated project uses no pods; `tauri ios init` insists on CocoaPods)
mkdir -p target/podshim; [ -x target/podshim/pod ] || { printf '#!/bin/sh\nexit 0\n' > target/podshim/pod; chmod +x target/podshim/pod; }
# 1. frontend (embedded into the binary at compile time)
(cd app && npm run build >/dev/null)
# 2. Rust staticlib; custom-protocol = serve the embedded frontend instead of the dev URL
CARGO_TARGET_DIR=$OUT/cargo cargo build --release -p miramail --lib --features tauri/custom-protocol --target aarch64-apple-ios
mkdir -p $APPLE/Externals/arm64/release
cp $OUT/cargo/aarch64-apple-ios/release/libmiramail_lib.a $APPLE/Externals/arm64/release/libapp.a
# 3. Xcode project without the "Build Rust Code" phase (it would call `tauri ios xcode-script`, which needs the signing identity path above)
python3 - <<'PY'
p = "app/src-tauri/gen/apple/project.yml"
s = open(p).read()
s = s[: s.index("    preBuildScripts:")].rstrip() + "\n"
open("app/src-tauri/gen/apple/project.device.yml", "w").write(s)
PY
(cd $APPLE && xcodegen -q -s project.device.yml -p .)
# 4. build + development-sign
xcodebuild -project $APPLE/miramail.xcodeproj -scheme miramail_iOS -configuration release \
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
