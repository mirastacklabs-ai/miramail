//! Detach / attach panes as native windows (docs/WORKSPACE_SPEC.md, "Attach / detach").
//!
//! Single owner rule: a pane's live state lives in exactly one webview. The `panes` table (in `workspace.db`) is the
//! handoff: `detach_pane` stores the payload and names the new window as owner; the new window reads it with
//! `get_pane`, keeps it fresh with `save_pane`, and `attach_pane` hands it back to the main window. Every command
//! validates its arguments (ids, labels, coordinates) and checks the CALLING window, so a webview can only touch the
//! pane it owns. Payloads are user content: never logged, never echoed in errors.
//!
//! Events: `pane://detached`, `pane://closed`, `pane://flush` are broadcast (`emit`, every window sees them);
//! `pane://attach`, `pane://close-request` and the menu events `ws://detach-pane` / `ws://attach-pane` /
//! `ws://close-pane` are targeted (`emit_to`) at one window. All other app events (`mail://*`, `cal://*`, `ai://*`)
//! stay broadcast, so every window sees updates; IMAP, SQLite, Keychain and the watchers live once in this process.

use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::Path;
use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, MutexGuard};
use tauri::{AppHandle, Emitter, Manager, State, WebviewUrl, WebviewWindow, WebviewWindowBuilder};

pub const MAIN: &str = "main";
const PREFIX: &str = "pane-";
/// Kinds whose state is self-contained JSON. Mailbox / reader / calendar share app-global state in the main webview.
const DETACHABLE: [&str; 3] = ["compose", "event", "aichat"];
const MAX_PAYLOAD_BYTES: usize = 1024 * 1024 + 4096;
const MAX_DEPTH: usize = 40;
const MAX_PANES: i64 = 32;
pub const MIN_W: f64 = 320.0;
pub const MIN_H: f64 = 240.0;
const MAX_SIZE: f64 = 4096.0;
const COORD_MIN: f64 = -8192.0;
const COORD_MAX: f64 = 16384.0;

// ---------------------------------------------------------------- pure helpers

