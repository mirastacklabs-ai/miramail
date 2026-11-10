# MiraMail

A free, ad-free mail client for macOS that works with any IMAP mailbox, with MIRA, an optional AI assistant that uses your own AI provider and only the folders you allow.

**Email → password → inbox.** No server settings. No account with us. No telemetry.

Built for IMAP providers the big clients treat as second-class: Zoho, Fastmail, iCloud, Migadu, Purelymail, web hosts, custom domains. Gmail works with an app password. Microsoft accounts (Outlook, Hotmail, Microsoft 365) are not supported yet.

## Download
Get `MiraMail-<version>.dmg` from [Releases](https://github.com/mirastacklabs-ai/miramail/releases), open it and drag MiraMail into Applications. It is signed with Developer ID and notarized by Apple. Requires a Mac with Apple Silicon and macOS 12 or later. The download does not update itself; check Releases for new versions.

## MIRA (optional)
Ask about your mail, get summaries, tidy up folders, draft replies, turn sentences into rules and plan your calendar. MIRA only sees the folders you allow in each conversation, every change waits for your Approve, and it never sends mail or deletes anything for good. Bring your own provider (OpenAI, Anthropic, OpenRouter or any OpenAI-compatible service) or run a model on your Mac with Ollama or LM Studio. No AI endpoint is contacted until you set one up.

## Privacy
No telemetry, no ads, no account with us. MiraMail talks only to your own mail servers (plus DNS and your mail domain's autoconfig), and to services you connect yourself (AI provider, CalDAV, Zoom). Passwords are kept in the macOS Keychain. Remote images are blocked until you allow them. Policy: https://mirastacklabs.ai/miramail-privacy/

Automation rules and the template auto-reply use no AI model and run only while MiraMail is open.

## Build from source
`cd app && npm install && npm run tauri -- build --bundles app`, then copy `target/release/bundle/macos/MiraMail.app` to /Applications. Tests: `cargo test --workspace` and `cd app && npm run check`.

## Licence
Proprietary freeware, all rights reserved (see `LICENSE`). The source is public so you can see what the app does; that grants no rights beyond `LICENSE`. MiraMail is not open source. Third-party notices: `THIRD_PARTY_NOTICES.md`.

Support: hello@mirastacklabs.ai

## Credits
© 2026 MIRASTACK LABS PRIVATE LIMITED. MiraMail's colours, typography (Oswald, JetBrains Mono, both SIL OFL 1.1) and visual identity are taken from MIRASTACK LABS (https://www.mirastacklabs.ai). The app icon is an original MIRASTACK LABS design (source: `app/src-tauri/icons/source/miramail-icon-v2.svg`).
