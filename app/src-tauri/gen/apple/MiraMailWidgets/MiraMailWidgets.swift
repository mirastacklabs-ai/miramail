import SwiftUI
import WidgetKit

// MiraMail home-screen widgets. Periodic, not live: the app writes widget.json (see Snapshot.swift) and reloads the
// timelines; between those the system refreshes every ~15 minutes. Tapping opens the app (miramail://mail or
// miramail://calendar). No network, no Keychain.

// MARK: - Brand (app/src/app.css tokens)

enum Brand {
  static func bg(_ s: ColorScheme) -> Color { s == .dark ? Color(red: 0.039, green: 0.039, blue: 0.039) : Color(red: 0.965, green: 0.961, blue: 0.945) }
  static func ink(_ s: ColorScheme) -> Color { s == .dark ? .white : Color(red: 0.039, green: 0.039, blue: 0.039) }
  static func muted(_ s: ColorScheme) -> Color { s == .dark ? Color(red: 0.56, green: 0.56, blue: 0.56) : Color(red: 0.37, green: 0.37, blue: 0.37) }
  /// --logo-teal (#00c9b1 dark / #007a6e light)
  static func teal(_ s: ColorScheme) -> Color { s == .dark ? Color(red: 0, green: 0.788, blue: 0.694) : Color(red: 0, green: 0.478, blue: 0.431) }
  static func hex(_ h: String, fallback: Color) -> Color {
    var t = h.trimmingCharacters(in: .whitespaces)
    if t.hasPrefix("#") { t.removeFirst() }
    guard t.count == 6, let v = UInt32(t, radix: 16) else { return fallback }
    return Color(red: Double((v >> 16) & 255) / 255, green: Double((v >> 8) & 255) / 255, blue: Double(v & 255) / 255)
  }
}

extension View {
  /// iOS 17 wants containerBackground; earlier systems take a plain background.
  @ViewBuilder func widgetBackground(_ c: Color) -> some View {
    if #available(iOS 17.0, *) { self.containerBackground(c, for: .widget) } else { self.background(c) }
  }
}

/// The MiraMail mark: teal envelope with a dark chevron (same drawing as components/Logo.svelte).
struct MarkView: View {
  var height: CGFloat = 16
  var body: some View {
    let w = height * 24 / 18
    ZStack {
      RoundedRectangle(cornerRadius: height * 4 / 18).fill(Color(red: 0.169, green: 0.831, blue: 0.749))
      Path { p in
        p.move(to: CGPoint(x: w * 3.5 / 24, y: height * 4.5 / 18))
        p.addLine(to: CGPoint(x: w * 12 / 24, y: height * 11 / 18))
        p.addLine(to: CGPoint(x: w * 20.5 / 24, y: height * 4.5 / 18))
      }
      .stroke(Color(red: 0.039, green: 0.039, blue: 0.039), style: StrokeStyle(lineWidth: height * 2.6 / 18, lineCap: .round, lineJoin: .round))
    }
    .frame(width: w, height: height)
    .accessibilityHidden(true)
  }
}

struct SyncPrompt: View {
  @Environment(\.colorScheme) private var scheme
  var body: some View {
    VStack(alignment: .leading, spacing: 8) {
      MarkView(height: 16)
      Spacer(minLength: 0)
      Text("Open MiraMail to sync").font(.system(size: 14, weight: .semibold)).foregroundColor(Brand.ink(scheme))
    }
    .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .leading)
  }
}

// MARK: - Mail widget

struct SnapshotEntry: TimelineEntry {
  let date: Date
  let snap: WidgetSnapshot?
}

struct MailProvider: TimelineProvider {
  func placeholder(in context: Context) -> SnapshotEntry { SnapshotEntry(date: Date(), snap: .sample) }
  func getSnapshot(in context: Context, completion: @escaping (SnapshotEntry) -> Void) {
    completion(SnapshotEntry(date: Date(), snap: context.isPreview ? .sample : WidgetSnapshot.load()))
  }
  func getTimeline(in context: Context, completion: @escaping (Timeline<SnapshotEntry>) -> Void) {
    let now = Date()
    let snap = WidgetSnapshot.load()
    var entries = [SnapshotEntry(date: now, snap: snap)]
    // Flip to "Open MiraMail to sync" on its own when the numbers get too old.
    if let s = snap {
      let stale = s.updated.addingTimeInterval(WidgetSnapshot.staleAfter + 1)
      if stale > now { entries.append(SnapshotEntry(date: stale, snap: snap)) }
    }
    completion(Timeline(entries: entries, policy: .after(now.addingTimeInterval(15 * 60))))
  }
}

struct MailWidgetView: View {
  let entry: SnapshotEntry
  @Environment(\.widgetFamily) private var family
  @Environment(\.colorScheme) private var scheme

