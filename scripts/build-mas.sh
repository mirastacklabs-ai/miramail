#!/usr/bin/env bash
# MiraMail Mac App Store build: build .app -> embed provisioning profile -> sign inside-out (App Sandbox) -> productbuild .pkg -> validate.
# UNTESTED end to end until an "Apple Distribution" + "3rd Party Mac Developer Installer" certificate and a Mac App Store
# provisioning profile exist on this Mac. See docs/MAS_BUILD.md. The Developer ID path (scripts/release.sh) is untouched.
# Configuration (env vars or a gitignored .env.local at the repo root; never commit secrets):
#   MAS_APP_IDENTITY        "Apple Distribution: Your Org (TEAMID)"                      (required)
#   MAS_INSTALLER_IDENTITY  "3rd Party Mac Developer Installer: Your Org (TEAMID)"       (required)
#   MAS_TEAM_ID             10-character Apple team id                                    (required)
#   MAS_PROFILE             path to the .provisionprofile (Mac App Store)                 (default: ./MiraMail_MAS.provisionprofile)
#   MAS_UNIVERSAL=0         skip the universal build and use the host architecture only
#   MAS_VALIDATE=1          also run `xcrun altool --validate-app` (needs MAS_ALTOOL_USER + MAS_ALTOOL_PASSWORD, an app-specific password;
#                           otherwise upload the .pkg with the Transporter app, which validates on upload)
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"
[ -f .env.local ] && set -a && . ./.env.local && set +a
die() { echo "build-mas: $*" >&2; exit 1; }

: "${MAS_APP_IDENTITY:?Set MAS_APP_IDENTITY to your 'Apple Distribution: ...' identity}"
: "${MAS_INSTALLER_IDENTITY:?Set MAS_INSTALLER_IDENTITY to your '3rd Party Mac Developer Installer: ...' identity}"
: "${MAS_TEAM_ID:?Set MAS_TEAM_ID to your 10-character Apple team id}"
MAS_PROFILE="${MAS_PROFILE:-$ROOT/MiraMail_MAS.provisionprofile}"
[[ "$MAS_TEAM_ID" =~ ^[A-Z0-9]{10}$ ]] || die "MAS_TEAM_ID must be 10 upper-case letters/digits"
[ -f "$MAS_PROFILE" ] || die "provisioning profile not found: $MAS_PROFILE (download the Mac App Store profile for ai.mirastacklabs.miramail from developer.apple.com)"
security find-identity -v -p codesigning | grep -F "$MAS_APP_IDENTITY" >/dev/null \
  || die "signing identity not in keychain: $MAS_APP_IDENTITY"
security find-identity -v | grep -F "$MAS_INSTALLER_IDENTITY" >/dev/null \
  || die "installer identity not in keychain: $MAS_INSTALLER_IDENTITY (a 'Mac Installer Distribution' / '3rd Party Mac Developer Installer' certificate)"

# Build. Universal needs the x86_64 Rust target; we never install toolchains here, so fall back to the host arch.
BUILD_ARGS=(--bundles app)
APP_DIR="target/release/bundle/macos"
if [ "${MAS_UNIVERSAL:-1}" = "1" ]; then
  if [ -d "$(rustc --print target-libdir --target x86_64-apple-darwin 2>/dev/null)" ] \
     && [ -d "$(rustc --print target-libdir --target aarch64-apple-darwin 2>/dev/null)" ]; then
    BUILD_ARGS+=(--target universal-apple-darwin)
    APP_DIR="target/universal-apple-darwin/release/bundle/macos"
  else
    echo "build-mas: x86_64-apple-darwin and/or aarch64-apple-darwin Rust target missing; building host architecture only." >&2
    echo "build-mas: for a universal build run: rustup target add x86_64-apple-darwin aarch64-apple-darwin" >&2
  fi