pub fn valid_pane_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 64 && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}
/// A detached window may only open a NEW pane (fresh id, not in the table, compose / event prefix); main may detach any id.
pub fn detach_allowed(caller_label: &str, id: &str, exists: bool) -> bool {
    if caller_label == MAIN {
        return valid_pane_id(id);
    }
    valid_label(caller_label) && !exists && (id.starts_with("c-") || id.starts_with("e-")) && valid_pane_id(id)
}
/// Window label for a pane: `pane-<id>`.
pub fn label_for(id: &str) -> Result<String, String> {
    if !valid_pane_id(id) {
        return Err("Invalid pane id.".into());
    }
    Ok(format!("{PREFIX}{id}"))
}
/// `main` or `pane-<valid id>`; nothing else (no slashes, colons, spaces, empty ids).
pub fn valid_label(label: &str) -> bool {
    label == MAIN || label.strip_prefix(PREFIX).is_some_and(valid_pane_id)
}
pub fn pane_id_of(label: &str) -> Option<&str> {
    label.strip_prefix(PREFIX).filter(|id| valid_pane_id(id))
}
pub fn is_detachable(kind: &str) -> bool {
    DETACHABLE.contains(&kind)
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Geo {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

fn clamp_num(v: f64, lo: f64, hi: f64) -> Result<f64, String> {
    if !v.is_finite() {
        return Err("Invalid window geometry.".into());
    }
    Ok(v.clamp(lo, hi))
}
/// Validate + clamp a requested window rectangle (logical px). Non-finite values are rejected, the rest clamped.
pub fn clamp_geometry(x: f64, y: f64, w: f64, h: f64) -> Result<Geo, String> {
    Ok(Geo { x: clamp_num(x, COORD_MIN, COORD_MAX)?, y: clamp_num(y, COORD_MIN, COORD_MAX)?, w: clamp_num(w, MIN_W, MAX_SIZE)?, h: clamp_num(h, MIN_H, MAX_SIZE)? })
}
pub fn clamp_point(x: f64, y: f64) -> Result<(f64, f64), String> {
    Ok((clamp_num(x, COORD_MIN, COORD_MAX)?, clamp_num(y, COORD_MIN, COORD_MAX)?))
}

/// Which window is under the point? `wins` is in priority order (front-most first); `exclude` is skipped
/// (the window the drag started in). Edges: left/top inclusive, right/bottom exclusive.
pub fn hit_test(wins: &[(String, Geo)], x: f64, y: f64, exclude: Option<&str>) -> Option<String> {
    wins.iter().find(|(l, g)| Some(l.as_str()) != exclude && x >= g.x && x < g.x + g.w && y >= g.y && y < g.y + g.h).map(|(l, _)| l.clone())
}

fn depth_ok(v: &Value, depth: usize) -> bool {
    if depth > MAX_DEPTH {
        return false;
    }
    match v {
        Value::Array(a) => a.iter().all(|x| depth_ok(x, depth + 1)),
        Value::Object(o) => o.values().all(|x| depth_ok(x, depth + 1)),
        _ => true,
    }
}
/// Parse the envelope `{v:1, kind, title, dirty, payload}` the frontend sends; returns (kind, canonical json).
pub fn check_envelope(json: &str) -> Result<(String, String), String> {
    if json.len() > MAX_PAYLOAD_BYTES {
        return Err("This pane is too large to move.".into());
    }
    let v: Value = serde_json::from_str(json).map_err(|_| "Pane state is not valid.".to_string())?;
    let kind = v.get("kind").and_then(Value::as_str).ok_or("Pane state is not valid.")?;
    if v.get("v").and_then(Value::as_i64) != Some(1) || !is_detachable(kind) || !v.get("payload").is_some_and(Value::is_object) || !depth_ok(&v, 0) {
        return Err("This pane can't be moved to its own window.".into());
    }
    Ok((kind.to_string(), json.to_string()))
}

// ---------------------------------------------------------------- store

#[derive(Debug, Clone, PartialEq)]
pub struct PaneRow {
    pub id: String,
    pub kind: String,
    pub payload: String,
    pub owner: String,
    pub geo: Option<Geo>,
}

pub struct PaneStore {
    conn: Mutex<Connection>,
}

impl PaneStore {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, String> {
        let c = Connection::open(path).map_err(|e| e.to_string())?;
        c.busy_timeout(std::time::Duration::from_secs(3)).map_err(|e| e.to_string())?;
        Self::from_conn(c)
    }
    #[cfg(test)]
    pub fn open_in_memory() -> Result<Self, String> {
        Self::from_conn(Connection::open_in_memory().map_err(|e| e.to_string())?)
    }
    fn from_conn(conn: Connection) -> Result<Self, String> {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS panes(
                id TEXT PRIMARY KEY, kind TEXT NOT NULL, payload TEXT NOT NULL, owner_window TEXT NOT NULL,
                x REAL, y REAL, w REAL, h REAL, updated_at INTEGER NOT NULL);",
        )
        .map_err(|e| e.to_string())?;
        // Rows handed back to the main window are only transient: drop leftovers from a crash mid-attach.
        conn.execute("DELETE FROM panes WHERE owner_window NOT LIKE 'pane-%'", []).map_err(|e| e.to_string())?;
        Ok(PaneStore { conn: Mutex::new(conn) })
    }
    fn lock(&self) -> Result<MutexGuard<'_, Connection>, String> {
        self.conn.lock().map_err(|_| "Pane storage is unavailable.".to_string())
    }
    fn now() -> i64 {
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
    }

    pub fn put(&self, id: &str, envelope: &str, owner: &str, geo: Geo) -> Result<(), String> {
        if !valid_pane_id(id) || !valid_label(owner) {
            return Err("Invalid pane id.".into());
        }
        let (kind, payload) = check_envelope(envelope)?;
        let c = self.lock()?;
        let n: i64 = c.query_row("SELECT COUNT(*) FROM panes", [], |r| r.get(0)).map_err(|e| e.to_string())?;
        let exists = c.query_row("SELECT 1 FROM panes WHERE id=?1", [id], |_| Ok(())).optional().map_err(|e| e.to_string())?.is_some();
        if !exists && n >= MAX_PANES {
            return Err("Too many detached windows.".into());
        }
        c.execute(
            "INSERT INTO panes(id,kind,payload,owner_window,x,y,w,h,updated_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)
             ON CONFLICT(id) DO UPDATE SET kind=excluded.kind, payload=excluded.payload, owner_window=excluded.owner_window,
               x=excluded.x, y=excluded.y, w=excluded.w, h=excluded.h, updated_at=excluded.updated_at",
            params![id, kind, payload, owner, geo.x, geo.y, geo.w, geo.h, Self::now()],
        )
        .map_err(|e| e.to_string())?;
        Ok(())
    }
    pub fn get(&self, id: &str) -> Result<Option<PaneRow>, String> {
        if !valid_pane_id(id) {
            return Err("Invalid pane id.".into());
        }
        self.lock()?
            .query_row("SELECT id,kind,payload,owner_window,x,y,w,h FROM panes WHERE id=?1", [id], row_of)
            .optional()
            .map_err(|e| e.to_string())
    }
    /// Latest payload from the owning window. Kind never changes after detach.
    pub fn update_payload(&self, id: &str, owner: &str, envelope: &str) -> Result<(), String> {
        let (kind, payload) = check_envelope(envelope)?;
        let n = self
            .lock()?
            .execute("UPDATE panes SET payload=?1, updated_at=?2 WHERE id=?3 AND owner_window=?4 AND kind=?5", params![payload, Self::now(), id, owner, kind])
            .map_err(|e| e.to_string())?;
        if n == 0 {
            return Err("This pane is not owned by this window.".into());
        }
        Ok(())
    }
    pub fn set_owner(&self, id: &str, owner: &str) -> Result<(), String> {
        if !valid_label(owner) {
            return Err("Invalid window.".into());
        }
        self.lock()?.execute("UPDATE panes SET owner_window=?1 WHERE id=?2", params![owner, id]).map_err(|e| e.to_string())?;
        Ok(())
    }
    pub fn set_geometry(&self, id: &str, g: Geo) -> Result<(), String> {
        self.lock()?.execute("UPDATE panes SET x=?1,y=?2,w=?3,h=?4 WHERE id=?5", params![g.x, g.y, g.w, g.h, id]).map_err(|e| e.to_string())?;
        Ok(())
    }
    pub fn delete(&self, id: &str) -> Result<(), String> {
        self.lock()?.execute("DELETE FROM panes WHERE id=?1", [id]).map_err(|e| e.to_string())?;
        Ok(())
    }
    /// Rows owned by a detached window (what relaunch restores).
    pub fn detached(&self) -> Result<Vec<PaneRow>, String> {
        let c = self.lock()?;
        let mut st = c.prepare("SELECT id,kind,payload,owner_window,x,y,w,h FROM panes WHERE owner_window LIKE 'pane-%' ORDER BY id LIMIT ?1").map_err(|e| e.to_string())?;
        let rows = st.query_map([MAX_PANES], row_of).map_err(|e| e.to_string())?;
        rows.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())
    }
}

