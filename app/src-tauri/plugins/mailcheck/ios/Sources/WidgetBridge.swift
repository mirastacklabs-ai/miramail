import Tauri
import WidgetKit

// Home-screen widgets: the bridge between the Rust snapshot writer (app/src-tauri/src/widget_snapshot.rs) and the
// WidgetKit extension (gen/apple/MiraMailWidgets). The widgets read only a JSON file from the App Group container;
// they never touch the Keychain or the network. Kept separate from MailCheckPlugin.swift: these are extensions of it,
// found by Tauri through the Objective-C selectors.
//
// App Group id: "group." + the app's bundle id (the extension's id is "<app id>.widgets"), so a build with another
// development id (.spike / .dev, see app/scripts/ios-device.sh) uses its own group. The shipping App ID is never used.

struct WidgetDirReply: Encodable {
  let path: String?
}

enum WidgetBridge {
  /// "group.<app bundle id>"; nil if the bundle id is unknown.
  static var groupId: String? {
    guard let id = Bundle.main.bundleIdentifier, !id.isEmpty else { return nil }
    return "group." + id
  }

  /// The shared container, or nil when the App Group is not provisioned for this build.
  static func containerPath() -> String? {
    guard let g = groupId,
          let url = FileManager.default.containerURL(forSecurityApplicationGroupIdentifier: g) else { return nil }
    return url.path
  }
}

extension MailCheckPlugin {
  /// Path of the App Group container (`path` is null when unavailable; the widgets then show "Open MiraMail to sync").
  @objc public func widgetContainer(_ invoke: Invoke) {
    invoke.resolve(WidgetDirReply(path: WidgetBridge.containerPath()))
  }

  /// Ask WidgetKit to refresh every MiraMail widget now (Rust throttles this to once per 5 minutes unless counts changed).
  @objc public func widgetReload(_ invoke: Invoke) {
    WidgetCenter.shared.reloadAllTimelines()
    invoke.resolve()
  }
}
