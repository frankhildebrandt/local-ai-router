pub mod catalog;
pub mod civitai;
pub mod commands;
pub mod core;
pub mod desktop;
pub mod domain;
pub mod gateway;
pub mod hub;
pub mod install;
pub mod library;
pub mod media;
pub mod model_catalog;
pub mod oauth;
pub mod protocol;
pub mod providers;
pub mod public_models;
pub mod resource;
pub mod routing;
pub mod runtime;
pub mod secrets;
pub mod storage;

use std::{sync::Arc, time::Duration};

use commands::AppServices;
use tauri::{Emitter, Manager, RunEvent, WindowEvent};
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
            let logical_cpus = resource::host_performance_cpu_count();
            let resource_policy =
                tauri::async_runtime::block_on(core.store.resource_policy(logical_cpus))?;
            let core = Arc::new(core);
            let runtimes = Arc::new(runtime::RuntimeManager::new(
                runtime::bundled_bin_dir(&resource_dir),
                app_data.join("kv-cache"),
                resource_policy,
                core.local_activity(),
            ));
            std::fs::create_dir_all(app_data.join("models"))?;
            let install = Arc::new(install::InstallManager::new(
                core.store.clone(),
                hub::hub_http_client()?,
                core.secrets.clone(),
                app_data.join("models"),
                "https://huggingface.co",
            ));
            tauri::async_runtime::block_on(install.interrupt_active())?;
            let shutdown = CancellationToken::new();
            let handle = app.handle().clone();
            let mut events = install.subscribe();
            tauri::async_runtime::spawn(async move {
                while let Ok(event) = events.recv().await {
                    let _ = handle.emit("install-job", event);
                }
            });
            app.manage(AppServices {
                core: core.clone(),
                runtimes: runtimes.clone(),
                model_library: app_data.join("models"),
                port,
                install,
                shutdown: shutdown.clone(),
            });

            let listener =
                tauri::async_runtime::block_on(tokio::net::TcpListener::bind(("127.0.0.1", port)))?;
            let shutdown_server = shutdown.clone();
            let gateway_core = core.clone();
            let gateway_runtimes = runtimes.clone();
            tauri::async_runtime::spawn(async move {
                if let Err(error) = axum::serve(listener, gateway::managed_router(gateway_core, gateway_runtimes))
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
                    for id in maintenance.reap_pending_restarts().await {
                        if let Ok(Some(mut target)) = maintenance_store.target(&id).await {
                            target.runtime_url = None;
                            target.state = "restarting".into();
                            let _ = maintenance_store.upsert_target(&target).await;
                            match maintenance.start(&target).await {
                                Ok(url) => {
                                    target.runtime_url = Some(url);
                                    target.state = "ready".into();
                                }
                                Err(error) => {
                                    tracing::error!(target = %id, %error, "resource-policy restart failed");
                                    target.state = "error".into();
                                }
                            }
                            let _ = maintenance_store.upsert_target(&target).await;
                        }
                    }
                    for id in maintenance.reap_over_budget().await {
                        if let Ok(Some(mut target)) = maintenance_store.target(&id).await {
                            target.runtime_url = None;
                            target.state = "stopped".into();
                            let _ = maintenance_store.upsert_target(&target).await;
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

            desktop::install(app)?;
            Ok(())
        })
        .on_menu_event(|app, event| desktop::handle_menu_event(app, event.id.as_ref()))
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
            commands::client_chat,
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
            commands::lookup_model_metadata,
            commands::delete_target,
            commands::import_local_model,
            commands::download_local_model,
            commands::list_local_catalog,
            commands::search_mlx_catalog,
            commands::inspect_mlx_model,
            commands::install_catalog_model,
            commands::list_install_jobs,
            commands::pause_install_job,
            commands::resume_install_job,
            commands::cancel_install_job,
            commands::clear_install_job,
            commands::start_local_model,
            commands::stop_local_model,
            commands::list_routes,
            commands::list_public_models,
            commands::save_route,
            commands::delete_route,
            commands::list_routing_policies,
            commands::save_routing_policy,
            commands::list_target_routing_profiles,
            commands::save_target_routing_profile,
            commands::list_routing_tasks,
            commands::save_routing_task,
            commands::delete_routing_task,
            commands::simulate_routing,
            commands::list_routing_attempts,
            commands::export_routing_config,
            commands::import_routing_config,
            commands::list_logs,
            commands::get_usage,
            commands::get_log_facets,
            commands::clear_logs,
            commands::export_logs_csv,
            commands::get_settings,
            commands::save_setting,
            commands::get_resource_policy,
            commands::get_resource_profile_preset,
            commands::save_resource_policy,
            commands::save_model_resource_overrides,
            commands::clear_kv_cache,
            commands::save_hugging_face_token,
            commands::save_civitai_token,
            commands::forget_all_credentials
        ])
        .build(tauri::generate_context!())
        .expect("error while running Local AI Router")
        .run(|app, event| match event {
            RunEvent::ExitRequested { .. } => desktop::shutdown(app),
            RunEvent::Reopen { .. } => desktop::show_main_window(app),
            _ => {}
        });
}
