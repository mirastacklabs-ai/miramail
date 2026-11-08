//! SQLite store (`calendar.db`). Holds no secrets: passwords and OAuth tokens live in the Keychain.

use crate::error::{Error, Result};
use crate::ics::{parse_inner, strip_prop, try_build_ics, try_build_ics_series, ParseOptions, TRUSTED_MAX_BYTES};
use crate::patch::patch_resource;
use crate::rrule::{expand_event_budget, override_occurrences, DEFAULT_WORK};
use crate::tm;
use crate::types::*;
use rusqlite::{params, Connection, OptionalExtension};
use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher, Hasher};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

pub const DEFAULT_CALENDAR_ID: &str = "personal";
/// Current `PRAGMA user_version` of `calendar.db`.
pub const SCHEMA_VERSION: i64 = 2;
/// Upper bound of occurrences returned by one [`Calendar::list_occurrences`] call (the result is
/// truncated, not an error).
pub const MAX_LIST_OCCURRENCES: usize = 50_000;
const DEFAULT_COLOR: &str = "#4f7cff";

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS calendars(
  id TEXT PRIMARY KEY, name TEXT NOT NULL, color TEXT NOT NULL, kind TEXT NOT NULL,
  url TEXT, username TEXT, visible INTEGER NOT NULL DEFAULT 1, read_only INTEGER NOT NULL DEFAULT 0,
  last_sync INTEGER, error TEXT, ctag TEXT, sync_marker TEXT, position INTEGER NOT NULL DEFAULT 0);
CREATE TABLE IF NOT EXISTS events(
  uid TEXT NOT NULL, calendar_id TEXT NOT NULL REFERENCES calendars(id) ON DELETE CASCADE,
  ics TEXT NOT NULL, summary TEXT NOT NULL, start_text TEXT NOT NULL, end_text TEXT NOT NULL,
  start_utc INTEGER NOT NULL, end_utc INTEGER NOT NULL, rrule TEXT, href TEXT, etag TEXT,
  PRIMARY KEY(uid, calendar_id));
CREATE INDEX IF NOT EXISTS events_range ON events(calendar_id, start_utc, end_utc);
CREATE INDEX IF NOT EXISTS events_href ON events(calendar_id, href);
CREATE TABLE IF NOT EXISTS overrides(
  uid TEXT NOT NULL, calendar_id TEXT NOT NULL REFERENCES calendars(id) ON DELETE CASCADE,
  recurrence_id TEXT NOT NULL, ics TEXT NOT NULL,
  PRIMARY KEY(uid, calendar_id, recurrence_id));
CREATE TABLE IF NOT EXISTS settings(key TEXT PRIMARY KEY, value TEXT NOT NULL);
";

/// Parameters for [`Calendar::create_calendar`]. `id: None` generates one.
#[derive(Debug, Clone)]
pub struct NewCalendar {
    pub id: Option<String>,
    pub name: String,
    pub color: String,
    pub kind: CalendarKind,
    pub url: Option<String>,
    pub username: Option<String>,
    pub read_only: bool,
}

/// Partial update for [`Calendar::update_calendar`] (`None` = unchanged).
#[derive(Debug, Clone, Default)]
pub struct CalendarPatch {
    pub name: Option<String>,
    pub color: Option<String>,
    pub visible: Option<bool>,
    pub url: Option<String>,
    pub username: Option<String>,
    pub read_only: Option<bool>,
}

/// Non-secret CalDAV sync bookkeeping for one calendar.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SyncState {
    pub ctag: Option<String>,
    pub sync_marker: Option<String>,
    pub error: Option<String>,
    pub last_sync: Option<i64>,
}

/// href/etag index row used by CalDAV sync.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventRef {
    pub uid: String,
    pub href: Option<String>,
    pub etag: Option<String>,
}

pub struct Calendar {
    conn: Connection,
}

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// Random-looking unique id (hex), no extra crates: process-random SipHash seed + time + counter.
pub fn new_id() -> String {
    let mut h = RandomState::new().build_hasher();
    let t = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    h.write_u64(t);
    h.write_u64(COUNTER.fetch_add(1, Ordering::Relaxed));
    let a = h.finish();
    let mut h2 = RandomState::new().build_hasher();
    h2.write_u64(a);
    format!("{a:016x}{:016x}", h2.finish())
}

