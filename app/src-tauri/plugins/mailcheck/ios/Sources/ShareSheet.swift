import QuickLook
import Tauri
import UIKit
import WebKit

// Share sheet (Save to Files, AirDrop, Mail, ...), Quick Look and print for MiraMail on iPhone. Public API only
// (UIActivityViewController, QLPreviewController, UIPrintInteractionController, WKWebView). The Rust side stages the
// file in `<Caches>/share/<unique>/<name>` inside the app container; this file refuses any other path and deletes the
// staged folder when the sheet is dismissed. Nothing here logs file names, subjects or bodies.

struct SharePathArgs: Decodable {
  let path: String
}

struct PrintHtmlArgs: Decodable {
  let html: String
  let title: String?
}

enum ShareSupport {
  /// `<Library/Caches>/share`, symlinks resolved. The Rust side stages every file as `<this>/<unique dir>/<file>`.
  static func shareRoot() -> URL? {
    guard let caches = FileManager.default.urls(for: .cachesDirectory, in: .userDomainMask).first else { return nil }
    return caches.appendingPathComponent("share", isDirectory: true).resolvingSymlinksInPath()
  }

  /// True only for exactly `<Caches>/share/<dir>/<file>` (one directory level, one file level, no `..`).
  static func isStaged(_ file: URL) -> Bool {
    guard let root = shareRoot() else { return false }
    let f = file.resolvingSymlinksInPath()
    let rc = root.pathComponents
    let fc = f.pathComponents
    guard fc.count == rc.count + 2, Array(fc.prefix(rc.count)) == rc else { return false }
    return !fc.contains("..") && !fc.suffix(2).contains(".") && !fc.suffix(2).contains("")
  }

  /// The staged file for `path`, only if `isStaged` and it is a regular file. Also marks it (and the share folder)
  /// `NSFileProtectionComplete`.
  static func stagedFile(_ path: String) -> URL? {
    let file = URL(fileURLWithPath: path).resolvingSymlinksInPath()
    guard isStaged(file) else { return nil }
    var isDir: ObjCBool = false
    guard FileManager.default.fileExists(atPath: file.path, isDirectory: &isDir), !isDir.boolValue else { return nil }
    let attrs: [FileAttributeKey: Any] = [.protectionKey: FileProtectionType.complete]
    if let root = shareRoot() { try? FileManager.default.setAttributes(attrs, ofItemAtPath: root.path) }
    try? FileManager.default.setAttributes(attrs, ofItemAtPath: file.deletingLastPathComponent().path)
    try? FileManager.default.setAttributes(attrs, ofItemAtPath: file.path)
    return file
  }

  /// Delete the per-share folder of a staged file, after re-checking it is exactly `<Caches>/share/<dir>/<file>`.
  static func cleanup(_ file: URL) {
    guard isStaged(file) else { return }
    try? FileManager.default.removeItem(at: file.resolvingSymlinksInPath().deletingLastPathComponent())
  }

  /// Delete later: the receiving app (Mail, a Files copy, AirDrop) may still be reading the file when the sheet reports
  /// completion. The launch sweep and the one-hour stale sweep on the Rust side are the backstop.
  static let cleanupDelay: TimeInterval = 60
  static func cleanupLater(_ file: URL) {
    DispatchQueue.main.asyncAfter(deadline: .now() + cleanupDelay) { cleanup(file) }
  }

  /// The top-most presented view controller of the foreground window scene's key window. Main thread only.
  static func topViewController() -> UIViewController? {
    let scenes = UIApplication.shared.connectedScenes.compactMap { $0 as? UIWindowScene }
    let scene = scenes.first(where: { $0.activationState == .foregroundActive }) ?? scenes.first
    let window = scene?.windows.first(where: { $0.isKeyWindow }) ?? scene?.windows.first
    var vc = window?.rootViewController
    while let next = vc?.presentedViewController, !next.isBeingDismissed { vc = next }
    return vc
  }

  /// A view controller that can present right now (nothing is mid-presentation or mid-dismissal), or nil.
  static func presenter() -> UIViewController? {
    guard let top = topViewController(), !top.isBeingPresented, !top.isBeingDismissed, !top.isMovingToParent,
          top.presentedViewController == nil else { return nil }
    return top
  }

  /// Answer an `Invoke` exactly once; if nothing answered after `seconds`, reject so the Rust caller never hangs.
  final class Settle {
    private var done = false
    private let invoke: Invoke
    init(_ invoke: Invoke, timeout seconds: TimeInterval = 8, message: String = "That took too long.") {
      self.invoke = invoke
      DispatchQueue.main.asyncAfter(deadline: .now() + seconds) { [weak self] in self?.reject(message) }
    }
    func resolve() { if !done { done = true; invoke.resolve() } }
    func reject(_ message: String) { if !done { done = true; invoke.reject(message) } }
  }