fi
# Do not leak the build machine's home/repo paths (usernames) into the shipped binary's panic/debug strings.
export RUSTFLAGS="${RUSTFLAGS:-} --remap-path-prefix=$HOME=/build --remap-path-prefix=$ROOT=/src"
case "$HOME$ROOT" in *" "*) die "HOME or the repo path contains a space: RUSTFLAGS path remapping would break" ;; esac
# Run the build without any signing/notarisation secrets in its environment (Tauri would otherwise sign/notarise itself).
(cd app && env -u MAS_ALTOOL_PASSWORD -u APPLE_SIGNING_IDENTITY -u APPLE_ID -u APPLE_PASSWORD -u APPLE_API_KEY -u APPLE_API_ISSUER -u APPLE_API_KEY_PATH npm run tauri -- build "${BUILD_ARGS[@]}")

OUTDIR="target/mas"
APP="$OUTDIR/MiraMail.app"
mkdir -p "$OUTDIR"
rm -rf "$APP" "$OUTDIR/MiraMail.pkg"   # our own build output inside the repo
ditto --noextattr --noqtn "$APP_DIR/MiraMail.app" "$APP"

# Provisioning profile + entitlements with the team id filled in (copies in target/, never committed).
cp "$MAS_PROFILE" "$APP/Contents/embedded.provisionprofile"
# Files from ~/Downloads carry com.apple.quarantine, which App Store upload rejects (error 91109): clear all extended attributes in our copy.
xattr -cr "$APP"
sed "s/TEAMID/$MAS_TEAM_ID/g" app/src-tauri/entitlements.mas.plist > "$OUTDIR/entitlements.mas.plist"
cp app/src-tauri/entitlements.mas.inherit.plist "$OUTDIR/entitlements.mas.inherit.plist"
plutil -lint "$OUTDIR/entitlements.mas.plist" "$OUTDIR/entitlements.mas.inherit.plist" >/dev/null

# Sign inside-out: nested code first (child entitlements), then the main executable bundle (app entitlements). No --deep.
MAIN="$APP/Contents/MacOS/miramail"
[ -f "$MAIN" ] || MAIN="$(find "$APP/Contents/MacOS" -type f -perm -u+x | head -1)"
# MiraMail ships no nested code today (no sidecars/frameworks); this loop covers anything added later, deepest paths first.
while IFS= read -r f; do
  [ "$f" = "$MAIN" ] && continue
  codesign --force --timestamp -s "$MAS_APP_IDENTITY" --entitlements "$OUTDIR/entitlements.mas.inherit.plist" "$f"
done < <({ find "$APP/Contents/Frameworks" "$APP/Contents/Helpers" "$APP/Contents/XPCServices" "$APP/Contents/PlugIns" -mindepth 1 -maxdepth 3 \( -type f -perm -u+x -o -name "*.framework" -o -name "*.xpc" -o -name "*.app" \) 2>/dev/null
           find "$APP/Contents/MacOS" -type f -perm -u+x 2>/dev/null; } | sort -r)
codesign --force --timestamp -s "$MAS_APP_IDENTITY" --entitlements "$OUTDIR/entitlements.mas.plist" "$APP"
codesign --verify --strict --verbose=2 "$APP"
codesign -d --entitlements - "$APP" 2>/dev/null | grep -q "com.apple.security.app-sandbox" || die "signed app is missing the app-sandbox entitlement"

