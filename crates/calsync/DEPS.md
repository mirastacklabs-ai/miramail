# calsync dependencies

| Crate | Why |
|---|---|
| calendar (path) | Store, `CalEvent`, ICS build/parse. |
| reqwest 0.12 (`default-features=false`, `rustls-tls`) | Same version/features as `autodetect`, so no new TLS stack. No `json`/`cookies`/`gzip`/system-proxy features. Redirects are disabled (`Policy::none()`) and handled manually. |
| tokio (`rt`, `macros`, `net`, `time`, `io-util`, `sync`) | Zoom loopback listener, timeouts, async mutex. A subset of what `autodetect` enables. `rt-multi-thread` only as dev feature. |
| quick-xml 0.37 | Same version as `autodetect`. Namespace-aware `NsReader`; DOCTYPE rejected so entities are never declared/expanded. |
| serde (derive), serde_json | camelCase wire structs (`ZoomStatus`, `DiscoveredCalendar`, `SyncSummary`); Zoom JSON; pending-change lists in settings. Already in the workspace. |
| thiserror | Error enums with user-facing messages. Already in the workspace. |
| chrono (`std`, `alloc`) | RFC 3339 -> UTC for Zoom `start_time`. Same features as `calendar`. |
| sha2 0.10 | PKCE S256. Already in the lockfile via reqwest/rustls. |
| base64 0.22 | base64url (no padding) for PKCE/state. Already in the lockfile via reqwest. |
| getrandom 0.3 | OS RNG for state + PKCE verifier (`calendar::new_id` is not secret-grade). Already in the lockfile. |

No new crates beyond what the workspace already compiles. Not used: `url` directly (use `reqwest::Url`), `rand`, `oauth2`, `hyper`/`axum`
(the callback server is ~60 lines of tokio TCP and serves one request), `psl` (registrable-domain check is a documented heuristic).
Network destinations: the user's configured CalDAV host (and same-registrable-domain hosts it names) and, after the user connects Zoom,
`zoom.us` / `api.zoom.us`. Nothing else.
