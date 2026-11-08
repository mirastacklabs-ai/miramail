//! Window state restore: remembers where the main window and the mini widget were (logical coordinates) and puts
//! them back on the next launch, but only if that spot is still on a connected screen. Also the one
//! `on_window_event` hook, which delegates to `panes` first.

use crate::plat_prefs::{Geo, PlatformState};
use crate::{notify, panes, widget};
use std::sync::atomic::{AtomicU64, Ordering};
use tauri::{AppHandle, LogicalPosition, LogicalSize, Manager, Window, WindowEvent};

/// Keep `g` only if at least a `60 x 40` corner of it is on some screen (so a window left on an unplugged
/// monitor comes back at the default position instead of off-screen).
pub fn clamp_visible(g: Geo, screens: &[Geo]) -> Option<Geo> {
    let ok = screens.iter().any(|s| {
        let (ox, oy) = (g.x.max(s.x), g.y.max(s.y));
        let (ex, ey) = ((g.x + g.w).min(s.x + s.w), (g.y + g.h).min(s.y + s.h));
        ex - ox >= 60.0 && ey - oy >= 40.0
    });
    ok.then_some(g)
}

pub fn screens(app: &AppHandle) -> Vec<Geo> {
    app.available_monitors()
        .unwrap_or_default()
        .iter()
        .map(|m| {
            let f = m.scale_factor();
            Geo { x: m.position().x as f64 / f, y: m.position().y as f64 / f, w: m.size().width as f64 / f, h: m.size().height as f64 / f }
        })
        .collect()
}

pub fn geo_of(w: &Window) -> Option<Geo> {
    let f = w.scale_factor().ok()?;
    let (p, s) = (w.outer_position().ok()?, w.inner_size().ok()?);
    Some(Geo { x: p.x as f64 / f, y: p.y as f64 / f, w: s.width as f64 / f, h: s.height as f64 / f })
}

pub fn apply_geo(w: &tauri::WebviewWindow, g: Geo) {
    let _ = w.set_size(LogicalSize::new(g.w, g.h));
    let _ = w.set_position(LogicalPosition::new(g.x, g.y));
}

/// Launch: put the main window back where it was.
pub fn restore_main(app: &AppHandle) {
    let Some(w) = app.get_webview_window(panes::MAIN) else { return };
    let saved = app.state::<PlatformState>().get().main_geo;
    if let Some(g) = saved.and_then(|g| clamp_visible(g, &screens(app))) {
        apply_geo(&w, g);
    }
}

static LAST_WRITE_MS: AtomicU64 = AtomicU64::new(0);

fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

/// Write at most about once a second while a window is being dragged; `force` always writes.
fn persist(app: &AppHandle, force: bool) {
    let now = now_ms();
    if force || now.saturating_sub(LAST_WRITE_MS.load(Ordering::Relaxed)) >= 1000 {
        LAST_WRITE_MS.store(now, Ordering::Relaxed);
        let _ = app.state::<PlatformState>().persist();
    }
}

/// Called on quit so the final position is never lost to the throttle.
pub fn save_all(app: &AppHandle) {
    if let Some(w) = app.get_webview_window(panes::MAIN) {
        remember_main(&w.as_ref().window());
    }
    persist(app, true);
}

fn remember_main(w: &Window) {
    if w.is_minimized().unwrap_or(false) || w.is_fullscreen().unwrap_or(false) {
        return;
    }
    if let Some(g) = geo_of(w) {
        w.app_handle().state::<PlatformState>().update_mem(|p| p.main_geo = Some(g));
    }
}

pub fn on_window_event(window: &Window, event: &WindowEvent) {
    panes::on_window_event(window, event);
    let app = window.app_handle();
    match (window.label(), event) {
        (panes::MAIN, WindowEvent::Focused(true)) => notify::on_focus_gained(app),
        (panes::MAIN, WindowEvent::Moved(_) | WindowEvent::Resized(_)) => {
            remember_main(window);
            persist(app, false);
        }
        (panes::MAIN, WindowEvent::CloseRequested { .. }) => save_all(app),
        (widget::LABEL, WindowEvent::Moved(_) | WindowEvent::Resized(_)) => {
            if let Some(g) = geo_of(window) {
                app.state::<PlatformState>().update_mem(|p| p.widget_geo = Some(g));
                persist(app, false);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCREEN: Geo = Geo { x: 0.0, y: 0.0, w: 1440.0, h: 900.0 };
    const SECOND: Geo = Geo { x: 1440.0, y: 0.0, w: 1920.0, h: 1080.0 };

    #[test]
    fn on_screen_windows_are_kept() {
        let g = Geo { x: 100.0, y: 80.0, w: 1180.0, h: 760.0 };
        assert_eq!(clamp_visible(g, &[SCREEN]), Some(g));
        let partly = Geo { x: 1300.0, y: 800.0, w: 400.0, h: 300.0 };
        assert_eq!(clamp_visible(partly, &[SCREEN]), Some(partly), "140x100 still visible");
    }

    #[test]
    fn windows_on_a_missing_screen_are_dropped() {
        let on_second = Geo { x: 2000.0, y: 100.0, w: 800.0, h: 600.0 };
        assert_eq!(clamp_visible(on_second, &[SCREEN]), None);
        assert_eq!(clamp_visible(on_second, &[SCREEN, SECOND]), Some(on_second));
        let barely = Geo { x: 1430.0, y: 100.0, w: 800.0, h: 600.0 };
        assert_eq!(clamp_visible(barely, &[SCREEN]), None, "only 10 px visible");
        assert_eq!(clamp_visible(on_second, &[]), None);
    }
}
