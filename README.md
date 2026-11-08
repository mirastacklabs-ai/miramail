# MiraMail

A lightweight, ad-free, free-to-use mail client for macOS.

**Email → password → inbox.** No server settings. No account with us. No telemetry.

Built for IMAP providers the big clients treat as second-class: Zoho, Fastmail, iCloud, Migadu, Purelymail, web hosts, custom domains.

Status: v1.0.0. See `docs/ROADMAP.md`.

Build: `cd app && npm install && npm run tauri -- build --bundles app`, then copy `target/release/bundle/macos/MiraMail.app` to /Applications.

Automation: optional local rules (move, archive, trash, mark read, flag) and an opt-in template auto-reply live in Preferences > AI. They use no AI model and run **only while MiraMail is open**; for time off, also set up your mail provider's own vacation responder or server-side filters (see `docs/AI_API.md`, P4).

Known gaps: UI not visually verified (no screenshot access); IMAP/SMTP/IDLE/Keychain paths only tested offline; GMX has no "enable IMAP" hint; provider list is hand-seeded, not a Mozilla ISPDB import; unread counts only for loaded folders; unsigned (ad-hoc) build.

License: proprietary freeware, all rights reserved (see `LICENSE`). Third-party notices: `THIRD_PARTY_NOTICES.md`.

## Docs index
- `docs/AI_POLICY.md`: network and AI policy (owner-approved exceptions to "no network calls")
- `docs/KNOWN_ISSUES.md`: known issues
- `docs/MAS_BUILD.md`: Mac App Store build
- `docs/appstore/SUBMISSION_GUIDE.md`: App Store submission steps

## Credits
© 2026 MIRASTACK LABS PRIVATE LIMITED. MiraMail's colours, typography (Oswald, JetBrains Mono, both SIL OFL 1.1) and visual identity are taken from MIRASTACK LABS (https://www.mirastacklabs.ai). The app icon is an original MIRASTACK LABS design (source: `app/src-tauri/icons/source/miramail-icon-v2.svg`).
