# mailcore dependencies

| Crate | Why |
|---|---|
| tokio (rt, net, time, sync, macros, io-util) | Async runtime, TCP, timers, channels for IDLE/watch |
| tokio-rustls (ring, tls12) | TLS for IMAP (implicit + STARTTLS); `ring` avoids the heavy aws-lc C build |
| webpki-roots | Bundled Mozilla CA roots; no OS trust-store/Keychain access, no network |
| async-imap (runtime-tokio) | IMAP protocol client/parsing (imap-proto); avoids hand-written literal/response parsing |
| futures | `TryStreamExt::try_collect` for async-imap streams |
| lettre (builder, smtp-transport, tokio1, tokio1-rustls, ring, webpki-roots) | SMTP submission and RFC 5322 message building |
| mail-parser | MIME parsing: charsets, encoded words, multipart, attachments |
| ammonia | HTML sanitiser for untrusted mail |
| rusqlite (bundled) | SQLite cache; bundled so no system libsqlite dependency |
| keyring | macOS Keychain access (service "miramail"); passwords never touch SQLite |
| serde, serde_json | Serialisable types for the Tauri boundary; account config stored as JSON |
| thiserror | Error enum |
| base64 | Encode inlined `cid:` images as `data:` URIs |
| tempfile (dev) | On-disk SQLite in migration tests |
