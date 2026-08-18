use tauri::menu::{AboutMetadata, Menu, MenuItem, PredefinedMenuItem, Submenu};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{App, AppHandle, Emitter, Manager, Runtime, WebviewWindow};
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons, MessageDialogKind};
use tauri_plugin_opener::OpenerExt;
use tauri_plugin_updater::UpdaterExt;

use crate::commands::AppServices;

const GITHUB_URL: &str = "https://github.com/frankhildebrandt/local-ai-router";

pub fn install<R: Runtime>(app: &mut App<R>) -> tauri::Result<()> {
    app.set_menu(app_menu(app)?)?;
    install_tray(app)?;
    Ok(())
}

pub fn show_main_window<R: Runtime>(app: &AppHandle<R>) {
    if let Some(window) = main_window(app) {
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
    }
}

pub fn hide_main_window<R: Runtime>(app: &AppHandle<R>) {
    if let Some(window) = main_window(app) {
        let _ = window.hide();
    }
}

pub fn shutdown<R: Runtime>(app: &AppHandle<R>) {
    if let Some(services) = app.try_state::<AppServices>() {
        services.shutdown.cancel();
        tauri::async_runtime::block_on(services.runtimes.stop_all());
    }
}

pub fn handle_menu_event<R: Runtime>(app: &AppHandle<R>, id: &str) {
    match id {
        "settings" => open_page(app, "settings"),
        "check-updates" => check_for_updates(app.clone()),
        "help-github" => {
            if let Err(error) = app.opener().open_url(GITHUB_URL, None::<&str>) {
                tracing::warn!(%error, "failed to open GitHub");
            }
        }
        "tray-show" => show_main_window(app),
        "tray-hide" => hide_main_window(app),
        "tray-quit" => app.exit(0),
        id if id.starts_with("nav-") => open_page(app, &id[4..]),
        _ => {}
    }
}

fn main_window<R: Runtime>(app: &AppHandle<R>) -> Option<WebviewWindow<R>> {
    app.get_webview_window("main")
}

fn toggle_main_window<R: Runtime>(app: &AppHandle<R>) {
    let Some(window) = main_window(app) else {
        return;
    };
    let visible = window.is_visible().unwrap_or(false);
    let focused = window.is_focused().unwrap_or(false);
    if visible && focused {
        let _ = window.hide();
    } else {
        show_main_window(app);
    }
}

fn open_page<R: Runtime>(app: &AppHandle<R>, page: &str) {
    let _ = app.emit("desktop-navigate", page);
    show_main_window(app);
}

fn app_menu<R: Runtime>(app: &App<R>) -> tauri::Result<Menu<R>> {
    let about = AboutMetadata {
        name: Some("Local AI Router".into()),
        version: Some(app.package_info().version.to_string()),
        ..Default::default()
    };
    let check_updates = MenuItem::with_id(
        app,
        "check-updates",
        "Check for Updates…",
        true,
        None::<&str>,
    )?;
    let settings = MenuItem::with_id(app, "settings", "Settings…", true, Some("CmdOrCtrl+,"))?;
    let overview = MenuItem::with_id(app, "nav-overview", "Overview", true, None::<&str>)?;
    let chat = MenuItem::with_id(app, "nav-chat", "Chat", true, None::<&str>)?;
    let usage = MenuItem::with_id(app, "nav-usage", "Usage", true, None::<&str>)?;
    let providers = MenuItem::with_id(app, "nav-providers", "Providers", true, None::<&str>)?;
    let cloud = MenuItem::with_id(app, "nav-cloud", "Cloud models", true, None::<&str>)?;
    let local = MenuItem::with_id(app, "nav-local", "Local models", true, None::<&str>)?;
    let routes = MenuItem::with_id(app, "nav-routes", "Custom routes", true, None::<&str>)?;
    let logs = MenuItem::with_id(app, "nav-logs", "Request logs", true, None::<&str>)?;
    let view_settings = MenuItem::with_id(app, "nav-settings", "Settings", true, None::<&str>)?;
    let github = MenuItem::with_id(
        app,
        "help-github",
        "Local AI Router on GitHub",
        true,
        None::<&str>,
    )?;

    Menu::with_items(
        app,
        &[
            &Submenu::with_items(
                app,
                "Local AI Router",
                true,
                &[
                    &PredefinedMenuItem::about(app, Some("About Local AI Router"), Some(about))?,
                    &PredefinedMenuItem::separator(app)?,
                    &check_updates,
                    &PredefinedMenuItem::separator(app)?,
                    &settings,
                    &PredefinedMenuItem::separator(app)?,
                    &PredefinedMenuItem::services(app, None)?,
                    &PredefinedMenuItem::separator(app)?,
                    &PredefinedMenuItem::hide(app, None)?,
                    &PredefinedMenuItem::hide_others(app, None)?,
                    &PredefinedMenuItem::show_all(app, None)?,
                    &PredefinedMenuItem::separator(app)?,
                    &PredefinedMenuItem::quit(app, None)?,
                ],
            )?,
            &Submenu::with_items(
                app,
                "Edit",
                true,
                &[
                    &PredefinedMenuItem::undo(app, None)?,
                    &PredefinedMenuItem::redo(app, None)?,
                    &PredefinedMenuItem::separator(app)?,
                    &PredefinedMenuItem::cut(app, None)?,
                    &PredefinedMenuItem::copy(app, None)?,
                    &PredefinedMenuItem::paste(app, None)?,
                    &PredefinedMenuItem::select_all(app, None)?,
                ],
            )?,
            &Submenu::with_items(
                app,
                "View",
                true,
                &[
                    &overview,
                    &chat,
                    &usage,
                    &providers,
                    &cloud,
                    &local,
                    &routes,
                    &logs,
                    &view_settings,
                ],
            )?,
            &Submenu::with_items(
                app,
                "Window",
                true,
                &[
                    &PredefinedMenuItem::minimize(app, None)?,
                    &PredefinedMenuItem::close_window(app, None)?,
                ],
            )?,
            &Submenu::with_items(app, "Help", true, &[&github])?,
        ],
    )
}