  var body: some View {
    Group {
      if let s = entry.snap, !s.isStale(at: entry.date) {
        if family == .systemSmall { small(s) } else { medium(s) }
      } else {
        SyncPrompt()
      }
    }
    .widgetBackground(Brand.bg(scheme))
    .widgetURL(URL(string: "miramail://mail"))
  }

  private func count(_ s: WidgetSnapshot) -> some View {
    VStack(alignment: .leading, spacing: 0) {
      MarkView(height: 16)
      Spacer(minLength: 0)
      Text("\(s.unreadTotal)")
        .font(.system(size: 44, weight: .bold, design: .rounded))
        .minimumScaleFactor(0.5).lineLimit(1)
        .foregroundColor(s.unreadTotal > 0 ? Brand.teal(scheme) : Brand.ink(scheme))
      Text(s.unreadTotal == 0 ? "All caught up" : (s.unreadTotal == 1 ? "unread message" : "unread messages"))
        .font(.system(size: 12, weight: .medium)).foregroundColor(Brand.muted(scheme)).lineLimit(1)
    }
    .accessibilityElement(children: .combine)
    .accessibilityLabel("\(s.unreadTotal) unread messages")
  }

  private func small(_ s: WidgetSnapshot) -> some View {
    count(s).frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .leading)
  }

  private func medium(_ s: WidgetSnapshot) -> some View {
    HStack(alignment: .top, spacing: 14) {
      count(s).frame(width: 110, alignment: .leading)
      Rectangle().fill(Brand.muted(scheme).opacity(0.3)).frame(width: 1)
      VStack(alignment: .leading, spacing: 8) {
        if s.latest.isEmpty {
          Text("No unread mail").font(.system(size: 13)).foregroundColor(Brand.muted(scheme))
        }
        ForEach(Array(s.latest.prefix(3).enumerated()), id: \.offset) { _, m in
          VStack(alignment: .leading, spacing: 1) {
            if let n = m.fromName, !n.isEmpty {
              Text(n).font(.system(size: 13, weight: .semibold)).foregroundColor(Brand.ink(scheme)).lineLimit(1)
              if let sub = m.subject, !sub.isEmpty {
                Text(sub).font(.system(size: 12)).foregroundColor(Brand.muted(scheme)).lineLimit(1)
              } else {
                Text(m.date, style: .relative).font(.system(size: 12)).foregroundColor(Brand.muted(scheme)).lineLimit(1)
              }
            } else {
              Text("New message").font(.system(size: 13, weight: .semibold)).foregroundColor(Brand.ink(scheme)).lineLimit(1)
              Text(m.date, style: .relative).font(.system(size: 12)).foregroundColor(Brand.muted(scheme)).lineLimit(1)
            }
          }
        }
        Spacer(minLength: 0)
      }
      .frame(maxWidth: .infinity, alignment: .leading)
    }
  }
}

struct MailWidget: Widget {
  var body: some WidgetConfiguration {
    StaticConfiguration(kind: "MiraMailMail", provider: MailProvider()) { entry in
      MailWidgetView(entry: entry)
    }
    .configurationDisplayName("MiraMail")
    .description("Unread count and the latest senders as of the last sync.")
    .supportedFamilies([.systemSmall, .systemMedium])
  }
}

// MARK: - Calendar widget

struct CalendarProvider: TimelineProvider {
  func placeholder(in context: Context) -> SnapshotEntry { SnapshotEntry(date: Date(), snap: .sample) }
  func getSnapshot(in context: Context, completion: @escaping (SnapshotEntry) -> Void) {
    completion(SnapshotEntry(date: Date(), snap: context.isPreview ? .sample : WidgetSnapshot.load()))
  }
  func getTimeline(in context: Context, completion: @escaping (Timeline<SnapshotEntry>) -> Void) {
    let now = Date()
    let snap = WidgetSnapshot.load()
    var entries = [SnapshotEntry(date: now, snap: snap)]
    // One entry at every start / end in the next 24 h, so "next event" moves on without the app (works offline).
    if let s = snap {
      var marks = Set<Double>()
      let horizon = now.addingTimeInterval(24 * 3600)
      for e in s.calendar {
        for d in [e.startDate, e.endDate] where d > now && d < horizon { marks.insert(d.timeIntervalSince1970) }
      }
      marks.insert(s.updated.addingTimeInterval(WidgetSnapshot.staleAfter + 1).timeIntervalSince1970)
      for m in marks.sorted().prefix(16) where Date(timeIntervalSince1970: m) > now {
        entries.append(SnapshotEntry(date: Date(timeIntervalSince1970: m), snap: snap))
      }
    }
    completion(Timeline(entries: entries, policy: .after(now.addingTimeInterval(15 * 60))))
  }
}