  /// Anchor for iPad popovers: bottom centre of the presenting view.
  static func anchor(_ pop: UIPopoverPresentationController?, in vc: UIViewController) {
    guard let pop = pop else { return }
    pop.sourceView = vc.view
    pop.sourceRect = CGRect(x: vc.view.bounds.midX, y: vc.view.bounds.maxY - 1, width: 1, height: 1)
    pop.permittedArrowDirections = []
  }

  static func onMain(_ work: @escaping () -> Void) {
    if Thread.isMainThread { work() } else { DispatchQueue.main.async(execute: work) }
  }
}

/// Quick Look data source; keeps itself alive through `current` until the preview is dismissed.
final class PreviewSource: NSObject, QLPreviewControllerDataSource, QLPreviewControllerDelegate {
  static var current: PreviewSource?
  let file: URL
  init(_ file: URL) { self.file = file }

  func numberOfPreviewItems(in controller: QLPreviewController) -> Int { 1 }
  func previewController(_ controller: QLPreviewController, previewItemAt index: Int) -> QLPreviewItem { file as NSURL }
  func previewControllerDidDismiss(_ controller: QLPreviewController) {
    ShareSupport.cleanupLater(file)
    if PreviewSource.current === self { PreviewSource.current = nil }
  }
}

/// Renders an HTML document off-screen and prints it. The Swift side enforces what the page's string cannot be trusted
/// to: JavaScript off, every navigation cancelled (links are inert), our own Content-Security-Policy first in the
/// document, and a content rule list that blocks every http(s)/ftp/ws load, so printing never contacts a sender
/// (a tracker pixel would otherwise reveal that the message was printed). Kept alive through `current`.
final class HTMLPrintJob: NSObject, WKNavigationDelegate {
  static var current: HTMLPrintJob?
  static let csp = "<meta http-equiv=\"Content-Security-Policy\" content=\"default-src 'none'; img-src data:; style-src 'unsafe-inline'\">"
  static let rules = "[{\"trigger\":{\"url-filter\":\"^(https?|ftp|wss?)://\"},\"action\":{\"type\":\"block\"}}]"
  private let web: WKWebView
  private let html: String
  private let title: String
  private var started = false
  private var finished = false

  init(html: String, title: String) {
    let cfg = WKWebViewConfiguration()
    cfg.defaultWebpagePreferences.allowsContentJavaScript = false
    cfg.websiteDataStore = .nonPersistent()
    self.web = WKWebView(frame: CGRect(x: 0, y: 0, width: 612, height: 792), configuration: cfg)
    self.html = HTMLPrintJob.csp + html
    self.title = title
    super.init()
    web.navigationDelegate = self
    web.isUserInteractionEnabled = false
    web.alpha = 0.01
  }

  /// Compiles the block-everything-remote rule list, then loads. If it cannot be compiled nothing is loaded or printed.
  func start(in host: UIView, onStarted: @escaping () -> Void, onFailure: @escaping () -> Void) {
    host.insertSubview(web, at: 0)
    WKContentRuleListStore.default().compileContentRuleList(forIdentifier: "mm-print-no-remote", encodedContentRuleList: HTMLPrintJob.rules) { [weak self] list, _ in
      DispatchQueue.main.async {
        guard let self = self, !self.finished else { return }
        guard let list = list else { self.finish(); onFailure(); return }
        self.web.configuration.userContentController.add(list)
        self.web.loadHTMLString(self.html, baseURL: nil)
        onStarted()
      }
    }
    // Never leave a half-started job: if rendering has not finished in 30 s, give up.
    DispatchQueue.main.asyncAfter(deadline: .now() + 30) { [weak self] in
      if let self = self, !self.started { self.finish() }
    }
  }

  func webView(_ webView: WKWebView, decidePolicyFor navigationAction: WKNavigationAction,
               decisionHandler: @escaping (WKNavigationActionPolicy) -> Void) {
    // Only the initial about:blank load of our own string; anything else (links, redirects, meta refresh) is cancelled.
    let scheme = navigationAction.request.url?.scheme ?? "about"
    decisionHandler(!started && scheme == "about" ? .allow : .cancel)
  }

  func webView(_ webView: WKWebView, didFinish navigation: WKNavigation!) {
    guard !started, !finished else { return }
    started = true
    let pc = UIPrintInteractionController.shared
    let info = UIPrintInfo(dictionary: nil)
    info.outputType = .general
    info.jobName = title.isEmpty ? "MiraMail message" : title
    pc.printInfo = info
    pc.printFormatter = web.viewPrintFormatter()
    let done: UIPrintInteractionController.CompletionHandler = { [weak self] _, _, _ in self?.finish() }
    if UIDevice.current.userInterfaceIdiom == .pad, let host = web.superview {
      let r = CGRect(x: host.bounds.midX, y: host.bounds.maxY - 1, width: 1, height: 1)
      pc.present(from: r, in: host, animated: true, completionHandler: done)
    } else {
      pc.present(animated: true, completionHandler: done)
    }
  }