fn install_tray<R: Runtime>(app: &mut App<R>) -> tauri::Result<()> {
    let show = MenuItem::with_id(app, "tray-show", "Open Local AI Router", true, None::<&str>)?;
    let hide = MenuItem::with_id(app, "tray-hide", "Hide Local AI Router", true, None::<&str>)?;
    let settings = MenuItem::with_id(app, "settings", "Settings…", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "tray-quit", "Quit Local AI Router", true, None::<&str>)?;
    let menu = Menu::with_items(
        app,
        &[
            &show,
            &hide,
            &PredefinedMenuItem::separator(app)?,
            &settings,
            &PredefinedMenuItem::separator(app)?,
            &quit,
        ],
    )?;
    let mut tray = TrayIconBuilder::new()
        .menu(&menu)
        .tooltip("Local AI Router")
        .icon_as_template(true)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| handle_menu_event(app, event.id.as_ref()))
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                toggle_main_window(tray.app_handle());
            }
        });
    if let Some(icon) = app.default_window_icon() {
        tray = tray.icon(icon.clone());
    }
    tray.build(app)?;
    Ok(())
}

fn check_for_updates<R: Runtime>(app: AppHandle<R>) {
    tauri::async_runtime::spawn(async move {
        let result = async {
            let updater = app.updater()?;
            updater.check().await
        }
        .await;
        match result {
            Ok(Some(update)) => {
                let version = update.version.clone();
                app.dialog()
                    .message(format!("Version {version} is available. Install now?"))
                    .title("Update available")
                    .kind(MessageDialogKind::Info)
                    .buttons(MessageDialogButtons::OkCancelCustom(
                        "Install".into(),
                        "Later".into(),
                    ))
                    .show(move |accepted| {
                        if !accepted {
                            return;
                        }
                        let app = app.clone();
                        tauri::async_runtime::spawn(async move {
                            match update.download_and_install(|_, _| {}, || {}).await {
                                Ok(()) => app.restart(),
                                Err(error) => show_message(
                                    &app,
                                    MessageDialogKind::Error,
                                    "Update failed",
                                    error.to_string(),
                                ),
                            }
                        });
                    });
            }
            Ok(None) => show_message(
                &app,
                MessageDialogKind::Info,
                "Local AI Router",
                "You're up to date.",
            ),
            Err(error) => show_message(
                &app,
                MessageDialogKind::Error,
                "Update check failed",
                error.to_string(),
            ),
        }
    });
}

fn show_message<R: Runtime>(
    app: &AppHandle<R>,
    kind: MessageDialogKind,
    title: &str,
    message: impl Into<String>,
) {
    app.dialog()
        .message(message)
        .title(title)
        .kind(kind)
        .show(|_| {});
}
