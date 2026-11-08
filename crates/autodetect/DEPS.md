# autodetect dependencies

| Crate | Why |
|---|---|
| tokio (rt, rt-multi-thread, macros, net, time, io-util, io-std) | Async runtime, TCP, timeouts (2s discovery / 5s verify budgets), JoinSet for parallel sources |
| reqwest (default-features=false, rustls-tls) | HTTPS-only autoconfig fetch (https_only, https-only redirects); nothing else uses it |
| hickory-resolver | MX / SRV / A lookups (pure-Rust, no libresolv) |
| tokio-rustls (ring) | TLS (implicit + STARTTLS) for IMAP/SMTP verification; exposes cert errors distinctly |
| rustls (ring, std, tls12) | Client config + `rustls::Error` / `CertificateError` for classifying invalid certs; `ring` matches reqwest's provider, so one crypto backend |
| webpki-roots | Bundled Mozilla roots: deterministic, never reads the macOS Keychain (private/corporate CAs are not trusted) |
| serde (derive) | Serialize/Deserialize on all public types (Tauri IPC) and provider TOML |
| toml (parse only) | Parses the bundled data/providers.toml |
| quick-xml | Thunderbird autoconfig XML (does not expand DTD entities) |
| thiserror | Error derives |
| rpassword | No-echo password prompt in the CLI (`--verify`) only |

dev-dependencies: `toml` with `display` (one error-shape test).

Not used on purpose: async-imap / lettre (see comment in `src/verify.rs`), futures (tiny `join_all` inlined), a base64 crate (encode-only helper inlined), serde_json.