/// New globally-unique event UID, e.g. `3f2a...@miramail`.
pub fn new_event_uid() -> String {
    format!("{}@miramail", new_id())
}

fn kind_str(k: CalendarKind) -> &'static str {
    match k {
        CalendarKind::Local => "local",
        CalendarKind::Caldav => "caldav",
    }
}

fn row_to_info(r: &rusqlite::Row<'_>) -> rusqlite::Result<CalendarInfo> {
    let kind: String = r.get(3)?;
    Ok(CalendarInfo {
        id: r.get(0)?,
        name: r.get(1)?,
        color: r.get(2)?,
        kind: if kind == "caldav" { CalendarKind::Caldav } else { CalendarKind::Local },
        url: r.get(4)?,
        username: r.get(5)?,
        visible: r.get::<_, i64>(6)? != 0,
        read_only: r.get::<_, i64>(7)? != 0,
        last_sync: r.get(8)?,
        error: r.get(9)?,
    })
}

const INFO_COLS: &str = "id,name,color,kind,url,username,visible,read_only,last_sync,error";

fn valid_color(c: &str) -> bool {
    c.len() == 7 && c.starts_with('#') && c[1..].bytes().all(|b| b.is_ascii_hexdigit())
}

fn ts_of(s: &str) -> Result<i64> {
    tm::wire_ts(s).ok_or_else(|| Error::Invalid("bad date/time".into()))
}

/// Rows this crate wrote itself: parsed without the inbound size cap and with our own zoom props.
fn trusted() -> ParseOptions {
    ParseOptions { trust_zoom_props: true }
}

fn has_column(conn: &Connection, table: &str, col: &str) -> Result<bool> {
    let mut st = conn.prepare(&format!("PRAGMA table_info(\"{table}\")"))?;
    let names: Vec<String> = st.query_map([], |r| r.get(1))?.collect::<std::result::Result<_, _>>()?;
    Ok(names.iter().any(|n| n == col))
}

/// Versioned schema migrations (`PRAGMA user_version`).
///  v1: original tables. v2: `events.raw_ics` / `raw_trust` (original server bytes), and removal of
///  any stored Zoom host start URL.
fn migrate(conn: &Connection) -> Result<()> {
    let v: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    if v > SCHEMA_VERSION {
        return Err(Error::Invalid("calendar.db was written by a newer MiraMail".into()));
    }
    conn.execute_batch(SCHEMA)?;
    if v < 2 {
        let tx = conn.unchecked_transaction()?;
        if !has_column(conn, "events", "raw_ics")? {
            tx.execute_batch("ALTER TABLE events ADD COLUMN raw_ics TEXT; ALTER TABLE events ADD COLUMN raw_trust INTEGER NOT NULL DEFAULT 0;")?;
        }
        for (table, key) in [("events", "uid, calendar_id"), ("overrides", "uid, calendar_id, recurrence_id")] {
            let rows: Vec<Vec<String>> = {
                let mut st = tx.prepare(&format!("SELECT {key}, ics FROM {table} WHERE ics LIKE '%X-MIRAMAIL-ZOOM-STARTURL%'"))?;
                let n = key.split(',').count();
                let out = st
                    .query_map([], |r| (0..=n).map(|i| r.get::<_, String>(i)).collect::<std::result::Result<Vec<_>, _>>())?
                    .collect::<std::result::Result<_, _>>()?;
                out
            };
            for mut row in rows {
                let ics = row.pop().unwrap_or_default();
                let clean = strip_prop(&ics, "X-MIRAMAIL-ZOOM-STARTURL");
                let sql = if table == "events" { "UPDATE events SET ics=?1 WHERE uid=?2 AND calendar_id=?3" } else { "UPDATE overrides SET ics=?1 WHERE uid=?2 AND calendar_id=?3 AND recurrence_id=?4" };
                let mut args: Vec<&dyn rusqlite::ToSql> = vec![&clean];
                args.extend(row.iter().map(|x| x as &dyn rusqlite::ToSql));
                tx.execute(sql, args.as_slice())?;
            }
        }
        tx.commit()?;
        conn.pragma_update(None, "user_version", SCHEMA_VERSION)?;
    }
    Ok(())
}

