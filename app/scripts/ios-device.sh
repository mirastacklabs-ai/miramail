#!/bin/bash
# Build the REAL MiraMail app (app/src + app/src-tauri) for a physical iPhone and install it (phase 1, docs/IOS_SPIKE.md).
# Why not `tauri ios build`: it signs with the identifier from tauri.conf.json (ai.mirastacklabs.miramail, the shipping Mac App
# Store App ID) and Xcode's automatic provisioning would then touch that App ID. This script builds the same artefacts by hand
# and always signs with a separate development id. Same manual path as app-ios/scripts/device-build.sh.
# DO NOT run `tauri ios init/dev/build`: it signs with the shipping id. The app data dir derives from the config identifier
# (ai.mirastacklabs.miramail.spike on iOS via src-tauri/tauri.ios.conf.json); never change it, or the app starts with an empty database.
#   UDID=<iPhone UDID> app/scripts/ios-device.sh        (INSTALL=0 to only build)
# Also builds the home-screen widget extension (<BUNDLE_ID>.widgets) and shares App Group group.<BUNDLE_ID> with it (docs/IOS_SPIKE.md).
set -euo pipefail
BUNDLE_ID="${BUNDLE_ID:-ai.mirastacklabs.miramail.spike}"
BUNDLE_ID="$(printf '%s' "$BUNDLE_ID" | tr '[:upper:]' '[:lower:]' | tr -d '[:space:]')"
# allowlist: only the development ids .spike or .dev; the shipping App ID itself never matches
BUNDLE_RE='^ai\.mirastacklabs\.miramail\.(spike|dev)$'
if ! [[ "$BUNDLE_ID" =~ $BUNDLE_RE ]]; then
  echo "refusing BUNDLE_ID '$BUNDLE_ID': must match $BUNDLE_RE (never the shipping App ID ai.mirastacklabs.miramail)"; exit 1
fi
# The widget extension (MiraMailWidgets, home-screen widgets) is signed with the same team and automatic provisioning. Its id is
# <BUNDLE_ID>.widgets and the App Group both share is group.<BUNDLE_ID>: Xcode creates these two (an extra App ID and an App
# Group) in the developer account on the first build. Neither is ever the shipping id.
WIDGET_ID="$BUNDLE_ID.widgets"
GROUP_ID="group.$BUNDLE_ID"
WIDGET_RE='^ai\.mirastacklabs\.miramail\.(spike|dev)\.widgets$'
if ! [[ "$WIDGET_ID" =~ $WIDGET_RE ]]; then
  echo "refusing widget id '$WIDGET_ID': must match $WIDGET_RE"; exit 1
fi
cd "$(dirname "$0")/../.."          # repo root
export PATH="$PWD/target/podshim:/opt/homebrew/opt/rustup/bin:$PATH" RUSTUP_TOOLCHAIN=1.99.1
UDID="${UDID:?set UDID to the iPhone UDID (xcrun devicectl list devices)}"
# Team id: env DEVELOPMENT_TEAM, else the gitignored .env.local (DEVELOPMENT_TEAM=...). Never hardcoded in tracked files.
TEAM="${DEVELOPMENT_TEAM:-$(sed -n 's/^DEVELOPMENT_TEAM=//p' .env.local 2>/dev/null | tail -1 | tr -d "\"' ")}"
[ -n "$TEAM" ] || { echo "set DEVELOPMENT_TEAM (env or .env.local at the repo root)"; exit 1; }
OUT=target/ios-phase1
APPLE=app/src-tauri/gen/apple
# xcodegen output goes to a throwaway copy under target/: the tracked files in gen/apple (xcodeproj, Info.plists,
# entitlements) are never rewritten by a build. The copy sits at the same depth so ../../src and ../../plugins resolve.
GEN=target/ios-gen/app/src-tauri
GEN_APPLE=$GEN/gen/apple

# Apple Developer account changes (new App IDs, the App Group, profiles) happen only with an explicit opt-in.
PROV_FLAGS=""
if [ "${ALLOW_PROVISIONING:-0}" = "1" ]; then
  PROV_FLAGS="-allowProvisioningUpdates -allowProvisioningDeviceRegistration"
else
  echo "ALLOW_PROVISIONING is not 1: building WITHOUT -allowProvisioningUpdates (Xcode will not touch the developer account)."
  echo "  Signing needs these to exist already; ALLOW_PROVISIONING=1 lets Xcode create them:"
  echo "    App IDs $BUNDLE_ID and $WIDGET_ID, both with the App Groups capability, and the App Group $GROUP_ID,"
  echo "    plus development profiles for both and the device registration."
fi

