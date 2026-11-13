#!/bin/bash
# App Store / TestFlight build of the iPhone app (universal purchase with the Mac App Store app). docs/IOS_RELEASE.md.
# Fixed ids: app ai.mirastacklabs.miramail, widget <app>.widgets, App Group group.<app>. No BUNDLE_ID override.
#   BUILD=<n> app/scripts/ios-release.sh                   archive + export a signed .ipa (SIGN=1, default)
#   SIGN=0 BUILD=<n> app/scripts/ios-release.sh            archive only, CODE_SIGNING_ALLOWED=NO, no Apple account contact
#   ALLOW_PROVISIONING=1 ...                               lets Xcode create/update App IDs, the App Group and profiles
# Never uploads (the owner uploads the .ipa with Transporter) and never installs on a device.
# Data dir: the Rust staticlib is built with TAURI_CONFIG={"identifier":"ai.mirastacklabs.miramail"} (merged after
# tauri.ios.conf.json, which keeps the .spike id for dev builds and is never edited): the app's data dir on the phone is
# Application Support/ai.mirastacklabs.miramail. Do NOT run `tauri ios init/dev/build` (they sign with the shipping id).
set -euo pipefail
APP_ID="ai.mirastacklabs.miramail"
WIDGET_ID="$APP_ID.widgets"
GROUP_ID="group.$APP_ID"
SIGN="${SIGN:-1}"
[ "$SIGN" = "0" ] || [ "$SIGN" = "1" ] || { echo "SIGN must be 0 or 1"; exit 1; }
BUILD="${BUILD:-}"
[[ "$BUILD" =~ ^[1-9][0-9]*$ ]] || { echo "set BUILD to a positive integer (CFBundleVersion of app and widget; must grow with every upload)"; exit 1; }
cd "$(dirname "$0")/../.."          # repo root
export PATH="$PWD/target/podshim:/opt/homebrew/opt/rustup/bin:$PATH" RUSTUP_TOOLCHAIN=1.99.1
VERSION="$(python3 -c 'import json; print(json.load(open("app/src-tauri/tauri.conf.json"))["version"])')"
[[ "$VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || { echo "ABORT: tauri.conf.json version '$VERSION' is not x.y.z"; exit 1; }
# Team id: env DEVELOPMENT_TEAM, else the gitignored .env.local. Never hardcoded in tracked files.
TEAM="${DEVELOPMENT_TEAM:-$(sed -n 's/^DEVELOPMENT_TEAM=//p' .env.local 2>/dev/null | tail -1 | tr -d "\"' ")}"
if [ "$SIGN" = "1" ] && [ -z "$TEAM" ]; then echo "set DEVELOPMENT_TEAM (env or .env.local at the repo root)"; exit 1; fi

OUT=target/ios-release
APPLE=app/src-tauri/gen/apple
GEN=target/ios-release-gen/app/src-tauri
GEN_APPLE=$GEN/gen/apple
ARCHIVE=$OUT/MiraMail.xcarchive
EXPORT=$OUT/export

PROV_FLAGS=""
if [ "$SIGN" = "1" ]; then
  if [ "${ALLOW_PROVISIONING:-0}" = "1" ]; then
    PROV_FLAGS="-allowProvisioningUpdates"
  else
    echo "ALLOW_PROVISIONING is not 1: building WITHOUT -allowProvisioningUpdates (Xcode will not touch the developer account)."
    echo "  Signing needs these to exist already in the Apple Developer portal / App Store Connect:"
    echo "    App IDs $APP_ID and $WIDGET_ID, both with the App Groups capability, the App Group $GROUP_ID,"
    echo "    and App Store distribution profiles for both (Xcode managed profiles are created by ALLOW_PROVISIONING=1)."
  fi
fi

# 0. no-op `pod` (the generated project uses no pods)
mkdir -p target/podshim; [ -x target/podshim/pod ] || { printf '#!/bin/sh\nexit 0\n' > target/podshim/pod; chmod +x target/podshim/pod; }
rm -rf "$OUT" target/ios-release-gen
mkdir -p "$OUT"
# 1. frontend (embedded into the binary at compile time)
(cd app && npm run build >/dev/null)
# 2. Rust staticlib with the shipping identifier. TAURI_CONFIG is merged AFTER tauri.ios.conf.json by tauri-build (lib.rs
# `read_from` then `json_patch::merge`), tauri-codegen and tauri-utils, so tracked config stays untouched.
TAURI_CONFIG="{\"identifier\":\"$APP_ID\"}" CARGO_TARGET_DIR=$OUT/cargo \
  cargo build --release -p miramail --lib --features tauri/custom-protocol --target aarch64-apple-ios
