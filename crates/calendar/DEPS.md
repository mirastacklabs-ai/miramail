# calendar dependencies

| Crate | Why |
|---|---|
| chrono (no default features; `std`, `alloc`) | Date/time arithmetic and RFC 3339 formatting. No `clock` feature: the crate never needs the system zone; DTSTAMP uses `SystemTime`. No `serde` feature: wire times are plain strings. |
| chrono-tz | IANA tz database (also `TZ_VARIANTS` to match an embedded VTIMEZONE against real zones) compiled in, so VTIMEZONE/TZID can be mapped to IANA ids and recurrences are expanded in wall-clock time with correct DST (no OS tz lookups, works offline, deterministic in tests). Also used to derive a VTIMEZONE (offsets + DST rules) when writing ICS. Adds roughly 0.5-1 MB to the binary; revisit `chrono-tz` feature `filter-by-regex` if the 15 MB budget gets tight. |
| rusqlite (bundled) | Own `calendar.db` store; same crate/feature set as `mailcore`, so no additional C build. |
| serde (derive) | camelCase wire types for the Tauri boundary. |
| thiserror | Error enum. |
| serde_json (dev) | Tests assert the exact JSON shape. |
| tempfile (dev) | On-disk SQLite persistence test. |

## Deliberately not used
- `rrule` / `icalendar` / `ical` crates: RRULE expansion (DAILY..YEARLY, BYDAY ordinals, BYSETPOS, WKST, UNTIL/COUNT, 10 000 cap, DST gap/overlap policy) is about 300 lines on top of chrono/chrono-tz, avoids pulling in a second date model, and gives hard caps against hostile rules. The ICS parser is a small tolerant line parser (unfold, params, escapes) that never panics and caps input at 1 MB, depth at 16, events at 5 000.
- `uuid` / `rand`: ids come from `RandomState` (process-random SipHash seed) + time + counter; they only need uniqueness, not secrecy.
- No networking crates. No secrets are stored in SQLite (a test asserts no password/secret/token/credential column; `set_setting` rejects secret-looking keys).

No dependencies were added by the hardening pass (VTIMEZONE evaluation, ICS patching and FNV hashing are in-crate).
