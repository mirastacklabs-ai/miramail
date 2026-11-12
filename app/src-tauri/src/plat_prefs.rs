//! Platform-integration preferences (Dock badge, menu-bar item, mini widget, notifications, remembered window
//! geometry). Plain JSON file `platform_prefs.json` in the app data dir; missing or corrupt file -> defaults.
//! Nothing here is secret and nothing leaves the Mac.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

pub const FILE: &str = "platform_prefs.json";

/// Logical (DPI-independent) window rectangle.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Geo {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum TrayStyle {
    /// The unread number (also "0").
    #[default]
    #[serde(rename = "unread")]
    Unread,
    /// Number of messages in the cached INBOXes.
    #[serde(rename = "total")]
    Total,
    /// The unread number, nothing at all when it is 0.
    #[serde(rename = "hideZero")]
    HideZero,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct PlatformPrefs {
    pub dock_badge: bool,
    pub tray_show: bool,
    pub tray_style: TrayStyle,
    pub widget_show: bool,
    /// Show subjects (not just senders) in the mini widget.
    pub widget_previews: bool,
    /// Widget shows one account only; `None` = all accounts combined.
    pub widget_account: Option<String>,
    pub widget_geo: Option<Geo>,
    pub main_geo: Option<Geo>,
    pub notify: bool,
    pub notify_preview: bool,
    pub notify_sound: bool,
    /// Accounts the user switched notifications off for.
    pub notify_off: Vec<String>,
    /// Accounts that notify only for VIP senders (local list; see `localorg`).
    pub notify_vip_only: Vec<String>,
    /// Mark mail of muted conversations as read when it arrives (off by default).
    pub mute_auto_read: bool,
}

impl Default for PlatformPrefs {
    fn default() -> Self {
        PlatformPrefs {
            dock_badge: true,
            tray_show: false,
            tray_style: TrayStyle::Unread,
            widget_show: false,
            widget_previews: false,
            widget_account: None,
            widget_geo: None,
            main_geo: None,
            notify: false,
            notify_preview: true,
            notify_sound: true,
            notify_off: Vec::new(),
            notify_vip_only: Vec::new(),
            mute_auto_read: false,
        }
    }
}

fn sane_geo(g: Geo, min_w: f64, min_h: f64) -> Option<Geo> {
    let ok = [g.x, g.y, g.w, g.h].iter().all(|v| v.is_finite()) && g.x.abs() < 100_000.0 && g.y.abs() < 100_000.0;
    ok.then(|| Geo { x: g.x, y: g.y, w: g.w.clamp(min_w, 6000.0), h: g.h.clamp(min_h, 6000.0) })
}

impl PlatformPrefs {
    /// Drop nonsense that a hand-edited file or a buggy caller could carry (NaN geometry, huge lists).
    pub fn sanitized(mut self) -> Self {
        self.widget_geo = self.widget_geo.and_then(|g| sane_geo(g, 140.0, 140.0));
        self.main_geo = self.main_geo.and_then(|g| sane_geo(g, 860.0, 560.0));
        // No notification backend off macOS yet (iOS phase 3): never "on", including prefs restored from a backup.
        if !cfg!(target_os = "macos") {
            self.notify = false;
        }
        self.notify_off.retain(|a| !a.is_empty() && a.len() <= 320);
        self.notify_off.sort();
        self.notify_off.dedup();
        self.notify_off.truncate(200);
        self.notify_vip_only.retain(|a| !a.is_empty() && a.len() <= 320);
        self.notify_vip_only.sort();
        self.notify_vip_only.dedup();
        self.notify_vip_only.truncate(200);
        if self.widget_account.as_ref().is_some_and(|a| a.is_empty() || a.len() > 320) {
            self.widget_account = None;
        }
        self
    }
}

/// Missing, unreadable or corrupt -> defaults. Never fails.
pub fn load(path: &Path) -> PlatformPrefs {
    std::fs::read(path).ok().and_then(|b| serde_json::from_slice::<PlatformPrefs>(&b).ok()).unwrap_or_default().sanitized()
}

/// Atomic write (temp file in the same directory, then rename).
pub fn save(path: &Path, p: &PlatformPrefs) -> std::io::Result<()> {
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(p).map_err(std::io::Error::other)?)?;
    std::fs::rename(&tmp, path)
}

