#!/bin/bash
# Manual simulator build/install/launch for the iOS spike.
# Why manual: `tauri ios build/dev` refuses here ("Xcode Simulator SDK 27.2 is not installed"): the only
# installed simulator runtime is iOS 26.5 while the Xcode SDK is 27.2. We therefore do what the CLI's
# Xcode script does ourselves: cargo build the staticlib, copy it to Externals/arm64/<cfg>/libapp.a,
# run xcodebuild without the "Build Rust Code" pre-build phase. Device builds do not go through this.
set -euo pipefail
cd "$(dirname "$0")/../.."          # repo root
export PATH="/opt/homebrew/opt/rustup/bin:$PATH" RUSTUP_TOOLCHAIN=1.99.1
UDID="${UDID:-DC94BB22-3A2D-4460-9718-DA6B711876BC}"   # existing "iPhone 17 Pro"; never create/erase simulators
OUT=target/ios-spike
APPLE=app-ios/src-tauri/gen/apple

CARGO_TARGET_DIR=$OUT/cargo cargo build --release -p miramail-ios --lib --features tauri/custom-protocol --target aarch64-apple-ios-sim
mkdir -p $APPLE/Externals/arm64/release
cp $OUT/cargo/aarch64-apple-ios-sim/release/libmiramail_ios_lib.a $APPLE/Externals/arm64/release/libapp.a

# project.sim.yml = generated project.yml minus the preBuildScripts, plus the bgrefresh Swift package and Info.plist keys.
python3 - <<'PY'
s = open("app-ios/src-tauri/gen/apple/project.yml").read()
s = s[: s.index("    preBuildScripts:")].rstrip() + "\n"
s = s.replace("targetTemplates:", "packages:\n  tauri-plugin-bgrefresh:\n    path: ../../plugins/bgrefresh/ios\ntargetTemplates:", 1)
s = s.replace("      - sdk: CoreGraphics.framework", "      - package: tauri-plugin-bgrefresh\n        product: tauri-plugin-bgrefresh\n      - sdk: CoreGraphics.framework", 1)
s = s.replace("        LSRequiresIPhoneOS: true", "        LSRequiresIPhoneOS: true\n        UIBackgroundModes: [fetch]\n        BGTaskSchedulerPermittedIdentifiers: [ai.mirastacklabs.miramail.refresh]", 1)
open("app-ios/src-tauri/gen/apple/project.sim.yml", "w").write(s)
PY
(cd $APPLE && xcodegen -q -s project.sim.yml -p .)
xcodebuild -project $APPLE/miramail-ios.xcodeproj -scheme miramail-ios_iOS -configuration release \
  -sdk iphonesimulator -arch arm64 -derivedDataPath $OUT/dd \
  CODE_SIGN_IDENTITY=- CODE_SIGNING_ALLOWED=YES DEVELOPMENT_TEAM= build 2>&1 | tail -40
APP=$(ls -d $OUT/dd/Build/Products/release-iphonesimulator/*.app | head -1)
echo "APP=$APP"
