import BackgroundTasks
import SwiftRs
import Tauri
import UIKit
import WebKit
import Network
import UserNotifications
import os

/// Implemented in Rust (tauri-plugin-mailcheck); linked in the same process.
@_silgen_name("mailcheck_run_check")
func mailcheck_run_check(_ buf: UnsafeMutablePointer<UInt8>?, _ cap: Int) -> Int32
@_silgen_name("mailcheck_cancel")
func mailcheck_cancel()
/// Implemented in Rust: iOS handed over a new refresh task (clears a cancel left over from an earlier one).
@_silgen_name("mailcheck_task_begin")
func mailcheck_task_begin()
/// Implemented in Rust: 1 = the user is in front of the app, 0 = not. Rust decides what may run (see lib.rs, presence.rs).
@_silgen_name("mailcheck_set_present")
func mailcheck_set_present(_ present: Int32)

/// Implemented in Rust: the current network path is constrained (Low Data Mode) / expensive (cellular, hotspot); 1 = yes.
@_silgen_name("mailcheck_set_path")
func mailcheck_set_path(_ constrained: Int32, _ expensive: Int32)

/// Fixed id (iOS accepts any string). It must equal BGTaskSchedulerPermittedIdentifiers in Info.plist.
private let taskId = "ai.mirastacklabs.miramail.refresh"
/// Notification identifiers that announce a message start with this (see app/src-tauri/src/notify.rs): "mm:<uid>:<account>".
private let targetPrefix = "mm:"
private let log = Logger(subsystem: "ai.mirastacklabs.miramail", category: "mailcheck")

struct BadgeArgs: Decodable {
  let count: Int
}

struct TapInfo: Encodable {
  let account: String
  let folder: String
  let uid: Int
}

struct AllowedReply: Encodable {
  let allowed: Bool
}

struct TapReply: Encodable {
  let tap: TapInfo?
}

/// Only reads the notification identifier; never logs it (it contains the account address).
final class TapDelegate: NSObject, UNUserNotificationCenterDelegate {
  /// Set when a notification announcing a message is tapped; taken (once) by Rust via `takeTap`.
  private static let lock = NSLock()
  private static var _pending: TapInfo? = nil
  static func setPending(_ t: TapInfo?) { lock.lock(); _pending = t; lock.unlock() }
  /// Returns the pending tap and clears it, in one step.
  static func takePending() -> TapInfo? { lock.lock(); defer { lock.unlock() }; let t = _pending; _pending = nil; return t }
  static weak var webview: WKWebView? = nil

  /// A banner is shown only for the "send a test" notification while the app is open; real new-mail notifications
  /// are not shown while MiraMail is in front (the user is looking at it).
  func userNotificationCenter(_ center: UNUserNotificationCenter, willPresent notification: UNNotification,
                              withCompletionHandler completionHandler: @escaping (UNNotificationPresentationOptions) -> Void) {
    if notification.request.identifier.hasPrefix("miramail-test") {
      completionHandler([.banner, .list, .sound])
    } else {
      completionHandler([])
    }
  }

  func userNotificationCenter(_ center: UNUserNotificationCenter, didReceive response: UNNotificationResponse,
                              withCompletionHandler completionHandler: @escaping () -> Void) {
    if response.actionIdentifier == UNNotificationDefaultActionIdentifier,
       let t = TapDelegate.parse(response.notification.request.identifier) {
      TapDelegate.setPending(t)
      // Tell the page to ask for it (no payload goes through JavaScript). On a cold start the page asks by itself.
      DispatchQueue.main.async {
        TapDelegate.webview?.evaluateJavaScript("window.dispatchEvent(new Event('miramail-tap'))", completionHandler: nil)
      }
    }
    completionHandler()
  }

  /// "mm:<uid>:<account>" -> {account, folder: INBOX, uid}. Anything else is not a message notification.
  static func parse(_ id: String) -> TapInfo? {
    guard id.hasPrefix(targetPrefix) else { return nil }
    let parts = id.dropFirst(targetPrefix.count).split(separator: ":", maxSplits: 1, omittingEmptySubsequences: false)
    guard parts.count == 2, let uid = UInt32(parts[0]), !parts[1].isEmpty else { return nil }
    return TapInfo(account: String(parts[1]), folder: "INBOX", uid: Int(uid))
  }
}

/// Completes a background task exactly once, whichever of "worker done" and "expired" comes first. Never blocks.
final class TaskCompletion {
  private let lock = NSLock()
  private var finished = false
  private let task: BGTask
  init(_ task: BGTask) { self.task = task }
  func complete(_ ok: Bool) {
    lock.lock()
    let first = !finished
    finished = true
    lock.unlock()
    if first { task.setTaskCompleted(success: ok) }
  }
}

class MailCheckPlugin: Plugin {
  static var booted = false
  static var registered = false
  static let delegate = TapDelegate()
  static var backgroundObserver: NSObjectProtocol? = nil
  static var activeObserver: NSObjectProtocol? = nil
  /// Public API only: tells Rust whether Low Data Mode applies to the current path (read-only; optional downloads wait).
  static var pathMonitor: NWPathMonitor? = nil