# 0. no-op `pod` (the generated project uses no pods; `tauri ios init` insists on CocoaPods)
mkdir -p target/podshim; [ -x target/podshim/pod ] || { printf '#!/bin/sh\nexit 0\n' > target/podshim/pod; chmod +x target/podshim/pod; }
# 1. frontend (embedded into the binary at compile time)
(cd app && npm run build >/dev/null)
# 2. Rust staticlib; custom-protocol = serve the embedded frontend instead of the dev URL
CARGO_TARGET_DIR=$OUT/cargo cargo build --release -p miramail --lib --features tauri/custom-protocol --target aarch64-apple-ios
# 3. Xcode project without the "Build Rust Code" phase (it would call `tauri ios xcode-script`, which needs the signing identity path above)
rm -rf target/ios-gen
mkdir -p $GEN
ln -s "$PWD/app/src-tauri/src" $GEN/src
ln -s "$PWD/app/src-tauri/plugins" $GEN/plugins
mkdir -p $GEN_APPLE
rsync -a --exclude '*.xcodeproj' --exclude Externals --exclude project.device.yml $APPLE/ $GEN_APPLE/
mkdir -p $GEN_APPLE/Externals/arm64/release
cp $OUT/cargo/aarch64-apple-ios/release/libmiramail_lib.a $GEN_APPLE/Externals/arm64/release/libapp.a
# The spike ids in project.yml (app, <app>.widgets, group.<app>) become BUNDLE_ID's: each target carries its own id, so no
# PRODUCT_BUNDLE_IDENTIFIER override is passed to xcodebuild (it would hit the extension too).
BUNDLE_ID="$BUNDLE_ID" GEN_APPLE="$GEN_APPLE" python3 - <<'PY'
import os
d = os.environ["GEN_APPLE"]
s = open(os.path.join(d, "project.yml")).read()
s = s[: s.index("    preBuildScripts:")].rstrip() + "\n"
s = s.replace("ai.mirastacklabs.miramail.spike", os.environ["BUNDLE_ID"])
open(os.path.join(d, "project.device.yml"), "w").write(s)
PY
(cd $GEN_APPLE && xcodegen -q -s project.device.yml -p .)
# xcodegen writes the SystemCapabilities target attribute as a quoted Swift-style string, which Xcode ignores. Rewrite it as the
# real property-list dictionary Xcode's Signing & Capabilities tab writes, so automatic provisioning enables App Groups.
GEN_APPLE="$GEN_APPLE" python3 - <<'PY'
import os
p = os.path.join(os.environ["GEN_APPLE"], "miramail.xcodeproj", "project.pbxproj")
s = open(p).read()
bad = 'SystemCapabilities = "[\\"com.apple.ApplicationGroups.iOS\\": [\\"enabled\\": 1]]";'
good = 'SystemCapabilities = {\n\t\t\t\t\t\tcom.apple.ApplicationGroups.iOS = {\n\t\t\t\t\t\t\tenabled = 1;\n\t\t\t\t\t\t};\n\t\t\t\t\t};'
n = s.count(bad)
if n != 2:
    raise SystemExit("ABORT: expected the SystemCapabilities attribute on 2 targets, found %d" % n)
open(p, "w").write(s.replace(bad, good))
PY
# the generated project must carry exactly the two ids (never the shipping App ID) and the matching App Group
IDS="$(sed -n 's/^[[:space:]]*PRODUCT_BUNDLE_IDENTIFIER = \(.*\);$/\1/p' $GEN_APPLE/miramail.xcodeproj/project.pbxproj | tr -d '"' | sort -u | tr '\n' ' ')"
[ "$IDS" = "$BUNDLE_ID $WIDGET_ID " ] || { echo "ABORT: generated project has bundle ids '$IDS', expected '$BUNDLE_ID $WIDGET_ID'"; exit 1; }
# application-groups must be exactly [GROUP_ID] (read as a property list from stdin)
groups_of() { python3 -c 'import plistlib,sys; print("\n".join(plistlib.loads(sys.stdin.buffer.read()).get("com.apple.security.application-groups", [])))'; }
for e in $GEN_APPLE/miramail_iOS/miramail_iOS.entitlements $GEN_APPLE/MiraMailWidgets/MiraMailWidgets.entitlements; do
  G="$(groups_of < "$e")"
  [ "$G" = "$GROUP_ID" ] || { echo "ABORT: $e application-groups is '$G', expected exactly $GROUP_ID"; exit 1; }
done
# 4. build + development-sign
xcodebuild -project $GEN_APPLE/miramail.xcodeproj -scheme miramail_iOS -configuration release \
  -sdk iphoneos -derivedDataPath $OUT/dd-device $PROV_FLAGS \
  -destination "id=$UDID" \
  DEVELOPMENT_TEAM="$TEAM" CODE_SIGN_STYLE=Automatic build 2>&1 | tail -30
APP=$(ls -d $OUT/dd-device/Build/Products/release-iphoneos/*.app | head -1)
echo "APP=$APP"
codesign -dvv "$APP" 2>&1 | grep -E 'Identifier=|Authority=Apple Development|TeamIdentifier' || true
# never install a build whose signed identifier is not the id we meant (e.g. the shipping App ID): app AND widget extension
APPEX="$APP/PlugIns/MiraMailWidgets.appex"
[ -d "$APPEX" ] || { echo "ABORT: $APPEX missing (widget extension not embedded); not installing"; exit 1; }
codesign -dvv "$APPEX" 2>&1 | grep -E 'Identifier=|TeamIdentifier' || true
check_signed() {   # $1 = bundle, $2 = expected Identifier; output is captured, never piped into grep -q (pipefail)
  local info ident team ents g
  info="$(codesign -dv "$1" 2>&1)"
  ident="$(printf '%s\n' "$info" | sed -n 's/^Identifier=//p' | head -1)"
  team="$(printf '%s\n' "$info" | sed -n 's/^TeamIdentifier=//p' | head -1)"
  [ "$ident" = "$2" ] || { echo "ABORT: signed Identifier '$ident' != '$2'; not installing"; exit 1; }
  [ "$team" = "$TEAM" ] || { echo "ABORT: signed TeamIdentifier '$team' != '$TEAM' for $1; not installing"; exit 1; }
  ents="$(codesign -d --entitlements :- "$1" 2>/dev/null)"
  g="$(printf '%s' "$ents" | groups_of)"
  [ "$g" = "$GROUP_ID" ] || { echo "ABORT: $1 application-groups is '$g', expected exactly $GROUP_ID; not installing"; exit 1; }
}
check_signed "$APP" "$BUNDLE_ID"
check_signed "$APPEX" "$WIDGET_ID"
if [ "${INSTALL:-1}" = "1" ]; then
  xcrun devicectl device install app --device "$UDID" "$APP"
fi