fn row_of(r: &rusqlite::Row<'_>) -> rusqlite::Result<PaneRow> {
    let (x, y, w, h): (Option<f64>, Option<f64>, Option<f64>, Option<f64>) = (r.get(4)?, r.get(5)?, r.get(6)?, r.get(7)?);
    let geo = match (x, y, w, h) {
        (Some(x), Some(y), Some(w), Some(h)) => clamp_geometry(x, y, w, h).ok(),
        _ => None,
    };
    Ok(PaneRow { id: r.get(0)?, kind: r.get(1)?, payload: r.get(2)?, owner: r.get(3)?, geo })
}

// ---------------------------------------------------------------- windows

/// Quit state: the flag, plus how many detached windows have written their payload since the flush was requested.
#[derive(Default)]
pub struct Shutdown {
    flag: AtomicBool,
    acks: Mutex<HashSet<String>>,
}
impl Shutdown {
    /// Block until `expected` acks arrived (never before `floor`, so the main window's own flush can finish), or `cap` passed.
    pub fn wait_acks(&self, expected: usize, floor: std::time::Duration, cap: std::time::Duration) -> bool {
        let t0 = std::time::Instant::now();
        loop {
            let e = t0.elapsed();
            let got = self.acks.lock().map(|a| a.len()).unwrap_or(usize::MAX) >= expected;
            if got && e >= floor {
                return true;
            }
            if e >= cap {
                return got;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }
}

fn build_window(app: &AppHandle, id: &str, geo: Geo) -> Result<WebviewWindow, String> {
    let label = label_for(id)?;
    // Both the URL fragment (`#pane=<id>`) and an init script carry the id; the frontend accepts either.
    let url = WebviewUrl::App(format!("index.html#pane={id}").into());
    let b = WebviewWindowBuilder::new(app, &label, url)
        .title("MiraMail")
        .inner_size(geo.w, geo.h)
        .position(geo.x, geo.y)
        .min_inner_size(MIN_W, MIN_H)
        .disable_drag_drop_handler()
        .initialization_script(format!("window.__MIRAMAIL_PANE__ = \"{id}\";"));
    #[cfg(target_os = "macos")]
    let b = b.title_bar_style(tauri::TitleBarStyle::Overlay).hidden_title(true);
    crate::linknav::harden(b, app, &label).build().map_err(|_| "Could not open a new window.".to_string())
}

fn logical_geo(w: &WebviewWindow) -> Option<Geo> {
    let s = w.scale_factor().ok()?;
    let p = w.outer_position().ok()?.to_logical::<f64>(s);
    let z = w.inner_size().ok()?.to_logical::<f64>(s);
    Some(Geo { x: p.x, y: p.y, w: z.width, h: z.height })
}
fn outer_rect(w: &WebviewWindow) -> Option<Geo> {
    let s = w.scale_factor().ok()?;
    let p = w.outer_position().ok()?.to_logical::<f64>(s);
    let z = w.outer_size().ok()?.to_logical::<f64>(s);
    Some(Geo { x: p.x, y: p.y, w: z.width, h: z.height })
}

/// Rows `restore` may reopen: the owner must be exactly this pane's own window. The rest are unservable
/// (`get_pane` would refuse them, leaving a window nobody can close) and are returned to be deleted.
pub fn split_restorable(rows: Vec<PaneRow>) -> (Vec<PaneRow>, Vec<String>) {
    let mut keep = Vec::new();
    let mut drop = Vec::new();
    for r in rows {
        if label_for(&r.id).is_ok_and(|l| l == r.owner) {
            keep.push(r);
        } else {
            drop.push(r.id);
        }
    }
    (keep, drop)
}

fn to_plat(g: Geo) -> crate::plat_prefs::Geo {
    crate::plat_prefs::Geo { x: g.x, y: g.y, w: g.w, h: g.h }
}
fn from_plat(g: crate::plat_prefs::Geo) -> Geo {
    Geo { x: g.x, y: g.y, w: g.w, h: g.h }
}

/// Where a restored window opens: its saved rectangle if still on a connected screen, else a cascade step near
/// the main window (or a fixed spot when there is no main geometry). `idx` staggers the fallbacks.
pub fn placement(saved: Option<Geo>, screens: &[crate::plat_prefs::Geo], main: Option<Geo>, idx: usize) -> Geo {
    let scr = screens;
    if let Some(g) = saved.and_then(|g| crate::winstate::clamp_visible(to_plat(g), scr)) {
        return from_plat(g);
    }
    let (w, h) = saved.map_or((720.0, 560.0), |g| (g.w, g.h));
    let step = 28.0 * (idx % 8) as f64;
    let (bx, by) = main.map_or((120.0, 120.0), |m| (m.x + 60.0, m.y + 60.0));
    let g = Geo { x: bx + step, y: by + step, w, h };
    // Still off every screen (e.g. main itself is odd): fall back to the first screen's corner.
    match crate::winstate::clamp_visible(to_plat(g), scr) {
        Some(_) => g,
        None => screens.first().map_or(g, |s| Geo { x: s.x + 80.0 + step, y: s.y + 80.0 + step, w: w.min(s.w), h: h.min(s.h) }),
    }
}

/// Called at setup: reopen every window that was detached when the app last quit.
pub fn restore(app: &AppHandle) {
    let store = app.state::<PaneStore>();
    let (rows, bad) = split_restorable(store.detached().unwrap_or_default());
    for id in bad {
        let _ = store.delete(&id);
    }
    let screens = crate::winstate::screens(app);
    let main = app.get_webview_window(MAIN).and_then(|w| logical_geo(&w));
    for (i, row) in rows.iter().enumerate() {
        let geo = placement(row.geo, &screens, main, i);
        if build_window(app, &row.id, geo).is_err() {
            eprintln!("could not restore a detached window");
        }
    }
}

fn snapshot_geometry(app: &AppHandle) {
    let store = app.state::<PaneStore>();
    for (label, w) in app.webview_windows() {
        if let (Some(id), Some(g)) = (pane_id_of(&label), logical_geo(&w)) {
            let _ = store.set_geometry(id, g);
        }
    }
}

/// Quit sequence, run once: remember where every detached window is, tell them to flush their payloads, then exit.
/// Windows are never destroyed individually here, so their `panes` rows survive for the next launch; the main
/// window is the last thing to go because the process exits only after the flush delay.
pub fn begin_shutdown(app: &AppHandle) {
    if app.state::<Shutdown>().flag.swap(true, Ordering::SeqCst) {
        return;
    }
    snapshot_geometry(app);
    let expected = app.webview_windows().keys().filter(|l| pane_id_of(l).is_some()).count();
    let _ = app.emit("pane://flush", ());
    let h = app.clone();
    std::thread::spawn(move || {
        // Each detached window acks by saving its payload (`save_pane`); wait for all of them, bounded.
        h.state::<Shutdown>().wait_acks(expected, std::time::Duration::from_millis(150), std::time::Duration::from_millis(1500));
        h.exit(0);
    });
}
pub fn shutting_down(app: &AppHandle) -> bool {
    app.state::<Shutdown>().flag.load(Ordering::SeqCst)
}

/// Window event hook: detached windows ask their webview before closing (dirty draft prompt); main starts the quit sequence.
pub fn on_window_event(window: &tauri::Window, event: &tauri::WindowEvent) {
    if let tauri::WindowEvent::CloseRequested { api, .. } = event {
        let app = window.app_handle();
        if shutting_down(app) {
            return;
        }
        let label = window.label();
        if label == MAIN {
            api.prevent_close();
            begin_shutdown(app);
        } else if pane_id_of(label).is_some() {
            api.prevent_close();
            let _ = app.emit_to(label, "pane://close-request", ());
        }
    }
}

/// Menu events go to the focused window only (a broadcast would act in every window).
pub fn emit_to_focused(app: &AppHandle, event: &str) {
    let label = app.webview_windows().into_iter().find(|(_, w)| w.is_focused().unwrap_or(false)).map(|(l, _)| l).unwrap_or_else(|| MAIN.to_string());
    let _ = app.emit_to(label, event, ());
}

fn caller_pane_id(w: &WebviewWindow) -> Result<String, String> {
    pane_id_of(w.label()).map(String::from).ok_or_else(|| "Only a detached window can do this.".to_string())
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct PaneEvent {
    pane_id: String,
    label: String,
}

// ---------------------------------------------------------------- commands

/// Called from the main window (tear-off) or from a detached window (a new pane opens in its own window). Creates `pane-<id>`, owner of the pane from now on. Returns the window label.
#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn detach_pane(app: AppHandle, window: WebviewWindow, store: State<'_, PaneStore>, pane_id: String, payload_json: String, x: f64, y: f64, w: f64, h: f64) -> Result<String, String> {
    // Main, or a detached window opening a NEW pane (e.g. Cmd+N): detached windows always hold exactly one pane.
    if !valid_label(window.label()) {
        return Err("Invalid window.".into());
    }
    let label = label_for(&pane_id)?;
    let geo = clamp_geometry(x, y, w, h)?;
    let existing = store.get(&pane_id)?;
    if !detach_allowed(window.label(), &pane_id, existing.is_some()) {
        return Err("A detached window can only open a new message or event.".into());
    }
    if let Some(row) = existing {
        if row.owner.starts_with(PREFIX) && app.get_webview_window(&row.owner).is_some() {
            return Err("This pane is already in its own window.".into());
        }
    }
    store.put(&pane_id, &payload_json, &label, geo)?;
    if let Err(e) = build_window(&app, &pane_id, geo) {
        let _ = store.delete(&pane_id);
        return Err(e);
    }
    let _ = app.emit("pane://detached", PaneEvent { pane_id, label: label.clone() });
    Ok(label)
}

/// The detached window reads its pane (envelope JSON) once at startup.
#[tauri::command]
pub async fn get_pane(window: WebviewWindow, store: State<'_, PaneStore>) -> Result<Option<String>, String> {
    let id = caller_pane_id(&window)?;
    Ok(store.get(&id)?.filter(|r| r.owner == window.label()).map(|r| r.payload))
}

/// Keeps the handoff row current so a quit / crash / attach never loses the latest state.
#[tauri::command]
pub async fn save_pane(window: WebviewWindow, store: State<'_, PaneStore>, shutdown: State<'_, Shutdown>, payload_json: String) -> Result<(), String> {
    let id = caller_pane_id(&window)?;
    let r = store.update_payload(&id, window.label(), &payload_json);
    if shutdown.flag.load(Ordering::SeqCst) {
        if let Ok(mut a) = shutdown.acks.lock() {
            a.insert(window.label().to_string());
        } // quit in progress: this window's flush landed (or failed; either way it is done)
    }
    r
}

/// Dock spec chosen by the sender; the receiver re-validates it against its own layout.
#[derive(Deserialize, Serialize, Clone, Debug)]
pub struct DockSpec {
    pub target: String,
    pub zone: String,
}
#[derive(Deserialize, Serialize, Clone, Copy, Debug)]
pub struct ScreenPoint {
    pub x: f64,
    pub y: f64,
}
#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct AttachEvent {
    pane_id: String,
    envelope: String,
    dock: Option<DockSpec>,
    at: Option<ScreenPoint>,
    park: bool,
}

/// Detached window -> main window. The caller must own the pane. The main window receives `pane://attach` with
/// the stored envelope; this window is then destroyed (the row stays until main calls `release_pane`).
#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn attach_pane(app: AppHandle, window: WebviewWindow, store: State<'_, PaneStore>, pane_id: String, target_label: String, dock: Option<DockSpec>, at: Option<ScreenPoint>, park: Option<bool>) -> Result<(), String> {
    let own = caller_pane_id(&window)?;
    if own != pane_id || !valid_label(&target_label) {
        return Err("Invalid pane.".into());
    }
    // Panes only ever go back to the main window (it owns the mailbox, folders and the draft tray).
    if target_label != MAIN || app.get_webview_window(MAIN).is_none() {
        return Err("The main window is not available.".into());
    }
    if let Some(d) = &dock {
        if !valid_pane_id(&d.target) || !["left", "right", "top", "bottom", "center"].contains(&d.zone.as_str()) {
            return Err("Invalid dock target.".into());
        }
    }
    let at = match at {
        Some(p) => {
            let (x, y) = clamp_point(p.x, p.y)?;
            Some(ScreenPoint { x, y })
        }
        None => None,
    };
    let row = store.get(&pane_id)?.filter(|r| r.owner == window.label()).ok_or("This pane is not owned by this window.")?;
    store.set_owner(&pane_id, MAIN)?;
    if app.emit_to(MAIN, "pane://attach", AttachEvent { pane_id: pane_id.clone(), envelope: row.payload, dock, at, park: park.unwrap_or(false) }).is_err() {
        // Nobody got it: this window keeps the pane.
        let _ = store.set_owner(&pane_id, window.label());
        return Err("Could not reach the main window.".into());
    }
    let _ = app.emit("pane://closed", PaneEvent { pane_id, label: window.label().to_string() });
    let _ = window.destroy();
    Ok(())
}

