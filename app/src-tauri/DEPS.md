# app/src-tauri dependencies

| Crate | Why |
|---|---|
| tauri 2 (+ tauri-build) | App shell, IPC commands/events, system WebView |
| mailcore, autodetect (path) | The two finished crates; all mail logic lives there |
| serde | camelCase wire wrapper types |
| tokio (sync, time, net) | Per-account async mutex, watcher channel (same runtime Tauri uses); `net` only for the DNS lookup before the one-click unsubscribe POST |
| reqwest 0.12 (rustls-tls, no default features) | Only the user-confirmed RFC 8058 one-click unsubscribe POST (`organise.rs`): https only, redirects off, 10 s timeout, proxies off, DNS answer vetted and pinned. Already in the workspace via autodetect / aichat / calsync, so no new code is compiled. |
| calendar, calsync (path) | Calendar storage/iTIP and CalDAV/Zoom network layer (workspace crates) |
| aichat (path), futures-util | AI chat engine (workspace crate; its HTTP/SSE layer already ships in the tree via reqwest). futures-util: `StreamExt::next` for the connection-test stream (already a dependency of aichat) |
| chrono (std, clock) | Reminder times and Zoom meeting durations (already in the tree via `calendar`; `clock` only adds local-zone lookup for floating times) |
| tauri features tray-icon, image-png | Menu-bar item (tray) and its glyph. (macos-private-api was removed: the mini widget is opaque so the app is Mac App Store eligible) |
| objc2, block2 (macOS) | Typed Objective-C calls and completion-handler blocks for `native.rs`. Already in the tree via tauri / tao / wry |
| objc2-foundation (NSURL, NSString, NSBundle, NSError, NSObject) | Strings/URLs for the wrappers below. Already in the tree |
| objc2-user-notifications (macOS) | New-mail notifications via UNUserNotificationCenter (public API). Replaces tauri-plugin-notification, whose mac-notification-sys dependency swizzled NSBundle, used private KVC keys and embedded NSAppleScript. Already in Cargo.lock (no new download) |
| objc2-service-management (macOS) | Launch at login via SMAppService.mainApp (works sandboxed). Replaces tauri-plugin-autostart (LaunchAgent plist / osascript). Already in the tree |
| objc2-app-kit (macOS; features NSWindow, NSResponder, NSWorkspace) | NSWindow.setCollectionBehavior (widget stays on the desktop) and NSWorkspace.openURL (open web/mail links and staged attachments; replaces tauri-plugin-opener, which spawned /usr/bin/open). Already in the tree via tao |
| base64 | `get_attachment` payload (same version mailcore already uses) |

dev: `tempfile` (temp-dir tests), `serde_json` (wire-shape assertions).

Capabilities: `widget.json` (window `widget` only): core:event:allow-listen/unlisten, core:window:allow-start-dragging; app commands from that window are further limited to get_inbox_summary, focus_main, widget_menu by `widget::guard`.
Capabilities (main/pane windows): `core:default`, `core:window:allow-start-dragging` (overlay title bar drag region). Links go through the app's own `open_link` command, which allows only https, http and mailto.
No network destinations beyond the user's mail servers + DNS (CSP `connect-src` is `ipc:` only; `devCsp` adds the local vite server).

## Frontend (npm) addition: mermaid 12.2.0 (exact pin, published 2025-10-13)
Why: MIRA replies may contain ```mermaid blocks; the owner wants them drawn as diagrams. Writing a layout engine ourselves is not realistic.
Safety: imported with `import()` only when a finished mermaid block is shown (own lazy chunks; the main bundle is unaffected). Initialised with securityLevel strict, htmlLabels off, startOnLoad off; the SVG is shown only inside `<img src="data:...">`, never as HTML. Source capped at 20 KB, 5 s timeout, config directives/click lines stripped. No network: CSP `connect-src` is `ipc:` only and mermaid's code path makes no fetches (diagram icon packs are not registered). `elkjs` (EPL-2.0) is aliased to a stub in `vite.config.ts` so it is not shipped.
Licences of everything bundled: MIT, ISC, Apache-2.0, BSD-3-Clause; dompurify is `MPL-2.0 OR Apache-2.0` (Apache-2.0 chosen). See `THIRD_PARTY_NOTICES.md`.
