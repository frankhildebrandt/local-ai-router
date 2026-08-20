use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GgufBackend {
    Cpu,
    Cuda,
    Vulkan,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GgufBackendPreference {
    Auto,
    Cpu,
    Cuda,
    Vulkan,
}

impl GgufBackendPreference {
    pub fn from_env(value: Option<&str>) -> Self {
        match value.map(str::trim).map(str::to_ascii_lowercase).as_deref() {
            Some("cpu") => Self::Cpu,
            Some("cuda") | Some("nvidia") => Self::Cuda,
            Some("vulkan") | Some("amd") => Self::Vulkan,
            _ => Self::Auto,
        }
    }
}

pub fn gguf_sidecar_stem(backend: GgufBackend) -> &'static str {
    match backend {
        GgufBackend::Cpu => "llama-server",
        GgufBackend::Cuda => "llama-server-cuda",
        GgufBackend::Vulkan => "llama-server-vulkan",
    }
}

pub fn host_gguf_sidecar_filename(backend: GgufBackend) -> String {
    let mut name = format!("{}-{}", gguf_sidecar_stem(backend), env!("TARGET"));
    if cfg!(windows) {
        name.push_str(".exe");
    }
    name
}

pub fn resolve_gguf_sidecar(
    bin_dir: &Path,
    gpu_layers: i32,
    preference: GgufBackendPreference,
    nvidia_present: bool,
    vulkan_present: bool,
) -> PathBuf {
    let candidates = gguf_backend_candidates(
        gpu_layers,
        preference,
        nvidia_present,
        vulkan_present,
    );
    for backend in candidates {
        let path = bin_dir.join(host_gguf_sidecar_filename(backend));
        if path.is_file() {
            return path;
        }
    }
    bin_dir.join(host_gguf_sidecar_filename(GgufBackend::Cpu))
}

pub fn gguf_backend_candidates(
    gpu_layers: i32,
    preference: GgufBackendPreference,
    nvidia_present: bool,
    vulkan_present: bool,
) -> Vec<GgufBackend> {
    if gpu_layers == 0 {
        return vec![GgufBackend::Cpu];
    }
    match preference {
        GgufBackendPreference::Cpu => vec![GgufBackend::Cpu],
        GgufBackendPreference::Cuda => {
            vec![GgufBackend::Cuda, GgufBackend::Cpu]
        }
        GgufBackendPreference::Vulkan => {
            vec![GgufBackend::Vulkan, GgufBackend::Cpu]
        }
        GgufBackendPreference::Auto => {
            let mut candidates = Vec::new();
            if nvidia_present {
                candidates.push(GgufBackend::Cuda);
            }
            if vulkan_present {
                candidates.push(GgufBackend::Vulkan);
            }
            candidates.push(GgufBackend::Cpu);
            candidates
        }
    }
}

#[cfg(not(target_os = "macos"))]
pub fn probe_nvidia_gpu() -> bool {
    std::process::Command::new("nvidia-smi")
        .args(["--query-gpu=name", "--format=csv,noheader"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

#[cfg(not(target_os = "macos"))]
pub fn probe_vulkan_gpu() -> bool {
    if std::process::Command::new("vulkaninfo")
        .arg("--summary")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
    {
        return true;
    }
    #[cfg(unix)]
    {
        for path in ["/usr/share/vulkan/icd.d", "/etc/vulkan/icd.d"] {
            if Path::new(path).is_dir() {
                if std::fs::read_dir(path)
                    .map(|entries| entries.flatten().next().is_some())
                    .unwrap_or(false)
                {
                    return true;
                }
            }
        }
    }
    #[cfg(windows)]
    {
        if Path::new(r"C:\Windows\System32\vulkan-1.dll").is_file() {
            return std::process::Command::new("vulkaninfo")
                .arg("--summary")
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .map(|status| status.success())
                .unwrap_or(false);
        }
    }
    false
}

#[cfg(target_os = "macos")]
pub fn probe_nvidia_gpu() -> bool {
    false
}

#[cfg(target_os = "macos")]
pub fn probe_vulkan_gpu() -> bool {
    false
}

pub fn resolve_gguf_sidecar_for_host(bin_dir: &Path, gpu_layers: i32) -> PathBuf {
    let preference =
        GgufBackendPreference::from_env(std::env::var("LOCAL_AI_ROUTER_GGUF_BACKEND").ok().as_deref());
    resolve_gguf_sidecar(
        bin_dir,
        gpu_layers,
        preference,
        probe_nvidia_gpu(),
        probe_vulkan_gpu(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cpu_only_when_gpu_layers_are_zero() {
        assert_eq!(
            gguf_backend_candidates(0, GgufBackendPreference::Auto, true, true),
            vec![GgufBackend::Cpu]
        );
    }

    #[test]
    fn auto_prefers_cuda_then_vulkan_then_cpu() {
        assert_eq!(
            gguf_backend_candidates(-1, GgufBackendPreference::Auto, true, true),
            vec![
                GgufBackend::Cuda,
                GgufBackend::Vulkan,
                GgufBackend::Cpu
            ]
        );
        assert_eq!(
            gguf_backend_candidates(-1, GgufBackendPreference::Auto, true, false),
            vec![GgufBackend::Cuda, GgufBackend::Cpu]
        );
        assert_eq!(
            gguf_backend_candidates(-1, GgufBackendPreference::Auto, false, true),
            vec![GgufBackend::Vulkan, GgufBackend::Cpu]
        );
        assert_eq!(
            gguf_backend_candidates(-1, GgufBackendPreference::Auto, false, false),
            vec![GgufBackend::Cpu]
        );
    }

    #[test]
    fn explicit_cuda_falls_back_to_cpu_binary() {
        assert_eq!(
            gguf_backend_candidates(-1, GgufBackendPreference::Cuda, false, false),
            vec![GgufBackend::Cuda, GgufBackend::Cpu]
        );
    }

    #[test]
    fn resolve_picks_first_existing_sidecar() {
        let root = tempfile::tempdir().unwrap();
        let bin = root.path();
        let cpu = bin.join(host_gguf_sidecar_filename(GgufBackend::Cpu));
        let cuda = bin.join(host_gguf_sidecar_filename(GgufBackend::Cuda));
        std::fs::write(&cpu, b"cpu").unwrap();
        std::fs::write(&cuda, b"cuda").unwrap();

        let resolved = resolve_gguf_sidecar(
            bin,
            -1,
            GgufBackendPreference::Auto,
            true,
            false,
        );
        assert_eq!(resolved, cuda);
    }

    #[test]
    fn resolve_falls_back_to_cpu_when_gpu_sidecar_missing() {
        let root = tempfile::tempdir().unwrap();
        let bin = root.path();
        let cpu = bin.join(host_gguf_sidecar_filename(GgufBackend::Cpu));
        std::fs::write(&cpu, b"cpu").unwrap();

        let resolved = resolve_gguf_sidecar(
            bin,
            -1,
            GgufBackendPreference::Cuda,
            true,
            false,
        );
        assert_eq!(resolved, cpu);
    }

    #[test]
    fn env_preference_parses_aliases() {
        assert_eq!(
            GgufBackendPreference::from_env(Some("CUDA")),
            GgufBackendPreference::Cuda
        );
        assert_eq!(
            GgufBackendPreference::from_env(Some("amd")),
            GgufBackendPreference::Vulkan
        );
        assert_eq!(
            GgufBackendPreference::from_env(Some("auto")),
            GgufBackendPreference::Auto
        );
    }
}