/// Main window acknowledges an attach: the transient row can go.
#[tauri::command]
pub async fn release_pane(window: WebviewWindow, store: State<'_, PaneStore>, pane_id: String) -> Result<(), String> {
    if window.label() != MAIN || !valid_pane_id(&pane_id) {
        return Err("Invalid request.".into());
    }
    if store.get(&pane_id)?.is_some_and(|r| r.owner == MAIN) {
        store.delete(&pane_id)?;
    }
    Ok(())
}

/// A detached window whose pane is gone (sent, discarded, parked) closes itself and forgets the pane.
#[tauri::command]
pub async fn close_pane_window(app: AppHandle, window: WebviewWindow, store: State<'_, PaneStore>) -> Result<(), String> {
    let id = caller_pane_id(&window)?;
    if store.get(&id)?.is_some_and(|r| r.owner == window.label()) {
        store.delete(&id)?;
    }
    let _ = app.emit("pane://closed", PaneEvent { pane_id: id, label: window.label().to_string() });
    window.destroy().map_err(|_| "Could not close the window.".to_string())
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WindowInfo {
    label: String,
    pane_id: Option<String>,
    kind: Option<String>,
    focused: bool,
    #[serde(flatten)]
    geo: Option<Geo>,
}

#[tauri::command]
pub async fn list_windows(app: AppHandle, store: State<'_, PaneStore>) -> Result<Vec<WindowInfo>, String> {
    let rows = store.detached()?;
    let mut out: Vec<WindowInfo> = app
        .webview_windows()
        .into_iter()
        .filter(|(l, _)| valid_label(l))
        .map(|(label, w)| {
            let row = pane_id_of(&label).and_then(|id| rows.iter().find(|r| r.id == id));
            WindowInfo { pane_id: row.map(|r| r.id.clone()), kind: row.map(|r| r.kind.clone()), focused: w.is_focused().unwrap_or(false), geo: logical_geo(&w), label }
        })
        .collect();
    out.sort_by(|a, b| (a.label != MAIN, &a.label).cmp(&(b.label != MAIN, &b.label)));
    Ok(out)
}

/// Which MiraMail window is under this screen point (logical px)? The calling window is skipped.
#[tauri::command]
pub async fn window_at(app: AppHandle, window: WebviewWindow, screen_x: f64, screen_y: f64) -> Result<Option<String>, String> {
    let (x, y) = clamp_point(screen_x, screen_y)?;
    let mut wins: Vec<(bool, String, Geo)> = app
        .webview_windows()
        .into_iter()
        .filter(|(l, w)| valid_label(l) && w.is_visible().unwrap_or(false) && !w.is_minimized().unwrap_or(false))
        .filter_map(|(l, w)| outer_rect(&w).map(|g| (w.is_focused().unwrap_or(false), l, g)))
        .collect();
    // Priority: focused first, then main, then the rest by label (z-order is not exposed by Tauri).
    wins.sort_by(|a, b| (!a.0, a.1 != MAIN, &a.1).cmp(&(!b.0, b.1 != MAIN, &b.1)));
    let ordered: Vec<(String, Geo)> = wins.into_iter().map(|(_, l, g)| (l, g)).collect();
    Ok(hit_test(&ordered, x, y, Some(window.label())))
}

#[tauri::command]
pub async fn focus_window(app: AppHandle, label: String) -> Result<(), String> {
    if !valid_label(&label) {
        return Err("Invalid window.".into());
    }
    let w = app.get_webview_window(&label).ok_or("That window is not open.")?;
    let _ = w.unminimize();
    w.set_focus().map_err(|_| "Could not focus the window.".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn env(kind: &str) -> String {
        json!({"v": 1, "kind": kind, "title": "T", "dirty": false, "payload": {"text": "hello"}}).to_string()
    }
    const G: Geo = Geo { x: 10.0, y: 20.0, w: 500.0, h: 400.0 };

    #[test]
    fn pane_ids_and_labels() {
        assert!(valid_pane_id("c-1a2B_x"));
        assert_eq!(label_for("c-abc").unwrap(), "pane-c-abc");
        for bad in ["", "a b", "a/b", "a:b", "../x", "a.b", "é", "a\u{0}b", &"a".repeat(65)] {
            assert!(label_for(bad).is_err(), "{bad:?}");
            assert!(!valid_label(&format!("pane-{bad}")), "{bad:?}");
        }
        assert!(valid_label("main"));
        assert!(valid_label("pane-aichat"));
        for bad in ["", "Main", "pane-", "pane", "pane-a/b", "pane:x", "other", "main ", "pane--x/..", "pane-a b"] {
            assert!(!valid_label(bad), "{bad:?}");
        }
        assert_eq!(pane_id_of("pane-e-1"), Some("e-1"));
        assert_eq!(pane_id_of("main"), None);
        assert_eq!(pane_id_of("pane-"), None);
    }

    #[test]
    fn only_self_contained_kinds_detach() {
        for k in ["compose", "event", "aichat"] {
            assert!(is_detachable(k));
        }
        for k in ["mailbox", "reader", "calendar", "", "Compose"] {
            assert!(!is_detachable(k));
        }
        assert!(check_envelope(&env("mailbox")).is_err());
        assert!(check_envelope(&env("compose")).is_ok());
    }

    #[test]
    fn envelope_validation_is_strict() {
        assert!(check_envelope("nope").is_err());
        assert!(check_envelope("[]").is_err());
        assert!(check_envelope(r#"{"v":2,"kind":"compose","payload":{}}"#).is_err());
        assert!(check_envelope(r#"{"v":1,"kind":"compose","payload":"x"}"#).is_err());
        assert!(check_envelope(r#"{"v":1,"kind":"compose"}"#).is_err());
        let big = json!({"v":1,"kind":"compose","payload":{"t":"x".repeat(MAX_PAYLOAD_BYTES)}}).to_string();
        let e = check_envelope(&big).unwrap_err();
        assert!(!e.contains("xxxx"), "errors never echo content");
        let mut deep = json!(1);
        for _ in 0..60 {
            deep = json!([deep]);
        }
        assert!(check_envelope(&json!({"v":1,"kind":"event","payload":{"d":deep}}).to_string()).is_err());
    }

    #[test]
    fn geometry_is_clamped_and_rejects_non_finite() {
        let g = clamp_geometry(1e9, -1e9, 1.0, 1e9).unwrap();
        assert_eq!(g, Geo { x: COORD_MAX, y: COORD_MIN, w: MIN_W, h: MAX_SIZE });
        assert_eq!(clamp_geometry(100.0, 50.0, 800.0, 600.0).unwrap(), Geo { x: 100.0, y: 50.0, w: 800.0, h: 600.0 });
        for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert!(clamp_geometry(bad, 0.0, 400.0, 300.0).is_err());
            assert!(clamp_geometry(0.0, 0.0, bad, 300.0).is_err());
            assert!(clamp_point(0.0, bad).is_err());
        }
        assert_eq!(clamp_point(1e12, -1e12).unwrap(), (COORD_MAX, COORD_MIN));
    }

    #[test]
    fn window_hit_test_math() {
        let a = Geo { x: 0.0, y: 0.0, w: 100.0, h: 100.0 };
        let b = Geo { x: 50.0, y: 50.0, w: 100.0, h: 100.0 };
        let wins = vec![("main".to_string(), a), ("pane-x".to_string(), b)];
        assert_eq!(hit_test(&wins, 10.0, 10.0, None).as_deref(), Some("main"));
        assert_eq!(hit_test(&wins, 120.0, 120.0, None).as_deref(), Some("pane-x"));
        assert_eq!(hit_test(&wins, 75.0, 75.0, None).as_deref(), Some("main"), "first in priority order wins on overlap");
        assert_eq!(hit_test(&wins, 75.0, 75.0, Some("main")).as_deref(), Some("pane-x"), "excluded window is skipped");
        assert_eq!(hit_test(&wins, 0.0, 0.0, None).as_deref(), Some("main"), "top-left edge is inside");
        assert_eq!(hit_test(&wins, 100.0, 10.0, None), None, "right edge is outside");
        assert_eq!(hit_test(&wins, 10.0, 100.0, None), None, "bottom edge is outside");
        assert_eq!(hit_test(&wins, -1.0, 10.0, None), None);
        assert_eq!(hit_test(&wins, 500.0, 500.0, None), None);
        assert_eq!(hit_test(&[], 1.0, 1.0, None), None);
        assert_eq!(hit_test(&wins, f64::NAN, 1.0, None), None, "NaN never matches");
    }

    #[test]
    fn store_handoff_roundtrip() {
        let s = PaneStore::open_in_memory().unwrap();
        s.put("c-1", &env("compose"), "pane-c-1", G).unwrap();
        let r = s.get("c-1").unwrap().unwrap();
        assert_eq!((r.kind.as_str(), r.owner.as_str(), r.geo), ("compose", "pane-c-1", Some(G)));
        assert_eq!(s.detached().unwrap().len(), 1);
        // only the owner may update the payload
        let newer = json!({"v":1,"kind":"compose","title":"T","dirty":true,"payload":{"text":"more"}}).to_string();
        assert!(s.update_payload("c-1", "pane-other", &newer).is_err());
        assert!(s.update_payload("c-1", "main", &newer).is_err());
        s.update_payload("c-1", "pane-c-1", &newer).unwrap();
        assert!(s.get("c-1").unwrap().unwrap().payload.contains("more"));
        // kind cannot be switched by an update
        assert!(s.update_payload("c-1", "pane-c-1", &env("event")).is_err());
        // handed back to main: no longer "detached"
        s.set_owner("c-1", MAIN).unwrap();
        assert!(s.detached().unwrap().is_empty());
        s.delete("c-1").unwrap();
        assert!(s.get("c-1").unwrap().is_none());
        s.delete("c-1").unwrap(); // idempotent
    }

    #[test]
    fn store_rejects_bad_input() {
        let s = PaneStore::open_in_memory().unwrap();
        assert!(s.put("../x", &env("compose"), "pane-x", G).is_err());
        assert!(s.put("a", &env("compose"), "pane-a/b", G).is_err());
        assert!(s.put("a", &env("compose"), "bogus", G).is_err());
        assert!(s.put("a", &env("calendar"), "pane-a", G).is_err());
        assert!(s.get("a b").is_err());
        assert!(s.get("nope").unwrap().is_none());
    }

    #[test]
    fn store_caps_pane_count_and_geometry_roundtrip() {
        let s = PaneStore::open_in_memory().unwrap();
        for i in 0..MAX_PANES {
            s.put(&format!("p{i}"), &env("aichat"), &format!("pane-p{i}"), G).unwrap();
        }
        assert!(s.put("extra", &env("aichat"), "pane-extra", G).is_err());
        s.put("p0", &env("aichat"), "pane-p0", G).unwrap(); // updating an existing one still works
        let g2 = Geo { x: -5.0, y: 7.5, w: 640.0, h: 480.0 };
        s.set_geometry("p1", g2).unwrap();
        assert_eq!(s.get("p1").unwrap().unwrap().geo, Some(g2));
    }

    #[test]
    fn restore_list_survives_reopen_and_drops_transient_rows() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("workspace.db");
        {
            let s = PaneStore::open(&p).unwrap();
            s.put("a", &env("compose"), "pane-a", G).unwrap();
            s.put("b", &env("event"), "pane-b", G).unwrap();
            s.set_owner("b", MAIN).unwrap(); // crashed mid-attach
        }
        let s = PaneStore::open(&p).unwrap();
        let ids: Vec<String> = s.detached().unwrap().into_iter().map(|r| r.id).collect();
        assert_eq!(ids, vec!["a".to_string()]);
        assert!(s.get("b").unwrap().is_none());
    }

    fn row(id: &str, owner: &str) -> PaneRow {
        PaneRow { id: id.into(), kind: "compose".into(), payload: env("compose"), owner: owner.into(), geo: None }
    }

    #[test]
    fn restore_skips_rows_not_owned_by_their_own_window() {
        let (keep, drop) = split_restorable(vec![row("a", "pane-a"), row("b", "pane-zzz"), row("c", "main")]);
        assert_eq!(keep.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(), ["a"]);
        assert_eq!(drop, ["b", "c"]);
    }

    #[test]
    fn detached_list_is_capped() {
        let s = PaneStore::open_in_memory().unwrap();
        for i in 0..MAX_PANES {
            s.put(&format!("p{i}"), &env("aichat"), &format!("pane-p{i}"), G).unwrap();
        }
        assert_eq!(s.detached().unwrap().len() as i64, MAX_PANES);
    }

    #[test]
    fn detach_rules_per_caller() {
        assert!(detach_allowed("main", "aichat", true));
        assert!(!detach_allowed("main", "../x", false));
        assert!(detach_allowed("pane-c-1", "c-2", false));
        assert!(detach_allowed("pane-c-1", "e-2", false));
        assert!(!detach_allowed("pane-c-1", "c-2", true), "id already in the table");
        assert!(!detach_allowed("pane-c-1", "aichat", false), "wrong prefix");
        assert!(!detach_allowed("pane-c-1", "x-1", false));
        assert!(!detach_allowed("widget", "c-1", false));
    }

    #[test]
    fn restored_windows_land_on_a_screen() {
        let scr = [crate::plat_prefs::Geo { x: 0.0, y: 0.0, w: 1440.0, h: 900.0 }];
        let main = Some(Geo { x: 100.0, y: 80.0, w: 1000.0, h: 700.0 });
        let on = Geo { x: 50.0, y: 50.0, w: 600.0, h: 400.0 };
        assert_eq!(placement(Some(on), &scr, main, 0), on);
        let off = Geo { x: 5000.0, y: 4000.0, w: 600.0, h: 400.0 };
        let p = placement(Some(off), &scr, main, 0);
        assert_eq!((p.x, p.y, p.w, p.h), (160.0, 140.0, 600.0, 400.0), "cascades near main, keeps size");
        let p2 = placement(None, &scr, main, 2);
        assert_eq!((p2.x, p2.y), (216.0, 196.0));
        // main also off-screen: first screen corner
        let far = Some(Geo { x: 9000.0, y: 9000.0, w: 100.0, h: 100.0 });
        let p3 = placement(Some(off), &scr, far, 0);
        assert!(p3.x < 1440.0 && p3.y < 900.0);
    }

    #[test]
    fn shutdown_waits_for_acks_but_is_capped() {
        use std::sync::Arc;
        use std::time::Duration;
        let s = Arc::new(Shutdown::default());
        let t0 = std::time::Instant::now();
        assert!(s.wait_acks(0, Duration::from_millis(30), Duration::from_millis(500)));
        assert!(t0.elapsed() >= Duration::from_millis(30));
        let s2 = s.clone();
        let h = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(60));
            s2.acks.lock().unwrap().extend(["pane-a".to_string(), "pane-b".to_string()]);
        });
        assert!(s.wait_acks(2, Duration::ZERO, Duration::from_secs(5)));
        h.join().unwrap();
        let t1 = std::time::Instant::now();
        assert!(!s.wait_acks(9, Duration::ZERO, Duration::from_millis(80)), "timeout cap returns");
        assert!(t1.elapsed() < Duration::from_secs(2));
    }
}
