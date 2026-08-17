use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::Context;
use parking_lot::Mutex;
use serde::Serialize;
use sysinfo::System;
use tokio::{
    process::{Child, Command},
    time::sleep,
};

use crate::secrets::generate_local_token;
use crate::{core::LocalActivityRegistry, domain::TargetKind, storage::ModelTarget};

struct RuntimeEntry {
    child: Child,
    port: u16,
    size_bytes: u64,
}

#[derive(Debug, Serialize)]
pub struct RuntimeStatus {
    pub target_id: String,
    pub port: u16,
    pub size_bytes: u64,
    pub queued: usize,
}

pub struct RuntimeManager {
    entries: Mutex<HashMap<String, RuntimeEntry>>,
    restart_attempted: Mutex<HashSet<String>>,
    bin_dir: PathBuf,
    budget_percent: u8,
    idle_timeout: Duration,
    activity: LocalActivityRegistry,
}

impl RuntimeManager {
    pub fn new(
        bin_dir: PathBuf,
        budget_percent: u8,
        idle_minutes: u64,
        activity: LocalActivityRegistry,
    ) -> Self {
        Self {
            entries: Mutex::new(HashMap::new()),
            restart_attempted: Mutex::new(HashSet::new()),
            bin_dir,
            budget_percent: budget_percent.clamp(10, 95),
            idle_timeout: Duration::from_secs(idle_minutes.max(1) * 60),
            activity,
        }
    }

    pub async fn start(&self, target: &ModelTarget) -> anyhow::Result<String> {
        if let Some(port) = self.entries.lock().get(&target.id).map(|entry| entry.port) {
            return Ok(format!("http://127.0.0.1:{port}/v1"));
        }
        let path = target
            .local_path
            .as_deref()
            .context("local model path missing")?;
        let size = target.size_bytes.unwrap_or(0).max(0) as u64;
        self.evict_to_fit(size).await?;
        let port = self.available_port()?;
        let binary = match target.kind {
            TargetKind::Gguf => "llama-server-aarch64-apple-darwin",
            TargetKind::Mlx => "mlx-server-aarch64-apple-darwin",
            _ => anyhow::bail!("not a local target"),
        };
        let binary = self.bin_dir.join(binary);
        if !binary.exists() {
            anyhow::bail!("runtime sidecar missing: {}", binary.display());
        }
        let mut command = Command::new(binary);
        let token = generate_local_token();
        match target.kind {
            TargetKind::Gguf => {
                command.env("LLAMA_ARG_API_KEY", &token);
                command.args([
                    "-m",
                    path,
                    "--host",
                    "127.0.0.1",
                    "--port",
                    &port.to_string(),
                    "--alias",
                    &target.provider_model,
                    "--jinja",
                    "-ngl",
                    "99",
                ]);
            }
            TargetKind::Mlx => {
                command.env("LOCAL_AI_ROUTER_RUNTIME_TOKEN", &token);
                command.args([
                    "--model",
                    path,
                    "--host",
                    "127.0.0.1",
                    "--port",
                    &port.to_string(),
                    "--alias",
                    &target.provider_model,
                ]);
            }
            _ => unreachable!(),
        }
        command
            .kill_on_drop(true)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        let child = command.spawn().context("starting inference sidecar")?;
        let base = format!("http://127.0.0.1:{port}");
        wait_ready(&base, &token).await?;
        self.entries.lock().insert(
            target.id.clone(),
            RuntimeEntry {
                child,
                port,
                size_bytes: size,
            },
        );
        self.activity.set_token(&target.id, token);
        self.activity.touch(&target.id);
        Ok(format!("{base}/v1"))
    }

    pub async fn stop(&self, id: &str) -> anyhow::Result<()> {
        let _reservation = self
            .activity
            .try_reserve_for_unload(id)
            .context("model is serving or has queued requests")?;
        self.stop_reserved(id).await
    }

