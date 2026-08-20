pub mod admin;
pub mod catalog;
pub mod civitai;
pub mod commands;
pub mod core;
pub mod desktop;
pub mod domain;
pub mod engine;
pub mod gateway;
pub mod hub;
pub mod identity;
pub mod install;
mod ipc;
pub mod library;
pub mod media;
pub mod model_catalog;
pub mod oauth;
pub mod oidc;
pub mod protocol;
pub mod providers;
pub mod public_models;
pub mod resource;
pub mod routing;
pub mod runtime;
pub mod secrets;
pub mod speculative;
pub mod storage;
pub mod tls;
pub mod tool_emulation;
pub mod uplink;

use engine::{Engine, EngineConfig};
use tauri::{Emitter, Manager, RunEvent, WindowEvent};

pub fn serve_headless<I, S>(args: I) -> anyhow::Result<()>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    engine::serve_headless(args)
}

pub fn run() {
    #[cfg(target_os = "linux")]
    if std::env::var_os("WEBKIT_DISABLE_DMABUF_RENDERER").is_none() {
        // NVIDIA/Wayland DMA-BUF often leaves the WebView blank; users can override with 0.
        unsafe {
            std::env::set_var("WEBKIT_DISABLE_DMABUF_RENDERER", "1");
        }
    }
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
            let bundled_ui = resource_dir.join("ui");
            let ui_dir = if bundled_ui.join("index.html").is_file() {
                Some(bundled_ui)
            } else {
                engine::default_ui_dir()
            };
            let engine = tauri::async_runtime::block_on(Engine::open(EngineConfig {
                data_dir: app_data,
                resource_dir: resource_dir.clone(),
                ui_dir,
                port: None,
                secrets: secrets::shared_keychain(),
            }))?;
            engine.spawn_maintenance();
            let services = engine.services.clone();
            let handle = app.handle().clone();
            let mut events = services.install.subscribe();
            tauri::async_runtime::spawn(async move {
                while let Ok(event) = events.recv().await {
                    let _ = handle.emit("install-job", event);
                }
            });
            let traffic_hub = services.core.traffic.clone();
            let traffic_handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                let mut events = traffic_hub.subscribe();
                loop {
                    match events.recv().await {
                        Ok(event) => {
                            let _ = traffic_handle.emit("gateway-traffic", event);
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                            let _ = traffic_handle.emit("gateway-traffic", traffic_hub.snapshot());
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                    }
                }
            });
            let listener = tauri::async_runtime::block_on(tokio::net::TcpListener::bind((
                engine.bind_ip(),
                services.port,
            )))?;
            let shutdown_server = services.shutdown.clone();
            let router = engine.router();
            let tls = engine.tls_config();
            tauri::async_runtime::spawn(async move {
                if let Err(error) =
                    crate::engine::serve_gateway(listener, tls, router, shutdown_server).await
                {
                    tracing::error!(%error, "gateway stopped");
                }
            });
            app.manage(services);
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
            ipc::dashboard,
            ipc::cancel_inflight_request,
            ipc::cancel_all_inflight_requests,
            ipc::list_local_api_keys,
            ipc::create_local_api_key,
            ipc::reveal_local_api_key,
            ipc::rename_local_api_key,
            ipc::rotate_local_api_key,
            ipc::revoke_local_api_key,
            ipc::client_chat,
            ipc::list_providers,
            ipc::list_provider_presets,
            ipc::save_provider,
            ipc::delete_provider,
            ipc::sync_provider_models,
            ipc::cached_provider_models,
            ipc::begin_openai_subscription,
            ipc::openai_subscription_status,
            ipc::logout_openai_subscription,
            ipc::test_provider_connection,
            ipc::list_targets,
            ipc::save_target,
            ipc::lookup_model_metadata,
            ipc::delete_target,
            ipc::import_local_model,
            ipc::download_local_model,
            ipc::list_local_catalog,
            ipc::search_mlx_catalog,
            ipc::inspect_mlx_model,
            ipc::install_catalog_model,
            ipc::list_install_jobs,
            ipc::pause_install_job,
            ipc::resume_install_job,
            ipc::cancel_install_job,
            ipc::clear_install_job,
            ipc::start_local_model,
            ipc::stop_local_model,
            ipc::list_routes,
            ipc::list_public_models,
            ipc::save_route,
            ipc::delete_route,
            ipc::list_routing_policies,
            ipc::save_routing_policy,
            ipc::list_target_routing_profiles,
            ipc::save_target_routing_profile,
            ipc::list_routing_tasks,
            ipc::save_routing_task,
            ipc::delete_routing_task,
            ipc::simulate_routing,
            ipc::list_routing_attempts,
            ipc::export_routing_config,
            ipc::import_routing_config,
            ipc::list_logs,
            ipc::get_usage,
            ipc::get_key_usage,
            ipc::get_log_facets,
            ipc::clear_logs,
            ipc::export_logs_csv,
            ipc::get_settings,
            ipc::save_setting,
            ipc::get_resource_policy,
            ipc::get_resource_profile_preset,
            ipc::save_resource_policy,
            ipc::save_model_resource_overrides,
            ipc::save_model_speculative_config,
            ipc::clear_kv_cache,
            ipc::save_hugging_face_token,
            ipc::save_civitai_token,
            ipc::forget_all_credentials,
            ipc::auth_status,
            ipc::login,
            ipc::list_directory_users,
            ipc::create_directory_user,
            ipc::update_directory_user,
            ipc::list_directory_groups,
            ipc::save_directory_group,
            ipc::delete_directory_group,
            ipc::user_permissions,
            ipc::join_uplink,
            ipc::uplink_status,
            ipc::disconnect_uplink,
            ipc::reveal_operator_bootstrap,
            ipc::list_oidc_allowlist,
            ipc::invite_oidc_identity,
            ipc::delete_oidc_allowlist,
            ipc::save_oidc_client,
            ipc::begin_oidc_login
        ])
        .build(tauri::generate_context!())
        .expect("error while running Local AI Router")
        .run(|app, event| match event {
            RunEvent::ExitRequested { .. } => desktop::shutdown(app),
            RunEvent::Reopen { .. } => desktop::show_main_window(app),
            _ => {}
        });
}
