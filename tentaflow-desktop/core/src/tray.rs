// =============================================================================
// Plik: tray.rs
// Opis: Modul tray icon — tworzenie ikony w zasobniku systemowym, menu
//       kontekstowe z dynamicznym statusem mesh i modelu.
// =============================================================================

use anyhow::Result;
use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder};
use tentaflow_ui::state::SharedAppState;

/// Identyfikatory pozycji menu
pub struct MenuIds {
    pub open_gui: MenuItem,
    pub status: MenuItem,
    pub model: MenuItem,
    pub dashboard: MenuItem,
    pub settings: MenuItem,
    pub quit: MenuItem,
}

/// Wrapper na tray icon z referencjami do elementow menu
pub struct AppTray {
    pub tray_icon: TrayIcon,
    pub menu_ids: MenuIds,
    _state: SharedAppState,
}

/// Generuje ikone 32x32 RGBA — niebieski gradient w ksztalcie kola
fn create_app_icon() -> Result<Icon> {
    let width: u32 = 32;
    let height: u32 = 32;
    let mut rgba = Vec::with_capacity((width * height * 4) as usize);
    let cx = width as f32 / 2.0;
    let cy = height as f32 / 2.0;
    let radius = cx - 1.0;

    for y in 0..height {
        for x in 0..width {
            let dx = x as f32 - cx;
            let dy = y as f32 - cy;
            let dist = (dx * dx + dy * dy).sqrt();

            if dist <= radius {
                // Blue gradient from center
                let t = dist / radius;
                let r = (0x1E as f32 * (1.0 - t) + 0x3B as f32 * t) as u8;
                let g = (0x40 as f32 * (1.0 - t) + 0x82 as f32 * t) as u8;
                let b = (0xE0 as f32 * (1.0 - t) + 0xF6 as f32 * t) as u8;
                rgba.extend_from_slice(&[r, g, b, 0xFF]);
            } else {
                rgba.extend_from_slice(&[0, 0, 0, 0]); // transparent
            }
        }
    }

    Icon::from_rgba(rgba, width, height)
        .map_err(|e| anyhow::anyhow!("Blad tworzenia ikony: {}", e))
}

/// Tworzy tray icon z menu kontekstowym
pub fn create_tray(state: SharedAppState) -> Result<AppTray> {
    let menu = Menu::new();

    // Odczytaj aktualny stan
    let (peer_count, model_name, mesh_connected) = {
        let s = state.read().unwrap_or_else(|e| e.into_inner());
        let peers = s.online_peer_count();
        let model = s
            .local_models
            .first()
            .filter(|m| m.loaded)
            .map(|m| m.name.clone())
            .unwrap_or_else(|| "brak".to_string());
        let connected = s.mesh_connected;
        (peers, model, connected)
    };

    // Pozycje menu
    let open_gui = MenuItem::new("Otworz TentaFlow", true, None);
    let status_text = if mesh_connected {
        format!("Status: Polaczony ({} peerow)", peer_count)
    } else {
        "Status: Rozlaczony".to_string()
    };
    let status = MenuItem::new(status_text, false, None);
    let model = MenuItem::new(format!("Model: {}", model_name), false, None);
    let dashboard = MenuItem::new("Dashboard", true, None);
    let settings = MenuItem::new("Ustawienia...", true, None);
    let quit = MenuItem::new("Zakoncz", true, None);

    menu.append(&open_gui)?;
    menu.append(&PredefinedMenuItem::separator())?;
    menu.append(&status)?;
    menu.append(&model)?;
    menu.append(&PredefinedMenuItem::separator())?;
    menu.append(&dashboard)?;
    menu.append(&settings)?;
    menu.append(&PredefinedMenuItem::separator())?;
    menu.append(&quit)?;

    let icon = create_app_icon()?;

    let tray_icon = TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_icon(icon)
        .with_tooltip("TentaFlow Desktop")
        .build()
        .map_err(|e| anyhow::anyhow!("Blad tworzenia tray icon: {}", e))?;

    let menu_ids = MenuIds {
        open_gui,
        status,
        model,
        dashboard,
        settings,
        quit,
    };

    Ok(AppTray {
        tray_icon,
        menu_ids,
        _state: state,
    })
}

/// Aktualizuje teksty menu na podstawie aktualnego stanu
pub fn update_menu_status(app_tray: &AppTray, state: &SharedAppState) {
    let s = state.read().unwrap_or_else(|e| e.into_inner());
    let peers = s.online_peer_count();
    let connected = s.mesh_connected;

    let model_name = s
        .local_models
        .first()
        .filter(|m| m.loaded)
        .map(|m| m.name.clone())
        .unwrap_or_else(|| "brak".to_string());
    drop(s);

    let status_text = if connected {
        format!("Status: Polaczony ({} peerow)", peers)
    } else {
        "Status: Rozlaczony".to_string()
    };

    app_tray.menu_ids.status.set_text(status_text);
    app_tray.menu_ids.model.set_text(format!("Model: {}", model_name));
}

/// Obsluguje zdarzenia menu — zwraca akcje do wykonania
pub enum TrayAction {
    OpenGui,
    OpenDashboard,
    OpenSettings,
    Quit,
    None,
}

/// Przetwarza zdarzenie menu i zwraca odpowiednia akcje
pub fn handle_menu_event(event: &MenuEvent, menu_ids: &MenuIds) -> TrayAction {
    let id = event.id();

    if id == menu_ids.open_gui.id() {
        TrayAction::OpenGui
    } else if id == menu_ids.dashboard.id() {
        TrayAction::OpenDashboard
    } else if id == menu_ids.settings.id() {
        TrayAction::OpenSettings
    } else if id == menu_ids.quit.id() {
        TrayAction::Quit
    } else {
        TrayAction::None
    }
}
