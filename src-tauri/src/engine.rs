use std::{
    net::{IpAddr, SocketAddr},
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use anyhow::Context;
use axum::Router;
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;

use crate::{
    commands::AppServices,
    core::AppCore,
    gateway, hub, install, resource, runtime,
    secrets::{file_secrets, shared_keychain, SecretStore},
    tls,
};

#[cfg(test)]
use crate::secrets::MemorySecrets;

const DEFAULT_PORT: u16 = 11435;
const BUNDLE_ID: &str = "app.local-ai-router.desktop";

#[derive(Debug, Clone, Default)]
pub struct ServeArgs {
    pub port: Option<u16>,
    pub data_dir: Option<PathBuf>,
    pub ui_dir: Option<PathBuf>,
    pub secrets_file: Option<PathBuf>,
    pub help: bool,
}

pub struct Engine {
    pub services: Arc<AppServices>,
    ui_dir: Option<PathBuf>,
    tls: Option<Arc<rustls::ServerConfig>>,
}

pub struct EngineConfig {
    pub data_dir: PathBuf,
    pub resource_dir: PathBuf,
    pub ui_dir: Option<PathBuf>,
    pub port: Option<u16>,
    pub secrets: Arc<dyn SecretStore>,
}

impl Engine {
    pub async fn open(config: EngineConfig) -> anyhow::Result<Self> {
        std::fs::create_dir_all(&config.data_dir)
            .with_context(|| format!("creating data directory {}", config.data_dir.display()))?;
        let models = config.data_dir.join("models");
        std::fs::create_dir_all(&models)?;
        let core = AppCore::open(&config.data_dir.join("router.sqlite3"), config.secrets).await?;
        core.store.reset_local_runtime_states().await?;
        let port = match config.port {
            Some(port) => port,
            None => core
                .store
                .setting("port")
                .await?
                .and_then(|value| value.parse().ok())
                .unwrap_or(DEFAULT_PORT),
        };
        let logical_cpus = resource::host_performance_cpu_count();
        let resource_policy = core.store.resource_policy(logical_cpus).await?;
        let core = Arc::new(core);
        let runtimes = Arc::new(runtime::RuntimeManager::new(
            runtime::bundled_bin_dir(&config.resource_dir),
            config.data_dir.join("kv-cache"),
            resource_policy,
            core.local_activity(),
        ));
        let install = Arc::new(install::InstallManager::new(
            core.store.clone(),
            hub::hub_http_client()?,
            core.secrets.clone(),
            models.clone(),
            "https://huggingface.co",
        ));
        install.interrupt_active().await?;
        let bind_mode = core
            .store
            .setting("bind_mode")
            .await?
            .unwrap_or_else(|| "loopback".into());
        let bind_address = core.store.setting("bind_address").await?;
        let bind = tls::bind_config(&bind_mode, bind_address.as_deref())?;
        let cert_path = core.store.setting("tls_cert_path").await?;
        let key_path = core.store.setting("tls_key_path").await?;
        let tls_material = if bind.tls_required {
            let _ = rustls::crypto::ring::default_provider().install_default();
            let paths = tls::user_cert_paths(cert_path.as_deref(), key_path.as_deref());
            Some(
                tls::resolve_tls_material_for(
                    &config.data_dir,
                    paths.as_ref().map(|(cert, _)| cert.as_path()),
                    paths.as_ref().map(|(_, key)| key.as_path()),
                    if bind.ip.is_unspecified() || bind.ip.is_loopback() {
                        &[] as &[IpAddr]
                    } else {
                        std::slice::from_ref(&bind.ip)
                    },
                )
                .context("non-loopback bind requires HTTPS certificate material")?,
            )
        } else {
            None
        };
        let tls_fingerprint = tls_material.as_ref().map(|material| material.fingerprint.clone());
        let tls = tls_material
            .as_ref()
            .map(tls::TlsMaterial::server_config)
            .transpose()?
            .map(Arc::new);
        let oidc = Arc::new(crate::oidc::OidcManager::new(
            core.client.clone(),
            core.secrets.clone(),
        ));
        Ok(Self {
            services: Arc::new(AppServices {
                core,
                runtimes,
                model_library: models,
                port,
                bind_ip: bind.ip,
                tls_required: bind.tls_required,
                tls_fingerprint,
                oidc,
                install,
                shutdown: CancellationToken::new(),
            }),
            ui_dir: config.ui_dir,
            tls,
        })
    }

    pub fn port(&self) -> u16 {
        self.services.port
    }

    pub fn bind_ip(&self) -> IpAddr {
        self.services.bind_ip
    }

    pub fn tls_config(&self) -> Option<Arc<rustls::ServerConfig>> {
        self.tls.clone()
    }

    pub fn router(&self) -> Router {
        hosted_router(self.services.clone(), self.ui_dir.clone())
    }

    pub fn spawn_maintenance(&self) {
        spawn_maintenance(self.services.clone());
    }
}

pub fn hosted_router(services: Arc<AppServices>, ui_dir: Option<PathBuf>) -> Router {
    let ui = ui_dir.clone();
    gateway::inference_router(services.core.clone(), Some(services.runtimes.clone()))
        .merge(crate::admin::router(services))
        .fallback(move |uri: axum::http::Uri| {
            let ui = ui.clone();
            async move { crate::admin::fallback(uri, ui).await }
        })
}

pub fn spawn_maintenance(services: Arc<AppServices>) {
    let maintenance = services.runtimes.clone();
    let maintenance_store = services.core.store.clone();
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(60)).await;
            for (id, may_restart) in maintenance.take_crashed() {
                if let Ok(Some(mut target)) = maintenance_store.target(&id).await {
                    target.runtime_url = None;
                    target.state = if may_restart { "restarting" } else { "error" }.into();
                    let _ = maintenance_store.upsert_target(&target).await;
                    if may_restart {
                        match maintenance
                            .start_resolved(&maintenance_store, &target)
                            .await
                        {
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
                    match maintenance
                        .start_resolved(&maintenance_store, &target)
                        .await
                    {
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
}

pub fn default_data_dir() -> PathBuf {
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    if cfg!(target_os = "macos") {
        home.join("Library/Application Support").join(BUNDLE_ID)
    } else if cfg!(windows) {
        std::env::var_os("APPDATA")
            .map(PathBuf::from)
            .unwrap_or(home)
            .join(BUNDLE_ID)
    } else {
        std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".local/share"))
            .join(BUNDLE_ID)
    }
}

pub fn default_ui_dir() -> Option<PathBuf> {
    let resource_ui = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(resource_dir_from_exe_dir))
        .and_then(|dir| ui_dir_from_resource_dir(&dir));
    let candidates = [
        std::env::current_dir().ok().map(|dir| dir.join("dist")),
        resource_ui,
        std::env::current_exe()
            .ok()
            .and_then(|exe| exe.parent().map(|dir| dir.join("ui"))),
        Some(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../dist")),
        Some(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("ui")),
    ];
    candidates
        .into_iter()
        .flatten()
        .map(|path| path.canonicalize().unwrap_or(path))
        .find(|path| path.join("index.html").is_file())
}

pub(crate) fn resource_dir_from_exe_dir(exe_dir: &Path) -> PathBuf {
    let macos = exe_dir.join("../Resources");
    if macos.is_dir() {
        return macos;
    }
    for name in ["local-ai-router", BUNDLE_ID] {
        let linux = exe_dir.join("../lib").join(name);
        if linux.is_dir() {
            return linux;
        }
    }
    // Windows NSIS/MSI and the headless zip place ui/ and sidecars/ next to the exe.
    exe_dir.to_path_buf()
}

fn ui_dir_from_resource_dir(resource_dir: &Path) -> Option<PathBuf> {
    let ui = resource_dir.join("ui");
    ui.join("index.html").is_file().then_some(ui)
}

fn resource_dir_from_exe() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(Path::to_path_buf))
        .map(|dir| resource_dir_from_exe_dir(&dir))
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")))
}

pub fn parse_serve_args<I, S>(args: I) -> anyhow::Result<ServeArgs>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut parsed = ServeArgs::default();
    let mut items = args.into_iter().peekable();
    while let Some(arg) = items.next() {
        match arg.as_ref() {
            "-h" | "--help" | "help" => parsed.help = true,
            "--port" => {
                parsed.port = Some(
                    items
                        .next()
                        .context("--port requires a value")?
                        .as_ref()
                        .parse()
                        .context("invalid --port")?,
                );
            }
            "--data-dir" => {
                parsed.data_dir = Some(PathBuf::from(
                    items.next().context("--data-dir requires a path")?.as_ref(),
                ));
            }
            "--ui-dir" => {
                parsed.ui_dir = Some(PathBuf::from(
                    items.next().context("--ui-dir requires a path")?.as_ref(),
                ));
            }
            "--secrets-file" => {
                parsed.secrets_file = Some(PathBuf::from(
                    items
                        .next()
                        .context("--secrets-file requires a path")?
                        .as_ref(),
                ));
            }
            flag if flag.starts_with("--port=") => {
                parsed.port = Some(flag.trim_start_matches("--port=").parse()?);
            }
            other => anyhow::bail!("unknown argument: {other}"),
        }
    }
    Ok(parsed)
}