# Pre-flight: fail here, not at upload, on anything Apple is known to reject.
# NOTE for maintainers: under `set -euo pipefail` a command that exits non-zero on "no match" aborts the script silently,
# and `cmd | grep -q` can fail the pipeline via SIGPIPE when grep exits early. So every check below captures output into a
# file or variable first and tests it with a here-string/file, and every command substitution has an explicit fallback.
preflight() {
  local plist="$APP/Contents/Info.plist" bad=0 bin="$MAIN" out="" tmp=""
  fail() { echo "preflight FAIL: $*" >&2; bad=1; }
  [ -f "$bin" ] || { fail "main executable not found"; die "pre-flight failed, not building the .pkg"; }
  # extended attributes: quarantine is rejected by Apple (error 91109)
  out="$(xattr -lr "$APP" 2>/dev/null || true)"
  if grep -Fq "com.apple.quarantine" <<<"$out"; then fail "com.apple.quarantine found in the bundle"; fi
  # bundle id / version / build / category / encryption flag
  out="$(/usr/libexec/PlistBuddy -c 'Print :CFBundleIdentifier' "$plist" 2>/dev/null || true)"
  [ "$out" = "ai.mirastacklabs.miramail" ] || fail "bundle id is '$out', not ai.mirastacklabs.miramail"
  for k in CFBundleShortVersionString CFBundleVersion LSMinimumSystemVersion LSApplicationCategoryType NSHumanReadableCopyright; do
    /usr/libexec/PlistBuddy -c "Print :$k" "$plist" >/dev/null 2>&1 || fail "Info.plist is missing $k"
  done
  out="$(/usr/libexec/PlistBuddy -c 'Print :ITSAppUsesNonExemptEncryption' "$plist" 2>/dev/null || true)"
  [ "$out" = "false" ] || fail "ITSAppUsesNonExemptEncryption must be false"
  # build number must equal tauri.conf.json bundleVersion and be an integer (it must increase with every upload)
  local want have
  want="$(plutil -extract bundle.macOS.bundleVersion raw -o - app/src-tauri/tauri.conf.json 2>/dev/null || true)"
  have="$(/usr/libexec/PlistBuddy -c 'Print :CFBundleVersion' "$plist" 2>/dev/null || true)"
  [[ "$have" =~ ^[0-9]+$ ]] || fail "CFBundleVersion '$have' is not an integer"
  [ -n "$want" ] && [ "$want" = "$have" ] || fail "CFBundleVersion '$have' differs from tauri.conf.json bundleVersion '$want'"
  echo "preflight: version $(/usr/libexec/PlistBuddy -c 'Print :CFBundleShortVersionString' "$plist" 2>/dev/null || echo '?') build $have (must be higher than the last uploaded build)"
  # privacy manifest present
  [ -f "$APP/Contents/Resources/PrivacyInfo.xcprivacy" ] || fail "PrivacyInfo.xcprivacy missing from Contents/Resources"
  # architecture vs minimum macOS (Apple error 90869: arm64-only needs >= 12.0)
  local archs="" minos="" min=""
  archs="$(lipo -archs "$bin" 2>/dev/null || true)"; [ -n "$archs" ] || fail "cannot read the architectures of the binary"
  min="$(/usr/libexec/PlistBuddy -c 'Print :LSMinimumSystemVersion' "$plist" 2>/dev/null || true)"
  if [ "$archs" = "arm64" ] && [ "$(printf '%s\n12.0\n' "$min" | sort -V | head -1)" != "12.0" ]; then fail "arm64-only build needs LSMinimumSystemVersion >= 12.0 (is $min)"; fi
  minos="$(vtool -show-build "$bin" 2>/dev/null | awk '/minos/{print $2; exit}' || true)"
  if [ -n "$minos" ] && [ "$(printf '%s\n%s\n' "$minos" "$min" | sort -V | tail -1)" != "$min" ]; then fail "binary minos $minos is newer than LSMinimumSystemVersion $min"; fi
  # no build-machine username/path inside the binary (file, not a pipe: see the note above)
  mkdir -p "$OUTDIR"
  strings -a "$bin" > "$OUTDIR/strings.txt" 2>/dev/null || true
  [ -s "$OUTDIR/strings.txt" ] || fail "could not read strings from the binary"
  if grep -Fq "/Users/" "$OUTDIR/strings.txt"; then fail "binary still contains /Users/ build paths"; fi
  rm -f "$OUTDIR/strings.txt"
  # provisioning profile: decode once, then check team, app id, expiry and that it is not a development profile
  local pp="$OUTDIR/profile.plist" now exp pid
  security cms -D -i "$APP/Contents/embedded.provisionprofile" > "$pp" 2>/dev/null || true
  if [ ! -s "$pp" ]; then
    fail "embedded.provisionprofile cannot be decoded"
  else
    pid="$(/usr/libexec/PlistBuddy -c 'Print :Entitlements:com.apple.application-identifier' "$pp" 2>/dev/null || true)"
    [ "$pid" = "$MAS_TEAM_ID.ai.mirastacklabs.miramail" ] || fail "profile application-identifier is '$pid', expected $MAS_TEAM_ID.ai.mirastacklabs.miramail"
    out="$(/usr/libexec/PlistBuddy -c 'Print :TeamIdentifier:0' "$pp" 2>/dev/null || true)"
    [ "$out" = "$MAS_TEAM_ID" ] || fail "profile team is '$out', expected $MAS_TEAM_ID"
    if /usr/libexec/PlistBuddy -c 'Print :ProvisionedDevices' "$pp" >/dev/null 2>&1; then fail "profile lists devices: it is a development profile, not Mac App Store"; fi
    out="$(/usr/libexec/PlistBuddy -c 'Print :Entitlements:com.apple.security.get-task-allow' "$pp" 2>/dev/null || true)"
    [ "$out" != "true" ] || fail "profile has get-task-allow: it is a development profile"
    exp="$(plutil -extract ExpirationDate raw -o - "$pp" 2>/dev/null || true)"
    now="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
    if [ -z "$exp" ]; then fail "cannot read the profile expiry"; elif [[ "$exp" < "$now" ]]; then fail "profile expired on $exp"; else echo "preflight: profile valid until $exp"; fi
  fi
  rm -f "$pp"
  # sandbox entitlement actually signed in
  out="$(codesign -d --entitlements - "$APP" 2>/dev/null || true)"
  grep -Fq "com.apple.security.app-sandbox" <<<"$out" || fail "app-sandbox entitlement missing from the signature"
  # icon: valid icns with the 1024px (512@2x) image (scratch dir inside the repo's target/)
  tmp="$OUTDIR/iconcheck"; rm -rf "$tmp"; mkdir -p "$tmp"
  iconutil -c iconset "$APP/Contents/Resources/icon.icns" -o "$tmp/i.iconset" >/dev/null 2>&1 || fail "icon.icns cannot be expanded"
  out="$(sips -g pixelWidth "$tmp/i.iconset/icon_512x512@2x.png" 2>/dev/null | awk '/pixelWidth/{print $2}' || true)"
  [ "$out" = "1024" ] || fail "icon.icns lacks the 1024px image"
  rm -rf "$tmp"
  [ "$bad" = 0 ] || die "pre-flight failed, not building the .pkg"
  echo "preflight: all checks passed"
}
preflight