LIB=$OUT/cargo/aarch64-apple-ios/release/libmiramail_lib.a
# proof: the shipping id is embedded and the dev id is not (counts, not `grep -q` in a pipe: SIGPIPE + pipefail)
LIB_STR="$(strings -a "$LIB")"
N_SPIKE="$(printf '%s\n' "$LIB_STR" | grep -cF "$APP_ID.spike" || true)"
N_ID="$(printf '%s\n' "$LIB_STR" | grep -cF "$APP_ID" || true)"
[ "$N_SPIKE" = "0" ] || { echo "ABORT: staticlib still embeds the .spike identifier (TAURI_CONFIG override not applied)"; exit 1; }
[ "$N_ID" -gt 0 ] || { echo "ABORT: staticlib does not embed the identifier $APP_ID"; exit 1; }
echo "OK identifier: staticlib embeds $APP_ID ($N_ID string lines), $N_SPIKE .spike strings"
# 3. Xcode project from a throwaway copy (same depth, so ../../src and ../../plugins resolve)
mkdir -p $GEN
ln -s "$PWD/app/src-tauri/src" $GEN/src
ln -s "$PWD/app/src-tauri/plugins" $GEN/plugins
mkdir -p $GEN_APPLE
rsync -a --exclude '*.xcodeproj' --exclude Externals --exclude 'project.*.yml' $APPLE/ $GEN_APPLE/
mkdir -p $GEN_APPLE/Externals/arm64/release
cp "$LIB" $GEN_APPLE/Externals/arm64/release/libapp.a
APP_ID="$APP_ID" VERSION="$VERSION" BUILD="$BUILD" GEN_APPLE="$GEN_APPLE" python3 - <<'PY'
import os
e = os.environ
d = e["GEN_APPLE"]
s = open(os.path.join(d, "project.yml")).read()
s = s[: s.index("    preBuildScripts:")].rstrip() + "\n"
s = s.replace("ai.mirastacklabs.miramail.spike", e["APP_ID"])
for old, new in (('CFBundleShortVersionString: 1.0.0', 'CFBundleShortVersionString: ' + e["VERSION"]),
                 ('CFBundleVersion: "1.0.0"', 'CFBundleVersion: "%s"' % e["BUILD"])):
    if s.count(old) != 2:
        raise SystemExit("ABORT: expected '%s' on 2 targets, found %d" % (old, s.count(old)))
    s = s.replace(old, new)
open(os.path.join(d, "project.release.yml"), "w").write(s)
PY
(cd $GEN_APPLE && xcodegen -q -s project.release.yml -p .)
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
# guards on the generated project
PBX=$GEN_APPLE/miramail.xcodeproj/project.pbxproj
IDS="$(sed -n 's/^[[:space:]]*PRODUCT_BUNDLE_IDENTIFIER = \(.*\);$/\1/p' $PBX | tr -d '"' | sort -u | tr '\n' ' ')"
[ "$IDS" = "$APP_ID $WIDGET_ID " ] || { echo "ABORT: generated project has bundle ids '$IDS', expected '$APP_ID $WIDGET_ID'"; exit 1; }
echo "OK pbxproj bundle ids: $IDS"
groups_of() { python3 -c 'import plistlib,sys; print("\n".join(plistlib.loads(sys.stdin.buffer.read()).get("com.apple.security.application-groups", [])))'; }
for e in $GEN_APPLE/miramail_iOS/miramail_iOS.entitlements $GEN_APPLE/MiraMailWidgets/MiraMailWidgets.entitlements; do
  G="$(groups_of < "$e")"
  [ "$G" = "$GROUP_ID" ] || { echo "ABORT: $e application-groups is '$G', expected exactly $GROUP_ID"; exit 1; }
  grep -q "aps-environment" "$e" && { echo "ABORT: $e has aps-environment (no push)"; exit 1; }
done
echo "OK entitlements source: application-groups == [$GROUP_ID], no aps-environment"
FAM="$(sed -n 's/^[[:space:]]*TARGETED_DEVICE_FAMILY = \(.*\);$/\1/p' $PBX | tr -d '"' | sort -u | tr '\n' ' ')"
[ "$FAM" = "1 " ] || { echo "ABORT: TARGETED_DEVICE_FAMILY values '$FAM', expected only 1"; exit 1; }
echo "OK TARGETED_DEVICE_FAMILY: $FAM"