pub fn serve_help() -> String {
    let data = default_data_dir_help();
    let secrets = default_secrets_help();
    format!(
        "Start Local AI Router without a desktop window or tray icon.

Usage:
  local-ai-router serve [options]

Options:
  --port <port>            Listen port (default: saved setting or 11435)
  --data-dir <path>        SQLite, models and KV cache directory
  --ui-dir <path>          Built admin SPA directory (contains index.html)
  --secrets-file <path>    Store secrets in a 0600 JSON vault instead of the platform keyring
  -h, --help               Show this help

Defaults match the desktop app:
  Data:  {data}
  Bind:  127.0.0.1 HTTP (opt-in LAN HTTPS from Settings)
  Secrets: {secrets}
"
    )
}

fn default_data_dir_help() -> &'static str {
    if cfg!(target_os = "macos") {
        "~/Library/Application Support/app.local-ai-router.desktop"
    } else if cfg!(windows) {
        r"%APPDATA%\app.local-ai-router.desktop"
    } else {
        "$XDG_DATA_HOME/app.local-ai-router.desktop (default ~/.local/share/...)"
    }
}

fn default_secrets_help() -> &'static str {
    if cfg!(target_os = "macos") {
        "macOS Keychain service app.local-ai-router.desktop"
    } else if cfg!(target_os = "linux") {
        "Secret Service (GNOME Keyring/KWallet), or --secrets-file for headless/systemd"
    } else if cfg!(windows) {
        "Windows Credential Manager, or --secrets-file for an isolated JSON vault"
    } else {
        "the platform keyring, or --secrets-file for an isolated 0600 JSON vault"
    }
}