    async fn stop_reserved(&self, id: &str) -> anyhow::Result<()> {
        let entry = self.entries.lock().remove(id);
        self.restart_attempted.lock().remove(id);
        if let Some(mut entry) = entry {
            entry.child.kill().await?;
        }
        Ok(())
    }

    pub async fn stop_all(&self) {
        let entries = std::mem::take(&mut *self.entries.lock());
        for (_, mut entry) in entries {
            let _ = entry.child.kill().await;
        }
    }

    pub fn statuses(&self) -> Vec<RuntimeStatus> {
        self.entries
            .lock()
            .iter()
            .map(|(id, entry)| RuntimeStatus {
                target_id: id.clone(),
                port: entry.port,
                size_bytes: entry.size_bytes,
                queued: self.activity.queued(id),
            })
            .collect()
    }

    pub fn take_crashed(&self) -> Vec<(String, bool)> {
        let crashed = self
            .entries
            .lock()
            .iter_mut()
            .filter_map(|(id, entry)| entry.child.try_wait().ok().flatten().map(|_| id.clone()))
            .collect::<Vec<_>>();
        let mut entries = self.entries.lock();
        let mut attempted = self.restart_attempted.lock();
        crashed
            .into_iter()
            .map(|id| {
                entries.remove(&id);
                let may_restart = attempted.insert(id.clone());
                (id, may_restart)
            })
            .collect()
    }

    pub async fn reap_idle(&self) -> Vec<String> {
        let ids = self
            .entries
            .lock()
            .keys()
            .filter_map(|id| {
                (self.activity.idle_for(id) >= self.idle_timeout)
                    .then(|| {
                        self.activity
                            .try_reserve_for_unload(id)
                            .map(|permit| (id.clone(), permit))
                    })
                    .flatten()
            })
            .collect::<Vec<_>>();
        let mut stopped = Vec::new();
        for (id, _reservation) in ids {
            if self.stop_reserved(&id).await.is_ok() {
                stopped.push(id);
            }
        }
        stopped
    }

    async fn evict_to_fit(&self, incoming: u64) -> anyhow::Result<()> {
        let mut system = System::new();
        system.refresh_memory();
        let budget = system
            .total_memory()
            .saturating_mul(self.budget_percent as u64)
            / 100;
        loop {
            let used: u64 = self
                .entries
                .lock()
                .values()
                .map(|entry| entry.size_bytes)
                .sum();
            if used.saturating_add(incoming) <= budget {
                return Ok(());
            }
            let candidate = self
                .entries
                .lock()
                .keys()
                .filter_map(|id| {
                    self.activity
                        .try_reserve_for_unload(id)
                        .map(|permit| (id.clone(), self.activity.idle_for(id), permit))
                })
                .max_by_key(|(_, idle, _)| *idle);
            match candidate {
                Some((id, _, _reservation)) => self.stop_reserved(&id).await?,
                None => anyhow::bail!("memory budget is exhausted by active models"),
            }
        }
    }

    fn available_port(&self) -> anyhow::Result<u16> {
        for port in 12100..12200 {
            if std::net::TcpListener::bind(("127.0.0.1", port)).is_ok() {
                return Ok(port);
            }
        }
        anyhow::bail!("no free inference port")
    }
}

async fn wait_ready(base_url: &str, token: &str) -> anyhow::Result<()> {
    let client = reqwest::Client::new();
    for _ in 0..120 {
        if client
            .get(format!("{base_url}/health"))
            .bearer_auth(token)
            .send()
            .await
            .map(|response| response.status().is_success())
            .unwrap_or(false)
        {
            return Ok(());
        }
        sleep(Duration::from_millis(250)).await;
    }
    anyhow::bail!("inference sidecar did not become ready")
}

pub fn bundled_bin_dir(resource_dir: &Path) -> PathBuf {
    let bundled = resource_dir.join("sidecars/bin");
    if bundled.join("llama-server-aarch64-apple-darwin").exists()
        || bundled.join("mlx-server-aarch64-apple-darwin").exists()
    {
        bundled
    } else {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../sidecars/bin")
    }
}
