import Foundation

// The snapshot the app writes into the App Group container (<group>/widget.json). The widgets only READ it: no mail
// server, no Keychain, no network. Writer and schema: app/src-tauri/src/widget_snapshot.rs (change both together and
// bump `version`).
//
//   { "version": 1, "updated_at": <unix s>, "details": <bool>,
//     "accounts": [ { "label": "Account 1", "unread": <n> } ],
//     "latest":   [ { "from_name": <string|null>, "subject": <string|null>, "received_at": <unix s> } ],   // <= 5, newest first
//     "calendar": [ { "title": <string|null>, "start": <unix s>, "end": <unix s>, "all_day": <bool>, "colour": "#rrggbb" } ] }
//
// `from_name` / `subject` / real event titles are present only when the user opted in ("details"); otherwise they are
// null and event titles are "Busy". No email address is ever in the file.

struct WidgetAccount: Codable, Equatable {
  let label: String
  let unread: Int
}

struct WidgetLatest: Codable, Equatable {
  let fromName: String?
  let subject: String?
  let receivedAt: Double
  var date: Date { Date(timeIntervalSince1970: receivedAt) }
}

struct WidgetEvent: Codable, Equatable {
  let title: String?
  let start: Double
  let end: Double
  let allDay: Bool
  let colour: String
  var startDate: Date { Date(timeIntervalSince1970: start) }
  var endDate: Date { Date(timeIntervalSince1970: end) }
}

struct WidgetSnapshot: Codable, Equatable {
  static let supportedVersion = 1
  /// Older than this and the widgets ask the user to open the app instead of showing old numbers.
  static let staleAfter: TimeInterval = 24 * 3600

  let version: Int
  let updatedAt: Double
  let details: Bool
  let accounts: [WidgetAccount]
  let latest: [WidgetLatest]
  let calendar: [WidgetEvent]

  var updated: Date { Date(timeIntervalSince1970: updatedAt) }
  var unreadTotal: Int { accounts.reduce(0) { $0 + max(0, $1.unread) } }
  func isStale(at now: Date) -> Bool { now.timeIntervalSince(updated) > WidgetSnapshot.staleAfter }

  static func decode(_ data: Data) -> WidgetSnapshot? {
    let d = JSONDecoder()
    d.keyDecodingStrategy = .convertFromSnakeCase
    guard let s = try? d.decode(WidgetSnapshot.self, from: data), s.version == supportedVersion else { return nil }
    return s
  }

  /// The group is "group.<app bundle id>"; this extension's id is "<app bundle id>.widgets".
  static var groupId: String? {
    guard var id = Bundle.main.bundleIdentifier else { return nil }
    if id.hasSuffix(".widgets") { id.removeLast(".widgets".count) }
    return "group." + id
  }

  static func load() -> WidgetSnapshot? {
    guard let g = groupId,
          let dir = FileManager.default.containerURL(forSecurityApplicationGroupIdentifier: g),
          let data = try? Data(contentsOf: dir.appendingPathComponent("widget.json")) else { return nil }
    return decode(data)
  }

  /// Fixed sample for the widget gallery (nothing from the user).
  static var sample: WidgetSnapshot {
    let now = Date().timeIntervalSince1970
    return WidgetSnapshot(
      version: supportedVersion, updatedAt: now, details: true,
      accounts: [WidgetAccount(label: "Account 1", unread: 4)],
      latest: [
        WidgetLatest(fromName: "Ann", subject: "Lunch on Friday?", receivedAt: now - 300),
        WidgetLatest(fromName: "Ben", subject: "Notes from today", receivedAt: now - 2400),
        WidgetLatest(fromName: "Cleo", subject: "Invoice 1042", receivedAt: now - 7200),
      ],
      calendar: [
        WidgetEvent(title: "Standup", start: now + 1800, end: now + 3600, allDay: false, colour: "#00c9b1"),
        WidgetEvent(title: "Design review", start: now + 7200, end: now + 10800, allDay: false, colour: "#ff671f"),
      ])
  }
}