impl Calendar {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        Self::init(Connection::open(path)?)
    }

    pub fn open_in_memory() -> Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> Result<Self> {
        conn.pragma_update(None, "foreign_keys", true)?;
        migrate(&conn)?;
        conn.execute(
            "INSERT OR IGNORE INTO calendars(id,name,color,kind) VALUES(?1,'Personal',?2,'local')",
            params![DEFAULT_CALENDAR_ID, DEFAULT_COLOR],
        )?;
        Ok(Self { conn })
    }

    // ---- calendars -------------------------------------------------------------------------

    pub fn list_calendars(&self) -> Result<Vec<CalendarInfo>> {
        let mut st = self.conn.prepare(&format!("SELECT {INFO_COLS} FROM calendars ORDER BY position, rowid"))?;
        let rows = st.query_map([], row_to_info)?.collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn get_calendar(&self, id: &str) -> Result<Option<CalendarInfo>> {
        Ok(self
            .conn
            .query_row(&format!("SELECT {INFO_COLS} FROM calendars WHERE id=?1"), [id], row_to_info)
            .optional()?)
    }

    pub fn create_calendar(&self, n: &NewCalendar) -> Result<CalendarInfo> {
        if n.name.trim().is_empty() {
            return Err(Error::Invalid("calendar name is empty".into()));
        }
        if !valid_color(&n.color) {
            return Err(Error::Invalid("color must be #rrggbb".into()));
        }
        let id = n.id.clone().unwrap_or_else(new_id);
        self.conn.execute(
            "INSERT INTO calendars(id,name,color,kind,url,username,read_only,position)
             VALUES(?1,?2,?3,?4,?5,?6,?7,(SELECT COALESCE(MAX(position),0)+1 FROM calendars))",
            params![id, n.name.trim(), n.color, kind_str(n.kind), n.url, n.username, n.read_only as i64],
        )?;
        self.get_calendar(&id)?.ok_or_else(|| Error::NotFound("calendar".into()))
    }

    pub fn create_local(&self, name: &str, color: &str) -> Result<CalendarInfo> {
        self.create_calendar(&NewCalendar {
            id: None,
            name: name.into(),
            color: color.into(),
            kind: CalendarKind::Local,
            url: None,
            username: None,
            read_only: false,
        })
    }

    pub fn update_calendar(&self, id: &str, p: &CalendarPatch) -> Result<()> {
        if let Some(c) = &p.color {
            if !valid_color(c) {
                return Err(Error::Invalid("color must be #rrggbb".into()));
            }
        }
        if p.name.as_deref().is_some_and(|n| n.trim().is_empty()) {
            return Err(Error::Invalid("calendar name is empty".into()));
        }
        let n = self.conn.execute(
            "UPDATE calendars SET name=COALESCE(?2,name), color=COALESCE(?3,color), visible=COALESCE(?4,visible),
               url=COALESCE(?5,url), username=COALESCE(?6,username), read_only=COALESCE(?7,read_only) WHERE id=?1",
            params![
                id,
                p.name.as_deref().map(str::trim),
                p.color,
                p.visible.map(|v| v as i64),
                p.url,
                p.username,
                p.read_only.map(|v| v as i64)
            ],
        )?;
        if n == 0 {
            return Err(Error::NotFound("calendar".into()));
        }
        Ok(())
    }

    /// Deletes a calendar and all its events. The default "Personal" calendar cannot be deleted.
    pub fn delete_calendar(&self, id: &str) -> Result<()> {
        if id == DEFAULT_CALENDAR_ID {
            return Err(Error::Invalid("the default calendar cannot be deleted".into()));
        }
        let tx = self.conn.unchecked_transaction()?;
        tx.execute("DELETE FROM events WHERE calendar_id=?1", [id])?;
        tx.execute("DELETE FROM overrides WHERE calendar_id=?1", [id])?;
        let n = tx.execute("DELETE FROM calendars WHERE id=?1", [id])?;
        tx.commit()?;
        if n == 0 {
            return Err(Error::NotFound("calendar".into()));
        }
        Ok(())
    }

    /// Replace the non-secret sync bookkeeping of a calendar (None clears the field).
    pub fn set_calendar_sync_state(&self, id: &str, ctag: Option<&str>, sync_marker: Option<&str>, error: Option<&str>, last_sync: Option<i64>) -> Result<()> {
        let n = self.conn.execute(
            "UPDATE calendars SET ctag=?2, sync_marker=?3, error=?4, last_sync=?5 WHERE id=?1",
            params![id, ctag, sync_marker, error, last_sync],
        )?;
        if n == 0 {
            return Err(Error::NotFound("calendar".into()));
        }
        Ok(())
    }

    pub fn get_calendar_sync_state(&self, id: &str) -> Result<SyncState> {
        self.conn
            .query_row("SELECT ctag,sync_marker,error,last_sync FROM calendars WHERE id=?1", [id], |r| {
                Ok(SyncState { ctag: r.get(0)?, sync_marker: r.get(1)?, error: r.get(2)?, last_sync: r.get(3)? })
            })
            .optional()?
            .ok_or_else(|| Error::NotFound("calendar".into()))
    }

    // ---- events ----------------------------------------------------------------------------

    fn check_calendar(&self, id: &str) -> Result<()> {
        let n: i64 = self.conn.query_row("SELECT COUNT(*) FROM calendars WHERE id=?1", [id], |r| r.get(0))?;
        if n == 0 {
            return Err(Error::NotFound("calendar".into()));
        }
        Ok(())
    }

    /// `NotFound` for an unknown calendar, `Invalid` for a read-only one.
    fn check_writable(&self, id: &str) -> Result<()> {
        let ro: Option<i64> = self.conn.query_row("SELECT read_only FROM calendars WHERE id=?1", [id], |r| r.get(0)).optional()?;
        match ro {
            None => Err(Error::NotFound("calendar".into())),
            Some(0) => Ok(()),
            Some(_) => Err(Error::Invalid("calendar is read-only".into())),
        }
    }

    /// Insert or replace an event (master or standalone) keyed by (uid, calendarId).
    /// Refuses read-only calendars; errors (instead of storing a damaged ICS) on an unparsable
    /// start, an over-cap RRULE / attendee / EXDATE list, or a result above [`crate::MAX_ICS_BYTES`].
    /// An existing original server ICS (`raw_ics`) is kept so [`Calendar::resource_ics_for_put`] can patch it.
    pub fn upsert_event(&self, ev: &CalEvent) -> Result<()> {
        self.check_writable(&ev.calendar_id)?;
        self.upsert_event_inner(ev, None)
    }

    /// [`Calendar::upsert_event`] for the sync engine: also writes to read-only calendars.
    pub fn sync_upsert_event(&self, ev: &CalEvent) -> Result<()> {
        self.upsert_event_inner(ev, None)
    }

    fn upsert_event_inner(&self, ev: &CalEvent, raw: Option<(&str, bool)>) -> Result<()> {
        if ev.uid.trim().is_empty() {
            return Err(Error::Invalid("uid is empty".into()));
        }
        self.check_calendar(&ev.calendar_id)?;
        let (s, e) = (ts_of(&ev.start)?, ts_of(&ev.end)?);
        let ics = try_build_ics(ev, None)?;
        self.conn.execute(
            "INSERT INTO events(uid,calendar_id,ics,summary,start_text,end_text,start_utc,end_utc,rrule,href,etag,raw_ics,raw_trust)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)
             ON CONFLICT(uid,calendar_id) DO UPDATE SET ics=excluded.ics, summary=excluded.summary,
               start_text=excluded.start_text, end_text=excluded.end_text, start_utc=excluded.start_utc,
               end_utc=excluded.end_utc, rrule=excluded.rrule, href=excluded.href, etag=excluded.etag,
               raw_trust=CASE WHEN excluded.raw_ics IS NULL THEN raw_trust ELSE excluded.raw_trust END,
               raw_ics=COALESCE(excluded.raw_ics, raw_ics)",
            params![ev.uid, ev.calendar_id, ics, ev.summary, ev.start, ev.end, s, e, ev.rrule, ev.href, ev.etag, raw.map(|r| r.0), raw.map(|r| r.1 as i64).unwrap_or(0)],
        )?;
        Ok(())
    }

    /// Insert or replace a RECURRENCE-ID override (uses `o.event.uid` / `o.event.calendar_id`).
    /// Refuses read-only calendars.
    pub fn upsert_override(&self, o: &EventOverride) -> Result<()> {
        self.check_writable(&o.event.calendar_id)?;
        self.upsert_override_inner(o)
    }

    /// [`Calendar::upsert_override`] for the sync engine: also writes to read-only calendars.
    pub fn sync_upsert_override(&self, o: &EventOverride) -> Result<()> {
        self.upsert_override_inner(o)
    }

    fn upsert_override_inner(&self, o: &EventOverride) -> Result<()> {
        self.check_calendar(&o.event.calendar_id)?;
        ts_of(&o.event.start)?;
        ts_of(&o.recurrence_id)?;
        let ics = try_build_ics_series(None, std::slice::from_ref(o), None)?;
        self.conn.execute(
            "INSERT OR REPLACE INTO overrides(uid,calendar_id,recurrence_id,ics) VALUES(?1,?2,?3,?4)",
            params![o.event.uid, o.event.calendar_id, o.recurrence_id, ics],
        )?;
        Ok(())
    }

    /// Refuses read-only calendars.
    pub fn delete_override(&self, uid: &str, calendar_id: &str, recurrence_id: &str) -> Result<bool> {
        self.check_writable(calendar_id)?;
        self.sync_delete_override(uid, calendar_id, recurrence_id)
    }

    /// [`Calendar::delete_override`] for the sync engine.
    pub fn sync_delete_override(&self, uid: &str, calendar_id: &str, recurrence_id: &str) -> Result<bool> {
        Ok(self.conn.execute(
            "DELETE FROM overrides WHERE uid=?1 AND calendar_id=?2 AND recurrence_id=?3",
            params![uid, calendar_id, recurrence_id],
        )? > 0)
    }

    /// Parse a CalDAV resource (SAFE defaults: inbound `X-MIRAMAIL-ZOOM-*` ignored) and store every
    /// event and override in it. See [`Calendar::upsert_ics_opts`].
    pub fn upsert_ics(&self, calendar_id: &str, data: &[u8], href: Option<&str>, etag: Option<&str>) -> Result<Vec<String>> {
        self.upsert_ics_opts(calendar_id, data, href, etag, ParseOptions::default())
    }

    /// As [`Calendar::upsert_ics`] with explicit [`ParseOptions`] (opt in to Zoom props only for
    /// resources of a CalDAV calendar the user owns). The ORIGINAL bytes are kept (`raw_ics`) next to
    /// the re-built form so a later save can patch them instead of re-generating (and losing data).
    /// Stale overrides of every UID in the resource are replaced. A RECURRENCE-ID component without
    /// a master (single-instance invite) is stored as an override with its recurrence-id preserved.
    /// Works on read-only calendars (this is the sync path). Returns the UIDs written.
    pub fn upsert_ics_opts(&self, calendar_id: &str, data: &[u8], href: Option<&str>, etag: Option<&str>, opts: ParseOptions) -> Result<Vec<String>> {
        self.check_calendar(calendar_id)?;
        let parsed = parse_inner(data, opts, crate::ics::MAX_ICS_BYTES)?;
        let raw_text = String::from_utf8_lossy(data);
        let raw = raw_text.strip_prefix('\u{feff}').unwrap_or(&raw_text).to_string();
        let tx = self.conn.unchecked_transaction()?;
        let mut uids: Vec<String> = Vec::new();
        for u in parsed.events.iter().map(|e| &e.uid).chain(parsed.overrides.iter().map(|o| &o.event.uid)) {
            if !uids.contains(u) {
                uids.push(u.clone());
            }
        }
        for u in &uids {
            tx.execute("DELETE FROM overrides WHERE uid=?1 AND calendar_id=?2", params![u, calendar_id])?;
        }
        for mut ev in parsed.events {
            ev.calendar_id = calendar_id.into();
            ev.href = href.map(String::from);
            ev.etag = etag.map(String::from);
            self.upsert_event_inner(&ev, Some((&raw, opts.trust_zoom_props)))?;
        }
        for mut o in parsed.overrides {
            o.event.calendar_id = calendar_id.into();
            o.event.href = href.map(String::from);
            o.event.etag = etag.map(String::from);
            self.upsert_override_inner(&o)?;
        }
        tx.commit()?;
        Ok(uids)
    }

    /// The ICS to PUT to a CalDAV server for `uid`: the original server bytes with only the edited
    /// properties patched in (SUMMARY, DESCRIPTION, LOCATION, DTSTART/DTEND/DURATION, RRULE, EXDATE,
    /// STATUS, ATTENDEE changes, the VALARM trigger, SEQUENCE bump, X-MIRAMAIL-ZOOM-*), keeping every
    /// other property, parameter and component (VTIMEZONE, other VALARMs, CLASS, TRANSP, CATEGORIES,
    /// URL, ATTACH, RDATE, X- props, PARTSTAT=DELEGATED, the server's overrides) verbatim. Falls back
    /// to a strict re-build of master + stored overrides when there are no original bytes.
    /// Errors (never silently drops data) on over-cap RRULE/attendees/EXDATEs or an unparsable time.
    /// The Zoom host `start_url` is never written.
    pub fn resource_ics_for_put(&self, uid: &str, calendar_id: &str) -> Result<String> {
        self.resource_ics_for_put_opts(uid, calendar_id, false)
    }

    /// [`Calendar::resource_ics_for_put`] with `SCHEDULE-AGENT=CLIENT` on added/rebuilt ATTENDEEs.
    pub fn resource_ics_for_put_opts(&self, uid: &str, calendar_id: &str, schedule_agent_client: bool) -> Result<String> {
        let row: Option<(Option<String>, i64)> = self
            .conn
            .query_row("SELECT raw_ics, raw_trust FROM events WHERE uid=?1 AND calendar_id=?2", params![uid, calendar_id], |r| Ok((r.get(0)?, r.get(1)?)))
            .optional()?;
        let master = self.get_event(uid, calendar_id)?;
        let overrides = self.get_overrides(uid, calendar_id)?;
        if master.is_none() && overrides.is_empty() {
            return Err(Error::NotFound("event".into()));
        }
        if let (Some((Some(raw), trust)), Some(_)) = (&row, &master) {
            if let Some(s) = patch_resource(raw, *trust != 0, uid, master.as_ref(), &overrides, schedule_agent_client)? {
                return Ok(s);
            }
        }
        crate::ics::try_build_ics_series_opts(master.as_ref(), &overrides, None, schedule_agent_client)
    }

    fn load_event(&self, ics: &str, calendar_id: &str, href: Option<String>, etag: Option<String>) -> Option<CalEvent> {
        let mut ev = parse_inner(ics.as_bytes(), trusted(), TRUSTED_MAX_BYTES).ok()?.events.into_iter().next()?;
        ev.calendar_id = calendar_id.into();
        ev.href = href;
        ev.etag = etag;
        Some(ev)
    }

    pub fn get_event(&self, uid: &str, calendar_id: &str) -> Result<Option<CalEvent>> {
        let row = self
            .conn
            .query_row("SELECT ics,href,etag FROM events WHERE uid=?1 AND calendar_id=?2", params![uid, calendar_id], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?, r.get::<_, Option<String>>(2)?))
            })
            .optional()?;
        Ok(row.and_then(|(ics, h, e)| self.load_event(&ics, calendar_id, h, e)))
    }

    /// First event with this UID in any calendar (for "already in calendar" checks on invites).
    pub fn find_event(&self, uid: &str) -> Result<Option<CalEvent>> {
        let cid: Option<String> = self
            .conn
            .query_row("SELECT calendar_id FROM events WHERE uid=?1 ORDER BY rowid LIMIT 1", [uid], |r| r.get(0))
            .optional()?;
        match cid {
            Some(c) => self.get_event(uid, &c),
            None => Ok(None),
        }
    }

    fn parse_override_row(rid: String, ics: &str, calendar_id: &str) -> Option<EventOverride> {
        let mut ev = parse_inner(ics.as_bytes(), trusted(), TRUSTED_MAX_BYTES).ok()?.overrides.into_iter().next()?.event;
        ev.calendar_id = calendar_id.into();
        Some(EventOverride { recurrence_id: rid, event: ev })
    }

    pub fn get_overrides(&self, uid: &str, calendar_id: &str) -> Result<Vec<EventOverride>> {
        let mut st = self.conn.prepare_cached("SELECT recurrence_id,ics FROM overrides WHERE uid=?1 AND calendar_id=?2 ORDER BY recurrence_id")?;
        let rows = st
            .query_map(params![uid, calendar_id], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows.into_iter().filter_map(|(rid, ics)| Self::parse_override_row(rid, &ics, calendar_id)).collect())
    }

    /// Copy the original server bytes (`raw_ics`, `raw_trust`) of `uid` from calendar `from` to the already
    /// stored event of the same uid in `to`, so a move between two calendars keeps what `CalEvent` cannot
    /// express. Returns whether anything was copied. Refuses a read-only target.
    pub fn copy_raw_ics(&self, uid: &str, from: &str, to: &str) -> Result<bool> {
        self.check_writable(to)?;
        let n = self.conn.execute(
            "UPDATE events SET raw_ics=(SELECT raw_ics FROM events WHERE uid=?1 AND calendar_id=?2),
                               raw_trust=(SELECT raw_trust FROM events WHERE uid=?1 AND calendar_id=?2)
             WHERE uid=?1 AND calendar_id=?3 AND EXISTS(SELECT 1 FROM events WHERE uid=?1 AND calendar_id=?2 AND raw_ics IS NOT NULL)",
            params![uid, from, to],
        )?;
        Ok(n > 0)
    }

    /// Deletes the event and its overrides. Returns whether an event existed. Refuses read-only calendars.
    pub fn delete_event(&self, uid: &str, calendar_id: &str) -> Result<bool> {
        self.check_writable(calendar_id)?;
        self.sync_delete_event(uid, calendar_id)
    }

    /// [`Calendar::delete_event`] for the sync engine: also deletes on read-only calendars.
    pub fn sync_delete_event(&self, uid: &str, calendar_id: &str) -> Result<bool> {
        let tx = self.conn.unchecked_transaction()?;
        tx.execute("DELETE FROM overrides WHERE uid=?1 AND calendar_id=?2", params![uid, calendar_id])?;
        let n = tx.execute("DELETE FROM events WHERE uid=?1 AND calendar_id=?2", params![uid, calendar_id])?;
        tx.commit()?;
        Ok(n > 0)
    }

    pub fn list_events(&self, calendar_id: &str) -> Result<Vec<CalEvent>> {
        let mut st = self.conn.prepare("SELECT ics,href,etag FROM events WHERE calendar_id=?1 ORDER BY start_utc, uid")?;
        let rows = st
            .query_map([calendar_id], |r| Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?, r.get::<_, Option<String>>(2)?)))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows.into_iter().filter_map(|(ics, h, e)| self.load_event(&ics, calendar_id, h, e)).collect())
    }

    /// uid/href/etag index of a calendar, for CalDAV change detection.
    pub fn list_event_refs(&self, calendar_id: &str) -> Result<Vec<EventRef>> {
        let mut st = self.conn.prepare("SELECT uid,href,etag FROM events WHERE calendar_id=?1 ORDER BY uid")?;
        let rows = st
            .query_map([calendar_id], |r| Ok(EventRef { uid: r.get(0)?, href: r.get(1)?, etag: r.get(2)? }))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Expanded occurrences overlapping `[start, end)` (RFC 3339). Recurrences expanded, overrides
    /// applied (override-only series included). The call shares one work budget and returns at most
    /// [`MAX_LIST_OCCURRENCES`] occurrences (truncated, not an error); summaries/locations are
    /// truncated to 256 chars.
    pub fn list_occurrences(&self, start: &str, end: &str, visible_only: bool) -> Result<Vec<Occurrence>> {
        let from = tm::parse_wire_dt(start).ok_or_else(|| Error::Invalid("start is not RFC 3339".into()))?;
        let to = tm::parse_wire_dt(end).ok_or_else(|| Error::Invalid("end is not RFC 3339".into()))?;
        let (f, t) = (from.timestamp(), to.timestamp());
        let mut st = self.conn.prepare(
            "SELECT e.ics,e.calendar_id,e.href,e.etag,c.color FROM events e JOIN calendars c ON c.id=e.calendar_id
             WHERE (?1=0 OR c.visible=1)
               AND (e.rrule IS NOT NULL OR (e.start_utc < ?3 + 90000 AND e.end_utc >= ?2 - 90000))",
        )?;
        let rows = st
            .query_map(params![visible_only as i64, f, t], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, Option<String>>(2)?, r.get::<_, Option<String>>(3)?, r.get::<_, String>(4)?))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let mut out: Vec<Occurrence> = Vec::new();
        let mut work: u64 = DEFAULT_WORK;
        'events: for (ics, cid, h, e, color) in rows {
            let Some(ev) = self.load_event(&ics, &cid, h, e) else { continue };
            let ovs = if ev.rrule.is_some() { self.get_overrides(&ev.uid, &cid)? } else { vec![] };
            for mut o in expand_event_budget(&ev, &ovs, from, to, &mut work) {
                o.color = color.clone();
                out.push(o);
                if out.len() >= MAX_LIST_OCCURRENCES {
                    break 'events;
                }
            }
        }
        if out.len() < MAX_LIST_OCCURRENCES {
            // override-only series (single-instance invites without a stored master)
            let mut st = self.conn.prepare(
                "SELECT o.uid,o.calendar_id,o.recurrence_id,o.ics,c.color FROM overrides o JOIN calendars c ON c.id=o.calendar_id
                 WHERE (?1=0 OR c.visible=1)
                   AND NOT EXISTS (SELECT 1 FROM events e WHERE e.uid=o.uid AND e.calendar_id=o.calendar_id)",
            )?;
            let rows = st
                .query_map(params![visible_only as i64], |r| {
                    Ok((r.get::<_, String>(1)?, r.get::<_, String>(2)?, r.get::<_, String>(3)?, r.get::<_, String>(4)?))
                })?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            for (cid, rid, ics, color) in rows {
                let Some(o) = Self::parse_override_row(rid, &ics, &cid) else { continue };
                for mut occ in override_occurrences(std::slice::from_ref(&o), &from, &to) {
                    occ.color = color.clone();
                    out.push(occ);
                }
                if out.len() >= MAX_LIST_OCCURRENCES {
                    break;
                }
            }
        }
        out.truncate(MAX_LIST_OCCURRENCES);
        out.sort_by(|a, b| {
            let ka = (tm::wire_ts(&a.start).unwrap_or(0), &a.summary, &a.uid);
            let kb = (tm::wire_ts(&b.start).unwrap_or(0), &b.summary, &b.uid);
            ka.cmp(&kb)
        });
        Ok(out)
    }

    // ---- settings --------------------------------------------------------------------------

    /// Plain, non-secret settings only. The key is normalised (lower-cased, non-alphanumerics removed)
    /// and rejected if it contains token/secret/pass/pwd/key/credential/zak/bearer/auth.
    pub fn set_setting(&self, key: &str, value: &str) -> Result<()> {
        let k: String = key.chars().filter(|c| c.is_alphanumeric()).flat_map(char::to_lowercase).collect();
        if ["token", "secret", "pass", "pwd", "key", "credential", "zak", "bearer", "auth"].iter().any(|w| k.contains(w)) {
            return Err(Error::Invalid("secrets belong in the Keychain, not SQLite".into()));
        }
        self.conn.execute("INSERT INTO settings(key,value) VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value", params![key, value])?;
        Ok(())
    }

    pub fn get_setting(&self, key: &str) -> Result<Option<String>> {
        Ok(self.conn.query_row("SELECT value FROM settings WHERE key=?1", [key], |r| r.get(0)).optional()?)
    }

    pub fn delete_setting(&self, key: &str) -> Result<()> {
        self.conn.execute("DELETE FROM settings WHERE key=?1", [key])?;
        Ok(())
    }

    /// Test/diagnostic access to the schema.
    #[doc(hidden)]
    pub fn column_names(&self) -> Result<Vec<String>> {
        let mut out = Vec::new();
        let tables: Vec<String> = self
            .conn
            .prepare("SELECT name FROM sqlite_master WHERE type='table'")?
            .query_map([], |r| r.get(0))?
            .collect::<std::result::Result<_, _>>()?;
        for t in tables {
            let mut st = self.conn.prepare(&format!("PRAGMA table_info(\"{t}\")"))?;
            let cols: Vec<String> = st.query_map([], |r| r.get(1))?.collect::<std::result::Result<_, _>>()?;
            out.extend(cols.into_iter().map(|c| format!("{t}.{c}")));
        }
        Ok(out)
    }
}