struct CalendarWidgetView: View {
  let entry: SnapshotEntry
  @Environment(\.widgetFamily) private var family
  @Environment(\.colorScheme) private var scheme

  var body: some View {
    Group {
      if let s = entry.snap, !s.isStale(at: entry.date) {
        if family == .systemSmall { small(s) } else { medium(s) }
      } else {
        SyncPrompt()
      }
    }
    .widgetBackground(Brand.bg(scheme))
    .widgetURL(URL(string: "miramail://calendar"))
  }

  private func colour(_ e: WidgetEvent) -> Color { Brand.hex(e.colour, fallback: Brand.teal(scheme)) }
  private func title(_ e: WidgetEvent) -> String { (e.title?.isEmpty == false ? e.title! : "Busy") }

  /// Events that touch today and have not ended yet, all-day first.
  private func today(_ s: WidgetSnapshot) -> [WidgetEvent] {
    let cal = Calendar.current
    let start = cal.startOfDay(for: entry.date)
    let end = cal.date(byAdding: .day, value: 1, to: start) ?? start.addingTimeInterval(86400)
    return s.calendar
      .filter { $0.startDate < end && $0.endDate > start && $0.endDate > entry.date }
      .sorted { ($0.allDay ? 0 : 1, $0.start) < ($1.allDay ? 0 : 1, $1.start) }
  }

  private func when(_ e: WidgetEvent) -> some View {
    Group {
      if e.allDay { Text("All day") } else { Text(e.startDate, style: .time) }
    }
    .font(.system(size: 12, weight: .medium)).foregroundColor(Brand.muted(scheme)).lineLimit(1)
  }

  private func small(_ s: WidgetSnapshot) -> some View {
    let upcoming = today(s)
    let next = upcoming.first(where: { !$0.allDay }) ?? upcoming.first
    return VStack(alignment: .leading, spacing: 4) {
      MarkView(height: 16)
      Spacer(minLength: 0)
      if let e = next {
        RoundedRectangle(cornerRadius: 2).fill(colour(e)).frame(width: 28, height: 4)
        Text(title(e)).font(.system(size: 15, weight: .semibold)).foregroundColor(Brand.ink(scheme)).lineLimit(2)
        when(e)
        if upcoming.count > 1 {
          Text("+\(upcoming.count - 1) more today").font(.system(size: 11)).foregroundColor(Brand.muted(scheme))
        }
      } else {
        Text("Nothing more today").font(.system(size: 15, weight: .semibold)).foregroundColor(Brand.ink(scheme))
      }
    }
    .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .leading)
  }

  private func medium(_ s: WidgetSnapshot) -> some View {
    let upcoming = today(s)
    return VStack(alignment: .leading, spacing: 6) {
      HStack {
        MarkView(height: 14)
        Text(entry.date, format: .dateTime.weekday(.wide).day().month(.abbreviated))
          .font(.system(size: 12, weight: .medium)).foregroundColor(Brand.muted(scheme))
        Spacer(minLength: 0)
      }
      if upcoming.isEmpty {
        Spacer(minLength: 0)
        Text("Nothing more today").font(.system(size: 14, weight: .semibold)).foregroundColor(Brand.ink(scheme))
      }
      ForEach(Array(upcoming.prefix(4).enumerated()), id: \.offset) { _, e in
        HStack(spacing: 8) {
          RoundedRectangle(cornerRadius: 2).fill(colour(e)).frame(width: 4, height: 28)
          VStack(alignment: .leading, spacing: 0) {
            Text(title(e)).font(.system(size: 13, weight: .semibold)).foregroundColor(Brand.ink(scheme)).lineLimit(1)
            if e.allDay {
              Text("All day").font(.system(size: 11)).foregroundColor(Brand.muted(scheme))
            } else {
              (Text(e.startDate, style: .time) + Text(" - ") + Text(e.endDate, style: .time))
                .font(.system(size: 11)).foregroundColor(Brand.muted(scheme)).lineLimit(1)
            }
          }
          Spacer(minLength: 0)
        }
      }
      if upcoming.count > 4 {
        Text("+\(upcoming.count - 4) more").font(.system(size: 11)).foregroundColor(Brand.muted(scheme))
      }
      Spacer(minLength: 0)
    }
    .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
  }
}

struct CalendarWidget: Widget {
  var body: some WidgetConfiguration {
    StaticConfiguration(kind: "MiraMailCalendar", provider: CalendarProvider()) { entry in
      CalendarWidgetView(entry: entry)
    }
    .configurationDisplayName("MiraMail Calendar")
    .description("Today's events. Works offline.")
    .supportedFamilies([.systemSmall, .systemMedium])
  }
}

@main
struct MiraMailWidgetsBundle: WidgetBundle {
  var body: some Widget {
    MailWidget()
    CalendarWidget()
  }
}
