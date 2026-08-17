pub mod commands;
pub mod core;
pub mod domain;
pub mod gateway;
pub mod library;
pub mod oauth;
pub mod protocol;
pub mod providers;
pub mod runtime;
pub mod secrets;
pub mod storage;

use std::{sync::Arc, time::Duration};

use commands::AppServices;
use tauri::{Manager, WindowEvent};
use tokio_util::sync::CancellationToken;

pub fn run() {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(
            tauri_plugin_autostart::Builder::new()
                .macos_launcher(tauri_plugin_autostart::MacosLauncher::LaunchAgent)
                .build(),
        )
        .setup(|app| {
            let app_data = app.path().app_data_dir()?;
            let resource_dir = app.path().resource_dir()?;
            let database = app_data.join("router.sqlite3");
            let core = tauri::async_runtime::block_on(core::AppCore::open(
                &database,
                secrets::shared_keychain(),
            ))?;
            tauri::async_runtime::block_on(core.store.reset_local_runtime_states())?;
            let port = tauri::async_runtime::block_on(core.store.setting("port"))?
                .and_then(|value| value.parse().ok())
                .unwrap_or(11435);
            let budget =
                tauri::async_runtime::block_on(core.store.setting("memory_budget_percent"))?
                    .and_then(|value| value.parse().ok())
                    .unwrap_or(70);
            let idle = tauri::async_runtime::block_on(core.store.setting("idle_unload_minutes"))?
                .and_then(|value| value.parse().ok())
                .unwrap_or(15);
            let core = Arc::new(core);
            let runtimes = Arc::new(runtime::RuntimeManager::new(
                runtime::bundled_bin_dir(&resource_dir),
                budget,
                idle,
                core.local_activity(),
            ));
            std::fs::create_dir_all(app_data.join("models"))?;
            app.manage(AppServices {
                core: core.clone(),
                runtimes: runtimes.clone(),
                model_library: app_data.join("models"),
                port,
            });

            let listener =
                tauri::async_runtime::block_on(tokio::net::TcpListener::bind(("127.0.0.1", port)))?;
            let shutdown = CancellationToken::new();
            let shutdown_server = shutdown.clone();
            let gateway_core = core.clone();
            tauri::async_runtime::spawn(async move {
                if let Err(error) = axum::serve(listener, gateway::router(gateway_core))
                    .with_graceful_shutdown(shutdown_server.cancelled_owned())
                    .await
                {
                    tracing::error!(%error, "gateway stopped");
                }
            });
            let maintenance = runtimes.clone();
            let maintenance_store = core.store.clone();
            tauri::async_runtime::spawn(async move {
                loop {
                    tokio::time::sleep(Duration::from_secs(60)).await;
                    for (id, may_restart) in maintenance.take_crashed() {
                        if let Ok(Some(mut target)) = maintenance_store.target(&id).await {
                            target.runtime_url = None;
                            target.state = if may_restart { "restarting" } else { "error" }.into();
                            let _ = maintenance_store.upsert_target(&target).await;
                            if may_restart {
                                match maintenance.start(&target).await {
                                    Ok(url) => {
                                        target.runtime_url = Some(url);
                                        target.state = "ready".into();
                                    }
                                    Err(_) => {
                                        target.state = "error".into();
                                    }
                                }
                                let _ = maintenance_store.upsert_target(&target).await;
                            }
                        }
                    }
                    for id in maintenance.reap_idle().await {
                        if let Ok(Some(mut target)) = maintenance_store.target(&id).await {
                            target.runtime_url = None;
                            target.state = "stopped".into();
                            let _ = maintenance_store.upsert_target(&target).await;
                        }
                    }
                    let retention = maintenance_store
                        .setting("log_retention_days")
                        .await
                        .ok()
                        .flatten()
                        .and_then(|value| value.parse().ok())
                        .unwrap_or(30);
                    let _ = maintenance_store.purge_old_logs(retention).await;
                }
            });

            let show = tauri::menu::MenuItem::with_id(
                app,
                "show",
                "Open Local AI Router",
                true,
                None::<&str>,
            )?;
            let quit = tauri::menu::MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
            let menu = tauri::menu::Menu::with_items(app, &[&show, &quit])?;
            let mut tray = tauri::tray::TrayIconBuilder::new()
                .menu(&menu)
                .show_menu_on_left_click(false)
                .on_menu_event({
                    let runtimes = runtimes.clone();
                    move |app, event| match event.id.as_ref() {
                        "show" => {
                            if let Some(window) = app.get_webview_window("main") {
                                let _ = window.show();
                                let _ = window.set_focus();
                            }
                        }
                        "quit" => {
                            shutdown.cancel();
                            tauri::async_runtime::block_on(runtimes.stop_all());
                            app.exit(0);
                        }
                        _ => {}
                    }
                });
            if let Some(icon) = app.default_window_icon() {
                tray = tray.icon(icon.clone());
            }
            tray.build(app)?;
            Ok(())
        })
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = window.hide();
            }
        })
        .invoke_handler(tauri::generate_handler![
            commands::dashboard,
            commands::list_local_api_keys,
            commands::create_local_api_key,
            commands::reveal_local_api_key,
            commands::rename_local_api_key,
            commands::rotate_local_api_key,
            commands::revoke_local_api_key,
            commands::list_providers,
            commands::list_provider_presets,
            commands::save_provider,
            commands::delete_provider,
            commands::sync_provider_models,
            commands::cached_provider_models,
            commands::begin_openai_subscription,
            commands::openai_subscription_status,
            commands::logout_openai_subscription,
            commands::test_provider_connection,
            commands::list_targets,
            commands::save_target,
            commands::delete_target,
            commands::import_local_model,
            commands::download_local_model,
            commands::start_local_model,
            commands::stop_local_model,
            commands::list_routes,
            commands::save_route,
            commands::delete_route,
            commands::list_logs,
            commands::get_usage,
            commands::get_log_facets,
            commands::clear_logs,
            commands::export_logs_csv,
            commands::get_settings,
            commands::save_setting,
            commands::save_hugging_face_token,
            commands::forget_all_credentials
        ])
        .run(tauri::generate_context!())
        .expect("error while running Local AI Router");
}