pub fn serve_headless<I, S>(args: I) -> anyhow::Result<()>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let parsed = parse_serve_args(args)?;
    if parsed.help {
        print!("{}", serve_help());
        return Ok(());
    }
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();
    let data_dir = parsed.data_dir.unwrap_or_else(default_data_dir);
    let ui_dir = parsed.ui_dir.or_else(default_ui_dir);
    let secrets = match parsed.secrets_file {
        Some(path) => file_secrets(path),
        None => shared_keychain(),
    };
    let runtime = tokio::runtime::Runtime::new()?;
    runtime.block_on(async move {
        let engine = Engine::open(EngineConfig {
            resource_dir: resource_dir_from_exe(),
            data_dir,
            ui_dir,
            port: parsed.port,
            secrets,
        })
        .await?;
        engine.spawn_maintenance();
        let addr = SocketAddr::from((engine.bind_ip(), engine.port()));
        let listener = TcpListener::bind(addr)
            .await
            .with_context(|| format!("binding {addr}"))?;
        let bound = listener.local_addr()?;
        let scheme = if engine.services.tls_required {
            "https"
        } else {
            "http"
        };
        tracing::info!(%bound, %scheme, "headless gateway listening");
        println!("Local AI Router (headless) {scheme}://{bound}");
        println!("Admin UI {scheme}://{bound}/");
        if let Some(fingerprint) = &engine.services.tls_fingerprint {
            println!("TLS fingerprint (SHA-256) {fingerprint}");
        }
        serve_gateway(listener, engine.tls_config(), engine.router(), engine.services.shutdown.clone())
            .await?;
        engine.services.runtimes.stop_all().await;
        Ok(())
    })
}

async fn shutdown_signal(token: CancellationToken) {
    let ctrl_c = tokio::signal::ctrl_c();
    tokio::select! {
        _ = ctrl_c => {}
        _ = token.cancelled() => {}
    }
}

pub async fn serve_gateway(
    listener: TcpListener,
    tls: Option<Arc<rustls::ServerConfig>>,
    router: Router,
    shutdown: CancellationToken,
) -> anyhow::Result<()> {
    let make_service = router.into_make_service();
    if let Some(tls) = tls {
        axum::serve(tls::TlsListener::new(listener, tls), make_service)
            .with_graceful_shutdown(shutdown_signal(shutdown))
            .await?;
    } else {
        axum::serve(listener, make_service)
            .with_graceful_shutdown(shutdown_signal(shutdown))
            .await?;
    }
    Ok(())
}

