# Generated iOS project: do not provision the shipping App ID

`project.yml` and `miramail.xcodeproj` carry the spike id `ai.mirastacklabs.miramail.spike` and no `DEVELOPMENT_TEAM`.
Never open this project in Xcode with automatic signing on `ai.mirastacklabs.miramail` (the Mac App Store App ID):
Xcode would create or modify that App ID and its profiles. Build on a device with `app/scripts/ios-device.sh`
(it refuses any id that is not a sub-id such as `.spike`). `tauri ios init` regenerates these files from
`tauri.conf.json` (`identifier`, which stays the shipping id for the Mac build): re-apply the spike id afterwards.

## Do not run `tauri ios init/dev/build`

They sign with the shipping App ID (`ai.mirastacklabs.miramail`), which Xcode's automatic provisioning would then modify.
Build with `app/scripts/ios-device.sh` (signed bundle id comes from `PRODUCT_BUNDLE_IDENTIFIER`, `.spike` only).

The app data dir derives from the config identifier (`ai.mirastacklabs.miramail.spike` on iOS, via `src-tauri/tauri.ios.conf.json`).
Never change it, or the app starts with an empty database.