# 4. archive
if [ "$SIGN" = "0" ]; then
  SIGN_ARGS=(CODE_SIGNING_ALLOWED=NO CODE_SIGNING_REQUIRED=NO CODE_SIGN_IDENTITY=)
else
  SIGN_ARGS=(DEVELOPMENT_TEAM="$TEAM" CODE_SIGN_STYLE=Automatic)
fi
xcodebuild -project $GEN_APPLE/miramail.xcodeproj -scheme miramail_iOS -configuration release \
  -sdk iphoneos -destination 'generic/platform=iOS' -derivedDataPath $OUT/dd $PROV_FLAGS \
  -archivePath "$ARCHIVE" "${SIGN_ARGS[@]}" archive 2>&1 | tail -40
ARCH_APP="$ARCHIVE/Products/Applications/MiraMail.app"
[ -d "$ARCH_APP" ] || { echo "ABORT: $ARCH_APP missing, archive failed"; exit 1; }
[ -d "$ARCH_APP/PlugIns/MiraMailWidgets.appex" ] || { echo "ABORT: widget extension not embedded"; exit 1; }
APP_STR="$(strings -a "$ARCH_APP/MiraMail")"
[ "$(printf '%s\n' "$APP_STR" | grep -cF "$APP_ID.spike" || true)" = "0" ] || { echo "ABORT: app binary embeds the .spike identifier"; exit 1; }
echo "OK app binary has no .spike string"

# 5. bundle checks (no signature needed). $1 = .app directory, $2 = "signed" to also check signatures.
check_bundle() {
  local app="$1" signed="$2" appex="$1/PlugIns/MiraMailWidgets.appex"
  APP="$app" APPEX="$appex" APP_ID="$APP_ID" WIDGET_ID="$WIDGET_ID" BUILD="$BUILD" VERSION="$VERSION" python3 - <<'PY'
import os, plistlib, sys
e = os.environ
def load(p):
    with open(p, "rb") as f: return plistlib.load(f)
def fail(m): print("ABORT: " + m); sys.exit(1)
app, appex = e["APP"], e["APPEX"]
a, w = load(app + "/Info.plist"), load(appex + "/Info.plist")
if a["CFBundleIdentifier"] != e["APP_ID"]: fail("app CFBundleIdentifier %r" % a["CFBundleIdentifier"])
if w["CFBundleIdentifier"] != e["WIDGET_ID"]: fail("widget CFBundleIdentifier %r" % w["CFBundleIdentifier"])
if a["CFBundleVersion"] != e["BUILD"] or w["CFBundleVersion"] != e["BUILD"]:
    fail("CFBundleVersion app=%r widget=%r expected %s" % (a["CFBundleVersion"], w["CFBundleVersion"], e["BUILD"]))
if a["CFBundleShortVersionString"] != e["VERSION"] or w["CFBundleShortVersionString"] != e["VERSION"]:
    fail("CFBundleShortVersionString app=%r widget=%r expected %s" % (a["CFBundleShortVersionString"], w["CFBundleShortVersionString"], e["VERSION"]))
if a.get("ITSAppUsesNonExemptEncryption") is not False: fail("ITSAppUsesNonExemptEncryption is %r, expected false" % a.get("ITSAppUsesNonExemptEncryption"))
if a.get("UIDeviceFamily") != [1]: fail("app UIDeviceFamily %r, expected [1]" % a.get("UIDeviceFamily"))
if w.get("UIDeviceFamily") != [1]: fail("widget UIDeviceFamily %r, expected [1]" % w.get("UIDeviceFamily"))
if not os.path.isfile(app + "/PrivacyInfo.xcprivacy"): fail("PrivacyInfo.xcprivacy missing in the app")
p = load(app + "/PrivacyInfo.xcprivacy")
if p.get("NSPrivacyTracking") is not False or p.get("NSPrivacyTrackingDomains") != [] or p.get("NSPrivacyCollectedDataTypes") != []:
    fail("PrivacyInfo.xcprivacy must declare no tracking, no domains, no collected data")
types = {t["NSPrivacyAccessedAPIType"] for t in p.get("NSPrivacyAccessedAPITypes", [])}
if not {"NSPrivacyAccessedAPICategoryFileTimestamp", "NSPrivacyAccessedAPICategoryDiskSpace"} <= types: fail("PrivacyInfo.xcprivacy lacks the FileTimestamp / DiskSpace declarations (nm -u list below)")
print("OK plists: ids, CFBundleVersion=%s (app == widget), CFBundleShortVersionString=%s, ITSAppUsesNonExemptEncryption=false, UIDeviceFamily=[1], PrivacyInfo.xcprivacy in the app (the widget binary imports no required-reason API symbol)" % (e["BUILD"], e["VERSION"]))
PY
  if [ "$signed" = "signed" ]; then
    check_signed "$app" "$APP_ID"
    check_signed "$appex" "$WIDGET_ID"
  fi
}
# signature, team, Apple Distribution authority, entitlements (groups exact, no get-task-allow, no aps-environment)
check_signed() {   # $1 = bundle, $2 = expected Identifier
  local info ident team auth ents
  info="$(codesign -dvv "$1" 2>&1)"
  ident="$(printf '%s\n' "$info" | sed -n 's/^Identifier=//p' | head -1)"
  team="$(printf '%s\n' "$info" | sed -n 's/^TeamIdentifier=//p' | head -1)"
  auth="$(printf '%s\n' "$info" | sed -n 's/^Authority=//p' | head -1)"
  [ "$ident" = "$2" ] || { echo "ABORT: signed Identifier '$ident' != '$2'"; exit 1; }
  [ "$team" = "$TEAM" ] || { echo "ABORT: signed TeamIdentifier '$team' != the configured team for $1"; exit 1; }
  case "$auth" in "Apple Distribution"*) ;; *) echo "ABORT: $1 first Authority is '$auth', expected Apple Distribution"; exit 1;; esac
  ents="$(codesign -d --entitlements :- "$1" 2>/dev/null)"
  printf '%s' "$ents" | GROUP_ID="$GROUP_ID" WHAT="$1" python3 -c '