  func webView(_ webView: WKWebView, didFail navigation: WKNavigation!, withError error: Error) { finish() }
  func webView(_ webView: WKWebView, didFailProvisionalNavigation navigation: WKNavigation!, withError error: Error) { finish() }
  func webViewWebContentProcessDidTerminate(_ webView: WKWebView) { finish() }

  func finish() {
    if finished { return }
    finished = true
    web.navigationDelegate = nil
    web.stopLoading()
    web.removeFromSuperview()
    UIPrintInteractionController.shared.printFormatter = nil
    if HTMLPrintJob.current === self { HTMLPrintJob.current = nil }
  }
}

extension MailCheckPlugin {
  /// Share sheet for a staged file. Resolves once the sheet is presented (or rejects: never hangs); the staged folder is
  /// deleted 60 s after the sheet is dismissed.
  @objc public func shareFile(_ invoke: Invoke) {
    guard let args = try? invoke.parseArgs(SharePathArgs.self), let file = ShareSupport.stagedFile(args.path) else {
      invoke.reject("That file can't be shared."); return
    }
    ShareSupport.onMain {
      let settle = ShareSupport.Settle(invoke)
      guard let top = ShareSupport.presenter() else {
        ShareSupport.cleanup(file); settle.reject("Another screen is still closing. Try again in a moment."); return
      }
      let vc = UIActivityViewController(activityItems: [file], applicationActivities: nil)
      vc.completionWithItemsHandler = { _, _, _, _ in ShareSupport.cleanupLater(file) }
      ShareSupport.anchor(vc.popoverPresentationController, in: top)
      top.present(vc, animated: true) { settle.resolve() }
    }
  }

  /// Quick Look inside the app for a staged file.
  @objc public func previewFile(_ invoke: Invoke) {
    guard let args = try? invoke.parseArgs(SharePathArgs.self), let file = ShareSupport.stagedFile(args.path) else {
      invoke.reject("That file can't be previewed."); return
    }
    ShareSupport.onMain {
      let settle = ShareSupport.Settle(invoke)
      guard let top = ShareSupport.presenter() else {
        ShareSupport.cleanup(file); settle.reject("Another screen is still closing. Try again in a moment."); return
      }
      let source = PreviewSource(file)
      PreviewSource.current = source
      let ql = QLPreviewController()
      ql.dataSource = source
      ql.delegate = source
      top.present(ql, animated: true) { settle.resolve() }
    }
  }

  /// Print sheet for a staged PDF or image file.
  @objc public func printFile(_ invoke: Invoke) {
    guard let args = try? invoke.parseArgs(SharePathArgs.self), let file = ShareSupport.stagedFile(args.path),
          UIPrintInteractionController.isPrintingAvailable, UIPrintInteractionController.canPrint(file) else {
      invoke.reject("That file can't be printed."); return
    }
    ShareSupport.onMain {
      guard let top = ShareSupport.presenter() else {
        ShareSupport.cleanup(file); invoke.reject("Another screen is still closing. Try again in a moment."); return
      }
      let pc = UIPrintInteractionController.shared
      let info = UIPrintInfo(dictionary: nil)
      info.outputType = .general
      info.jobName = "MiraMail"
      pc.printInfo = info
      pc.printingItem = file
      let done: UIPrintInteractionController.CompletionHandler = { _, _, _ in ShareSupport.cleanupLater(file) }
      if UIDevice.current.userInterfaceIdiom == .pad {
        let r = CGRect(x: top.view.bounds.midX, y: top.view.bounds.maxY - 1, width: 1, height: 1)
        pc.present(from: r, in: top.view, animated: true, completionHandler: done)
      } else {
        pc.present(animated: true, completionHandler: done)
      }
      invoke.resolve()
    }
  }

  /// Print sheet for an HTML document (the sanitised message). Resolves once rendering has started, rejects when it cannot.
  @objc public func printHtml(_ invoke: Invoke) {
    guard let args = try? invoke.parseArgs(PrintHtmlArgs.self), UIPrintInteractionController.isPrintingAvailable else {
      invoke.reject("Printing isn't available."); return
    }
    ShareSupport.onMain {
      let settle = ShareSupport.Settle(invoke)
      guard let top = ShareSupport.presenter() else {
        settle.reject("Another screen is still closing. Try again in a moment."); return
      }
      HTMLPrintJob.current?.finish()
      let job = HTMLPrintJob(html: args.html, title: args.title ?? "")
      HTMLPrintJob.current = job
      job.start(in: top.view, onStarted: { settle.resolve() }, onFailure: { settle.reject("Couldn't prepare the printout.") })
    }
  }
}