#[derive(Default)]
pub struct PlatformState {
    prefs: Mutex<PlatformPrefs>,
    path: Mutex<Option<PathBuf>>,
}

impl PlatformState {
    pub fn init(&self, path: PathBuf) {
        *self.prefs.lock().unwrap_or_else(|e| e.into_inner()) = load(&path);
        *self.path.lock().unwrap_or_else(|e| e.into_inner()) = Some(path);
    }
    pub fn get(&self) -> PlatformPrefs {
        self.prefs.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
    /// Replace everything (sanitised) and persist. No-op persistence when no path was set (tests).
    pub fn set(&self, p: PlatformPrefs) -> Result<PlatformPrefs, String> {
        let p = p.sanitized();
        *self.prefs.lock().unwrap_or_else(|e| e.into_inner()) = p.clone();
        self.persist()?;
        Ok(p)
    }
    /// Change in memory only (geometry updates while dragging); `persist` writes later.
    pub fn update_mem(&self, f: impl FnOnce(&mut PlatformPrefs)) {
        f(&mut self.prefs.lock().unwrap_or_else(|e| e.into_inner()));
    }
    pub fn persist(&self) -> Result<(), String> {
        let Some(path) = self.path.lock().unwrap_or_else(|e| e.into_inner()).clone() else { return Ok(()) };
        save(&path, &self.get()).map_err(|e| format!("Couldn't save preferences: {e}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_conservative() {
        let d = PlatformPrefs::default();
        assert!(d.dock_badge && !d.tray_show && !d.widget_show && !d.notify && !d.widget_previews);
    }

    #[test]
    fn roundtrip_through_a_temp_dir_and_survives_restart() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(FILE);
        let s = PlatformState::default();
        s.init(path.clone());
        let mut p = s.get();
        p.tray_show = true;
        p.tray_style = TrayStyle::HideZero;
        p.widget_geo = Some(Geo { x: 10.0, y: 20.0, w: 220.0, h: 260.0 });
        p.notify_off = vec!["b@x.org".into(), "a@x.org".into(), "a@x.org".into()];
        s.set(p).unwrap();
        let s2 = PlatformState::default();
        s2.init(path);
        let g = s2.get();
        assert!(g.tray_show);
        assert_eq!(g.tray_style, TrayStyle::HideZero);
        assert_eq!(g.widget_geo, Some(Geo { x: 10.0, y: 20.0, w: 220.0, h: 260.0 }));
        assert_eq!(g.notify_off, vec!["a@x.org".to_string(), "b@x.org".to_string()], "sorted and de-duplicated");
    }

    #[test]
    fn corrupt_or_missing_file_gives_defaults_and_partial_files_fill_in() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(FILE);
        assert_eq!(load(&path), PlatformPrefs::default());
        std::fs::write(&path, b"{ not json").unwrap();
        assert_eq!(load(&path), PlatformPrefs::default());
        std::fs::write(&path, br#"{"trayShow":true,"trayStyle":"total"}"#).unwrap();
        let p = load(&path);
        assert!(p.tray_show && p.dock_badge, "missing keys take defaults");
        assert_eq!(p.tray_style, TrayStyle::Total);
    }

    #[test]
    fn nonsense_geometry_is_dropped_or_clamped() {
        let p = PlatformPrefs {
            widget_geo: Some(Geo { x: f64::NAN, y: 0.0, w: 200.0, h: 200.0 }),
            main_geo: Some(Geo { x: 5.0, y: 5.0, w: 10.0, h: 10_000.0 }),
            ..Default::default()
        }
        .sanitized();
        assert_eq!(p.widget_geo, None);
        assert_eq!(p.main_geo, Some(Geo { x: 5.0, y: 5.0, w: 860.0, h: 6000.0 }));
    }

    #[test]
    fn no_leftover_temp_file_after_save() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(FILE);
        save(&path, &PlatformPrefs::default()).unwrap();
        let names: Vec<_> = std::fs::read_dir(dir.path()).unwrap().map(|e| e.unwrap().file_name()).collect();
        assert_eq!(names.len(), 1);
    }
}