#[cfg(test)]
pub(crate) async fn test_engine(data_dir: &Path, ui_dir: Option<PathBuf>) -> Engine {
    Engine::open(EngineConfig {
        data_dir: data_dir.to_path_buf(),
        resource_dir: PathBuf::from(env!("CARGO_MANIFEST_DIR")),
        ui_dir,
        port: Some(0),
        secrets: Arc::new(MemorySecrets::default()),
    })
    .await
    .unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::Body,
        http::{header, Request, StatusCode},
    };
    use http_body_util::BodyExt;
    use serde_json::{json, Value};
    use tower::ServiceExt;

    fn write_spa(dir: &Path) {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(
            dir.join("index.html"),
            "<!doctype html><title>Local AI Router</title><h1>Admin</h1>",
        )
        .unwrap();
        std::fs::write(dir.join("app.js"), "window.__LAR = true;").unwrap();
    }

    async fn body_text(response: axum::response::Response) -> String {
        String::from_utf8(
            response
                .into_body()
                .collect()
                .await
                .unwrap()
                .to_bytes()
                .to_vec(),
        )
        .unwrap()
    }

    #[test]
    fn packaged_linux_layout_resolves_ui_and_sidecars_from_usr_lib() {
        let root = tempfile::tempdir().unwrap();
        let exe_dir = root.path().join("usr/bin");
        let resources = root.path().join("usr/lib/local-ai-router");
        let ui = resources.join("ui");
        let sidecars = resources.join("sidecars/bin");
        std::fs::create_dir_all(&exe_dir).unwrap();
        std::fs::create_dir_all(&ui).unwrap();
        std::fs::create_dir_all(&sidecars).unwrap();
        std::fs::write(ui.join("index.html"), "<title>Local AI Router</title>").unwrap();
        std::fs::write(sidecars.join("llama-server-x86_64-unknown-linux-gnu"), b"").unwrap();

        let resolved = resource_dir_from_exe_dir(&exe_dir);
        assert_eq!(
            resolved.canonicalize().unwrap(),
            resources.canonicalize().unwrap()
        );
        assert_eq!(
            ui_dir_from_resource_dir(&resolved)
                .unwrap()
                .canonicalize()
                .unwrap(),
            ui.canonicalize().unwrap()
        );
        assert!(sidecars
            .join("llama-server-x86_64-unknown-linux-gnu")
            .is_file());
    }

    #[test]
    fn macos_app_bundle_layout_resolves_resources_next_to_macos() {
        let root = tempfile::tempdir().unwrap();
        let exe_dir = root.path().join("Contents/MacOS");
        let resources = root.path().join("Contents/Resources");
        std::fs::create_dir_all(&exe_dir).unwrap();
        std::fs::create_dir_all(resources.join("ui")).unwrap();
        std::fs::write(resources.join("ui/index.html"), "<title>Local AI Router</title>").unwrap();
        assert_eq!(
            resource_dir_from_exe_dir(&exe_dir)
                .canonicalize()
                .unwrap(),
            resources.canonicalize().unwrap()
        );
    }

    #[test]
    fn packaged_windows_layout_resolves_ui_and_sidecars_next_to_the_exe() {
        let root = tempfile::tempdir().unwrap();
        let exe_dir = root.path().join("Local AI Router");
        let ui = exe_dir.join("ui");
        let sidecars = exe_dir.join("sidecars/bin");
        std::fs::create_dir_all(&ui).unwrap();
        std::fs::create_dir_all(&sidecars).unwrap();
        std::fs::write(ui.join("index.html"), "<title>Local AI Router</title>").unwrap();
        std::fs::write(
            sidecars.join("llama-server-x86_64-pc-windows-msvc.exe"),
            b"",
        )
        .unwrap();

        let resolved = resource_dir_from_exe_dir(&exe_dir);
        assert_eq!(
            resolved.canonicalize().unwrap(),
            exe_dir.canonicalize().unwrap()
        );
        assert_eq!(
            ui_dir_from_resource_dir(&resolved)
                .unwrap()
                .canonicalize()
                .unwrap(),
            ui.canonicalize().unwrap()
        );
        assert!(sidecars
            .join("llama-server-x86_64-pc-windows-msvc.exe")
            .is_file());
    }

    #[test]
    fn tauri_conf_lists_nsis_for_windows_installers() {
        let conf = include_str!("../tauri.conf.json");
        assert!(conf.contains("\"nsis\""));
        assert!(conf.contains("icon.ico"));
        assert!(conf.contains("scripts/sync-ui.mjs"));
    }

    #[test]
    fn serve_args_parse_port_data_and_secrets() {
        let args = parse_serve_args([
            "--port",
            "18080",
            "--data-dir",
            "/tmp/lar",
            "--ui-dir",
            "/tmp/ui",
            "--secrets-file",
            "/tmp/secrets.json",
        ])
        .unwrap();
        assert_eq!(args.port, Some(18080));
        assert_eq!(args.data_dir.as_deref(), Some(Path::new("/tmp/lar")));
        assert_eq!(args.ui_dir.as_deref(), Some(Path::new("/tmp/ui")));
        assert_eq!(
            args.secrets_file.as_deref(),
            Some(Path::new("/tmp/secrets.json"))
        );
    }

    #[test]
    fn serve_help_documents_headless_defaults() {
        let help = serve_help();
        assert!(help.contains("without a desktop window or tray icon"));
        assert!(help.contains("127.0.0.1"));
        assert!(help.contains("app.local-ai-router.desktop"));
        assert!(help.contains("--secrets-file"));
        #[cfg(target_os = "macos")]
        {
            assert!(help.contains("Application Support"));
            assert!(help.contains("Keychain"));
        }
        #[cfg(target_os = "linux")]
        {
            assert!(help.contains(".local/share") || help.contains("XDG_DATA_HOME"));
            assert!(help.contains("secret service") || help.contains("Secret Service"));
        }
        #[cfg(windows)]
        {
            assert!(help.contains("APPDATA"));
            assert!(help.contains("Credential Manager"));
        }
    }

    #[test]
    fn default_data_dir_uses_the_host_app_support_location() {
        let dir = default_data_dir();
        let rendered = dir.to_string_lossy();
        assert!(rendered.contains("app.local-ai-router.desktop"));
        #[cfg(target_os = "macos")]
        assert!(rendered.contains("Application Support"));
        #[cfg(target_os = "linux")]
        {
            let xdg = std::env::var_os("XDG_DATA_HOME").map(PathBuf::from);
            if let Some(xdg) = xdg {
                assert_eq!(dir, xdg.join("app.local-ai-router.desktop"));
            } else {
                assert!(rendered.contains(".local/share"));
            }
        }
        #[cfg(windows)]
        {
            let appdata = std::env::var_os("APPDATA").map(PathBuf::from);
            if let Some(appdata) = appdata {
                assert_eq!(dir, appdata.join("app.local-ai-router.desktop"));
            } else {
                assert!(rendered.contains("app.local-ai-router.desktop"));
            }
        }
    }

    #[tokio::test]
    async fn headless_router_serves_admin_spa_and_health() {
        let root = tempfile::tempdir().unwrap();
        let ui = root.path().join("ui");
        write_spa(&ui);
        let engine = test_engine(&root.path().join("data"), Some(ui)).await;
        let response = engine
            .router()
            .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert!(body_text(response).await.contains("Local AI Router"));

        let health = engine
            .router()
            .oneshot(
                Request::builder()
                    .uri("/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(health.status(), StatusCode::OK);
        let payload: Value =
            serde_json::from_slice(&health.into_body().collect().await.unwrap().to_bytes())
                .unwrap();
        assert_eq!(payload["status"], "ok");
    }

    #[tokio::test]
    async fn headless_smoke_serves_spa_and_authenticated_api() {
        let root = tempfile::tempdir().unwrap();
        let ui = root.path().join("ui");
        write_spa(&ui);
        let engine = test_engine(&root.path().join("data"), Some(ui)).await;
        let created = engine
            .router()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/admin/create_local_api_key")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(json!({"name":"Smoke"}).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(created.status(), StatusCode::OK);
        let key: Value =
            serde_json::from_slice(&created.into_body().collect().await.unwrap().to_bytes())
                .unwrap();
        let token = key["token"].as_str().unwrap();
        assert!(token.starts_with("lar_"));

        let spa = engine
            .router()
            .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert!(body_text(spa).await.contains("Admin"));

        let models = engine
            .router()
            .oneshot(
                Request::builder()
                    .uri("/v1/models")
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(models.status(), StatusCode::OK);
        let payload: Value =
            serde_json::from_slice(&models.into_body().collect().await.unwrap().to_bytes())
                .unwrap();
        assert!(payload["data"].as_array().is_some());
    }

    #[tokio::test]
    async fn dashboard_admin_command_reports_loopback_gateway() {
        let root = tempfile::tempdir().unwrap();
        let engine = test_engine(root.path(), None).await;
        let response = engine
            .router()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/admin/dashboard")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let payload: Value =
            serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes())
                .unwrap();
        assert_eq!(payload["running"], true);
        assert!(payload["base_url"]
            .as_str()
            .unwrap()
            .starts_with("http://127.0.0.1:"));
    }

    #[tokio::test]
    async fn loopback_admin_stays_unlocked_and_inference_still_needs_a_key() {
        let root = tempfile::tempdir().unwrap();
        let engine = test_engine(root.path(), None).await;
        let dashboard = engine
            .router()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/admin/dashboard")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(dashboard.status(), StatusCode::OK);

        let models = engine
            .router()
            .oneshot(
                Request::builder()
                    .uri("/v1/models")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(models.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn off_loopback_admin_rejects_unauthenticated_browsers() {
        let root = tempfile::tempdir().unwrap();
        let data = root.path().join("lan");
        std::fs::create_dir_all(&data).unwrap();
        {
            let store = crate::storage::Store::open(&data.join("router.sqlite3"))
                .await
                .unwrap();
            store.set_setting("bind_mode", "lan").await.unwrap();
        }
        let engine = Engine::open(EngineConfig {
            data_dir: data,
            resource_dir: PathBuf::from(env!("CARGO_MANIFEST_DIR")),
            ui_dir: None,
            port: Some(0),
            secrets: Arc::new(MemorySecrets::default()),
        })
        .await
        .unwrap();
        assert!(engine.services.tls_required);
        let denied = engine
            .router()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/admin/dashboard")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(denied.status(), StatusCode::UNAUTHORIZED);

        let password = engine
            .services
            .core
            .secrets
            .get(crate::identity::OPERATOR_BOOTSTRAP_ACCOUNT)
            .unwrap()
            .unwrap();
        let login = engine
            .router()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/admin/login")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        json!({"username":"operator","password":password}).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(login.status(), StatusCode::OK);
        let cookie = login
            .headers()
            .get(header::SET_COOKIE)
            .unwrap()
            .to_str()
            .unwrap()
            .to_owned();
        let accepted = engine
            .router()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/admin/dashboard")
                    .header(header::CONTENT_TYPE, "application/json")
                    .header(header::COOKIE, cookie.split(';').next().unwrap())
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(accepted.status(), StatusCode::OK);

        let models = engine
            .router()
            .oneshot(
                Request::builder()
                    .uri("/v1/models")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(models.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn lan_bind_requires_https_material_and_fails_closed_on_bad_certs() {
        let root = tempfile::tempdir().unwrap();
        let data = root.path().join("data");
        std::fs::create_dir_all(&data).unwrap();
        {
            let store = crate::storage::Store::open(&data.join("router.sqlite3"))
                .await
                .unwrap();
            store.set_setting("bind_mode", "lan").await.unwrap();
            store
                .set_setting("tls_cert_path", data.join("missing.crt").to_str().unwrap())
                .await
                .unwrap();
            store
                .set_setting("tls_key_path", data.join("missing.key").to_str().unwrap())
                .await
                .unwrap();
        }
        let error = Engine::open(EngineConfig {
            data_dir: data.clone(),
            resource_dir: PathBuf::from(env!("CARGO_MANIFEST_DIR")),
            ui_dir: None,
            port: Some(0),
            secrets: Arc::new(MemorySecrets::default()),
        })
        .await
        .err()
        .expect("lan bind without certs must fail closed");
        assert!(error.to_string().contains("HTTPS") || error.to_string().contains("TLS") || error.to_string().contains("certificate"));

        let ok_dir = root.path().join("ok");
        std::fs::create_dir_all(&ok_dir).unwrap();
        {
            let store = crate::storage::Store::open(&ok_dir.join("router.sqlite3"))
                .await
                .unwrap();
            store.set_setting("bind_mode", "lan").await.unwrap();
        }
        let engine = Engine::open(EngineConfig {
            data_dir: ok_dir,
            resource_dir: PathBuf::from(env!("CARGO_MANIFEST_DIR")),
            ui_dir: None,
            port: Some(0),
            secrets: Arc::new(MemorySecrets::default()),
        })
        .await
        .unwrap();
        assert!(engine.services.tls_required);
        assert!(engine.tls_config().is_some());
        assert!(engine.services.tls_fingerprint.as_ref().unwrap().contains(':'));
        assert_eq!(engine.bind_ip().to_string(), "0.0.0.0");
    }
}