import os, plistlib, sys
d = plistlib.loads(sys.stdin.buffer.read())
w = os.environ["WHAT"]
if d.get("com.apple.security.application-groups") != [os.environ["GROUP_ID"]]: sys.exit("ABORT: %s application-groups %r" % (w, d.get("com.apple.security.application-groups")))
if d.get("get-task-allow"): sys.exit("ABORT: %s has get-task-allow true" % w)
if "aps-environment" in d: sys.exit("ABORT: %s has aps-environment (no push)" % w)
print("OK signed:", w, "Identifier/Team/Apple Distribution/groups exact/no get-task-allow/no aps-environment")
'
}
check_bundle "$ARCH_APP" unsigned

# 6. required-reason API evidence (docs/IOS_RELEASE.md, PrivacyInfo.xcprivacy): undefined symbols of both binaries
echo "--- required-reason API symbols (nm -u) ---"
for b in "$ARCH_APP/MiraMail" "$ARCH_APP/PlugIns/MiraMailWidgets.appex/MiraMailWidgets"; do
  echo "# $b"
  nm -u "$b" 2>/dev/null | grep -E '_(stat|fstat|lstat|fstatat|getattrlist|fgetattrlist|getattrlistbat|statfs|fstatfs|statvfs|fstatvfs|mach_absolute_time|clock_gettime_nsec_np|systemUptime)$|NSFileCreationDate|NSFileModificationDate|NSURLContentModificationDateKey|NSURLCreationDateKey|NSFileSystemFreeSize|NSFileSystemSize|volumeAvailableCapacity|NSUserDefaults|activeInputModes' | sort -u || true
done

if [ "$SIGN" = "0" ]; then
  echo "SIGN=0: archive at $ARCHIVE (unsigned, nothing exported). OK"; exit 0
fi

# 7. export the App Store .ipa (generated options; the team id is written at runtime into target/, never tracked)
OPTS=$OUT/ExportOptions.plist
TEAM="$TEAM" OPTS="$OPTS" python3 - <<'PY'
import os, plistlib
plistlib.dump({"method": "app-store-connect", "destination": "export", "signingStyle": "automatic",
               "teamID": os.environ["TEAM"], "uploadSymbols": True, "manageAppVersionAndBuildNumber": False},
              open(os.environ["OPTS"], "wb"))
PY
xcodebuild -exportArchive -archivePath "$ARCHIVE" -exportPath "$EXPORT" -exportOptionsPlist "$OPTS" $PROV_FLAGS 2>&1 | tail -30
IPA="$(ls "$EXPORT"/*.ipa 2>/dev/null | head -1)"
[ -n "$IPA" ] || { echo "ABORT: no .ipa in $EXPORT"; exit 1; }
CHK="$OUT/ipa-check"; rm -rf "$CHK"; mkdir -p "$CHK"
unzip -q "$IPA" -d "$CHK"
check_bundle "$CHK/Payload/MiraMail.app" signed
rm -rf "$CHK"
echo "OK $IPA ($VERSION build $BUILD). Upload with Transporter; this script never uploads."