# Installer package (signed with the installer certificate).
productbuild --sign "$MAS_INSTALLER_IDENTITY" --component "$APP" /Applications "$OUTDIR/MiraMail.pkg"
pkgutil --check-signature "$OUTDIR/MiraMail.pkg"
# The upload check (Apple 91109) runs on the PACKAGE: expand it inside target/ and re-check the payload for quarantine.
rm -rf "$OUTDIR/pkgcheck"
pkgutil --expand-full "$OUTDIR/MiraMail.pkg" "$OUTDIR/pkgcheck" >/dev/null || die "cannot expand the .pkg for the final check"
POUT="$(xattr -lr "$OUTDIR/pkgcheck" 2>/dev/null || true)"
rm -rf "$OUTDIR/pkgcheck"
if grep -Fq "com.apple.quarantine" <<<"$POUT"; then die "the .pkg payload contains com.apple.quarantine (Apple error 91109)"; fi
echo "postcheck: the .pkg payload has no quarantine attributes"

if [ "${MAS_VALIDATE:-0}" = "1" ]; then
  : "${MAS_ALTOOL_USER:?Set MAS_ALTOOL_USER (Apple ID) for validation}"
  : "${MAS_ALTOOL_PASSWORD:?Set MAS_ALTOOL_PASSWORD (app-specific password, e.g. @keychain:AC_PASSWORD)}"
  xcrun altool --validate-app -f "$OUTDIR/MiraMail.pkg" -t macos -u "$MAS_ALTOOL_USER" -p "$MAS_ALTOOL_PASSWORD"
else
  echo "Skipped validation. Upload $OUTDIR/MiraMail.pkg with the Transporter app (it validates on upload), or re-run with MAS_VALIDATE=1."
fi
echo "Done: $OUTDIR/MiraMail.pkg"
