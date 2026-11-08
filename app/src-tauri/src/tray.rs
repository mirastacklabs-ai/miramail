//! Menu-bar item: a template mail glyph + the unread number as the title. Menu: per-account unread, Open MiraMail,
//! Compose, Check now, Quit. Built from the same summary as the Dock badge; created/removed with the setting.

use crate::badge::{tray_title, InboxSummary};
use crate::plat_prefs::PlatformPrefs;
use tauri::image::Image;
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::AppHandle;

pub const ID: &str = "mm-tray";
pub const ACCOUNT_PREFIX: &str = "tray-acct:";
const SIZE: usize = 36;

fn dist_to_segment(p: (f32, f32), a: (f32, f32), b: (f32, f32)) -> f32 {
    let (abx, aby) = (b.0 - a.0, b.1 - a.1);
    let len2 = abx * abx + aby * aby;
    let t = if len2 == 0.0 { 0.0 } else { (((p.0 - a.0) * abx + (p.1 - a.1) * aby) / len2).clamp(0.0, 1.0) };
    let (cx, cy) = (a.0 + t * abx, a.1 + t * aby);
    ((p.0 - cx).powi(2) + (p.1 - cy).powi(2)).sqrt()
}

/// A 36x36 envelope outline, black with alpha (macOS tints template images for light/dark menu bars).
pub fn glyph_rgba() -> Vec<u8> {
    let (l, r, t, b) = (4.0f32, 32.0, 9.0, 27.0);
    let mid = ((l + r) / 2.0, 20.0);
    let segs = [
        ((l, t), (r, t)),
        ((r, t), (r, b)),
        ((r, b), (l, b)),
        ((l, b), (l, t)),
        ((l, t), mid),
        (mid, (r, t)),
    ];
    let mut px = vec![0u8; SIZE * SIZE * 4];
    for y in 0..SIZE {
        for x in 0..SIZE {
            let p = (x as f32 + 0.5, y as f32 + 0.5);
            let d = segs.iter().map(|(a, b)| dist_to_segment(p, *a, *b)).fold(f32::MAX, f32::min);
            let alpha = (1.6 - d).clamp(0.0, 1.0);
            let i = (y * SIZE + x) * 4;
            px[i + 3] = (alpha * 255.0) as u8;
        }
    }
    px
}

fn build_menu(app: &AppHandle, s: &InboxSummary) -> tauri::Result<Menu<tauri::Wry>> {
    let menu = Menu::new(app)?;
    for a in &s.accounts {
        let n = s.per_account.get(a).copied().unwrap_or(0);
        let label = format!("{a}: {n} unread");
        menu.append(&MenuItem::with_id(app, format!("{ACCOUNT_PREFIX}{a}"), label, true, None::<&str>)?)?;
    }
    if !s.accounts.is_empty() {
        menu.append(&PredefinedMenuItem::separator(app)?)?;
    }
    menu.append(&MenuItem::with_id(app, "tray-open", "Open MiraMail", true, None::<&str>)?)?;
    menu.append(&MenuItem::with_id(app, "tray-compose", "Compose", true, None::<&str>)?)?;
    menu.append(&MenuItem::with_id(app, "tray-check", "Check now", true, None::<&str>)?)?;
    menu.append(&PredefinedMenuItem::separator(app)?)?;
    menu.append(&MenuItem::with_id(app, "tray-quit", "Quit MiraMail", true, None::<&str>)?)?;
    Ok(menu)
}

/// Create / update / remove the tray item. Call on the main thread.
pub fn sync(app: &AppHandle, s: &InboxSummary, prefs: &PlatformPrefs) {
    if !prefs.tray_show {
        let _ = app.remove_tray_by_id(ID);
        return;
    }
    let Ok(menu) = build_menu(app, s) else { return };
    let title = tray_title(prefs.tray_style, s.total, s.inbox_total);
    if let Some(t) = app.tray_by_id(ID) {
        let _ = t.set_menu(Some(menu));
        let _ = t.set_title(title);
        return;
    }
    let icon = Image::new_owned(glyph_rgba(), SIZE as u32, SIZE as u32);
    let built = TrayIconBuilder::with_id(ID)
        .icon(icon)
        .icon_as_template(true)
        .tooltip("MiraMail")
        .menu(&menu)
        .show_menu_on_left_click(true)
        .build(app);
    if let Ok(t) = built {
        let _ = t.set_title(title);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glyph_is_a_visible_envelope_on_a_transparent_background() {
        let px = glyph_rgba();
        assert_eq!(px.len(), SIZE * SIZE * 4);
        let opaque = px.chunks(4).filter(|p| p[3] > 128).count();
        assert!(opaque > 100 && opaque < SIZE * SIZE / 2, "outline only, got {opaque}");
        assert_eq!(px[3], 0, "corner is transparent");
        assert!(px.chunks(4).all(|p| p[0] == 0 && p[1] == 0 && p[2] == 0), "template image: black + alpha");
        // the flap's apex is drawn
        assert!(px[(20 * SIZE + 18) * 4 + 3] > 128);
    }
}
