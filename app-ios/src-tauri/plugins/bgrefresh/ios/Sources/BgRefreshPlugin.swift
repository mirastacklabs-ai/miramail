import BackgroundTasks
import SwiftRs
import Tauri
import UIKit
import WebKit
import UserNotifications
import os

/// Implemented in Rust (tauri-plugin-bgrefresh, `bgrefresh_run_check`); linked in the same process.
@_silgen_name("bgrefresh_run_check")
func bgrefresh_run_check(_ buf: UnsafeMutablePointer<UInt8>?, _ cap: Int) -> Int32

private let taskId = "ai.mirastacklabs.miramail.refresh"
private let log = Logger(subsystem: "ai.mirastacklabs.miramail", category: "bgrefresh")

struct BgStatus: Encodable {
  var scheduled: Bool
  var notifications: String
  var fires: Int
  var last_fire: String
  var last_result: String
}

class BgRefreshPlugin: Plugin {
  static var registered = false

  override func load(webview: WKWebView) {
    // BGTaskScheduler requires registration before the app finishes launching; if Tauri's plugin
    // load runs later, register() raises. We log either way so the spike can tell which happened.
    if !BgRefreshPlugin.registered {
      BgRefreshPlugin.registered = BGTaskScheduler.shared.register(forTaskWithIdentifier: taskId, using: nil) { task in
        BgRefreshPlugin.handle(task as! BGAppRefreshTask)
      }
      log.notice("bgrefresh: register returned \(BgRefreshPlugin.registered)")
    }
    if ProcessInfo.processInfo.arguments.contains("--autotest") {  // headless simulator verification only
      log.notice("bgrefresh: autotest submit -> \(BgRefreshPlugin.submit())")
      UNUserNotificationCenter.current().requestAuthorization(options: [.alert, .sound, .badge]) { ok, err in
        log.notice("bgrefresh: autotest notification auth granted=\(ok)")
      }
      // Same code path the BGAppRefreshTask handler runs (Swift -> Rust handler -> local notification).
      DispatchQueue.global(qos: .utility).asyncAfter(deadline: .now() + 3) {
        let ok = BgRefreshPlugin.runCheckAndNotify(origin: "autotest")
        log.notice("bgrefresh: autotest runCheckAndNotify ok=\(ok)")
      }
    }
  }

  static func submit() -> String {
    let req = BGAppRefreshTaskRequest(identifier: taskId)
    req.earliestBeginDate = Date(timeIntervalSinceNow: 15 * 60)
    do {
      try BGTaskScheduler.shared.submit(req)
      log.notice("bgrefresh: submitted")
      return "submitted"
    } catch {
      log.error("bgrefresh: submit failed \(error.localizedDescription)")
      return "submit failed: \(error.localizedDescription)"
    }
  }

  /// Runs the Rust check and posts a local notification. Blocking; call off the main thread.
  static func runCheckAndNotify(origin: String) -> Bool {
    var buf = [UInt8](repeating: 0, count: 512)
    let code = buf.withUnsafeMutableBufferPointer { bgrefresh_run_check($0.baseAddress, $0.count) }
    let msg = String(cString: buf)
    log.notice("bgrefresh: rust check code=\(code) origin=\(origin)")

    let d = UserDefaults.standard
    d.set(d.integer(forKey: "bg.fires") + 1, forKey: "bg.fires")
    d.set(ISO8601DateFormatter().string(from: Date()) + " (" + origin + ")", forKey: "bg.last")
    d.set("code=\(code) \(msg)", forKey: "bg.result")

    let content = UNMutableNotificationContent()
    content.title = "MiraMail"
    content.body = code == 0 ? msg : "Check failed"
    let sem = DispatchSemaphore(value: 0)
    UNUserNotificationCenter.current().add(UNNotificationRequest(identifier: UUID().uuidString, content: content, trigger: nil)) { err in
      if let err = err { log.error("bgrefresh: notify failed \(err.localizedDescription)") }
      else { log.notice("bgrefresh: notification posted") }
      sem.signal()
    }
    _ = sem.wait(timeout: .now() + 5)
    return code == 0
  }

  static func handle(_ task: BGAppRefreshTask) {
    log.notice("bgrefresh: task fired")
    _ = submit()  // keep the chain going
    task.expirationHandler = { log.error("bgrefresh: expired") }
    DispatchQueue.global(qos: .utility).async {
      let ok = runCheckAndNotify(origin: "BGAppRefreshTask")
      task.setTaskCompleted(success: ok)
    }
  }

  func status(then: @escaping (BgStatus) -> Void) {
    BGTaskScheduler.shared.getPendingTaskRequests { reqs in
      UNUserNotificationCenter.current().getNotificationSettings { s in
        let d = UserDefaults.standard
        let n: String
        switch s.authorizationStatus {
        case .authorized: n = "authorized"
        case .denied: n = "denied"
        case .notDetermined: n = "notDetermined"
        case .provisional: n = "provisional"
        default: n = "other"
        }
        then(BgStatus(
          scheduled: reqs.contains { $0.identifier == taskId },
          notifications: n,
          fires: d.integer(forKey: "bg.fires"),
          last_fire: d.string(forKey: "bg.last") ?? "never",
          last_result: (d.string(forKey: "bg.result") ?? "") + (BgRefreshPlugin.registered ? "" : " [register FAILED]")))
      }
    }
  }

  @objc public func schedule(_ invoke: Invoke) {
    _ = BgRefreshPlugin.submit()
    status { invoke.resolve($0) }
  }

  @objc public func requestNotifications(_ invoke: Invoke) {
    UNUserNotificationCenter.current().requestAuthorization(options: [.alert, .sound, .badge]) { _, _ in
      self.status { invoke.resolve($0) }
    }
  }

  @objc public func status(_ invoke: Invoke) {
    status { invoke.resolve($0) }
  }

  @objc public func simulateFire(_ invoke: Invoke) {
    DispatchQueue.global(qos: .utility).async {
      _ = BgRefreshPlugin.runCheckAndNotify(origin: "manual")
      self.status { invoke.resolve($0) }
    }
  }
}

@_cdecl("init_plugin_bgrefresh")
public func initPlugin() -> Plugin {
  return BgRefreshPlugin()
}