  /// Everything that must be in place before application(_:didFinishLaunchingWithOptions:) returns. Runs once. Called
  /// from `initPlugin` (plugin setup, which Tauri runs inside didFinishLaunching on the main thread, and which does not
  /// need the web view), and again (no-op) from `load(webview:)`.
  static func bootstrap() {
    if booted { return }
    booted = true
    UNUserNotificationCenter.current().delegate = delegate
    // BGTaskScheduler needs the registration before the app finishes launching. The handler is queued on the main queue
    // so it cannot run before launch (and so the Rust setup that installs the check handler) has finished.
    registered = BGTaskScheduler.shared.register(forTaskWithIdentifier: taskId, using: DispatchQueue.main) { task in
      MailCheckPlugin.handle(task as! BGAppRefreshTask)
    }
    log.notice("mailcheck: register returned \(registered)")
    // Rust is told every time the user arrives or leaves; it starts / holds back everything that talks to the mail
    // servers or changes mail on its own. Leaving also schedules the next background check.
    backgroundObserver = NotificationCenter.default.addObserver(
      forName: UIApplication.didEnterBackgroundNotification, object: nil, queue: .main) { _ in
      mailcheck_set_present(0)
      MailCheckPlugin.submit()
    }
    activeObserver = NotificationCenter.default.addObserver(
      forName: UIApplication.didBecomeActiveNotification, object: nil, queue: .main) { _ in
      mailcheck_set_present(1)
    }
    publishInitialPresence()
    let monitor = NWPathMonitor()
    monitor.pathUpdateHandler = { path in mailcheck_set_path(path.isConstrained ? 1 : 0, path.isExpensive ? 1 : 0) }
    monitor.start(queue: DispatchQueue(label: "ai.mirastacklabs.miramail.path", qos: .utility))
    pathMonitor = monitor
  }

  /// The state at launch: `.background` (iOS started the process for a refresh, or prewarmed it) = not present; a normal
  /// launch is `.inactive` until didBecomeActive = present. `applicationState` is main-thread only: off the main thread
  /// the read is queued there (never `sync`, which could deadlock) and Rust keeps its default ("not present") meanwhile.
  static func publishInitialPresence() {
    let apply = {
      let bg = UIApplication.shared.applicationState == .background
      log.notice("mailcheck: launch background=\(bg)")
      mailcheck_set_present(bg ? 0 : 1)
    }
    if Thread.isMainThread { apply() } else { DispatchQueue.main.async(execute: apply) }
  }

  override func load(webview: WKWebView) {
    TapDelegate.webview = webview
    MailCheckPlugin.bootstrap()  // no-op when initPlugin already did it
  }

  static func submit() {
    guard registered else { return }
    let req = BGAppRefreshTaskRequest(identifier: taskId)
    req.earliestBeginDate = Date(timeIntervalSinceNow: 15 * 60)
    do {
      try BGTaskScheduler.shared.submit(req)
    } catch {
      log.error("mailcheck: submit failed \(error.localizedDescription)")
    }
  }

  static func handle(_ task: BGAppRefreshTask) {
    log.notice("mailcheck: task fired")
    submit()  // keep the chain going
    mailcheck_task_begin()  // before the expiration handler exists: an old cancel must not stop this task
    let completion = TaskCompletion(task)
    task.expirationHandler = {
      log.error("mailcheck: expired")
      // Non-blocking: the Rust check stops at its next step, leaves the database consistent and completes the task
      // itself. If it has not within 2 s, report the task as not successful anyway (exactly once either way).
      mailcheck_cancel()
      DispatchQueue.global(qos: .utility).asyncAfter(deadline: .now() + 2) { completion.complete(false) }
    }
    DispatchQueue.global(qos: .utility).async {
      var buf = [UInt8](repeating: 0, count: 256)
      let code = buf.withUnsafeMutableBufferPointer { mailcheck_run_check($0.baseAddress, $0.count) }
      log.notice("mailcheck: check code=\(code) \(String(cString: buf))")  // counts only
      completion.complete(code == 0)
    }
  }

  @objc public func setBadge(_ invoke: Invoke) {
    guard let args = try? invoke.parseArgs(BadgeArgs.self) else { invoke.resolve(); return }
    let n = max(0, args.count)
    if #available(iOS 16.0, *) {
      UNUserNotificationCenter.current().setBadgeCount(n) { _ in }
    } else {
      DispatchQueue.main.async { UIApplication.shared.applicationIconBadgeNumber = n }
    }
    invoke.resolve()
  }

  /// Read-only: may notifications be shown? Never prompts (the prompt stays behind the user's tap in Settings).
  @objc public func notifAllowed(_ invoke: Invoke) {
    UNUserNotificationCenter.current().getNotificationSettings { s in
      let ok = s.authorizationStatus == .authorized || s.authorizationStatus == .provisional
      invoke.resolve(AllowedReply(allowed: ok))
    }
  }

  @objc public func takeTap(_ invoke: Invoke) {
    let t = TapDelegate.takePending()
    invoke.resolve(TapReply(tap: t))
  }
}

@_cdecl("init_plugin_mailcheck")
public func initPlugin() -> Plugin {
  MailCheckPlugin.bootstrap()
  return MailCheckPlugin()
}
