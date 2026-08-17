use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ResourceProfile {
    Stealth,
    Balanced,
    Performance,
    Custom,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ResourcePolicy {
    pub version: u8,
    pub profile: ResourceProfile,
    pub memory_budget_percent: u8,
    pub memory_budget_mib: Option<u64>,
    pub auto_load: bool,
    pub idle_unload_minutes: u64,
    pub compute_duty_percent: u8,
    pub cpu_threads: usize,
    pub max_parallel_prompts: usize,
    pub process_priority: i8,
    pub gguf_gpu_layers: i32,
    pub disk_kv_enabled: bool,
    pub disk_kv_max_bytes: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ResourceOverrides {
    pub memory_budget_mib: Option<u64>,
    pub auto_load: Option<bool>,
    pub idle_unload_minutes: Option<u64>,
    pub compute_duty_percent: Option<u8>,
    pub cpu_threads: Option<usize>,
    pub max_parallel_prompts: Option<usize>,
    pub process_priority: Option<i8>,
    pub gguf_gpu_layers: Option<i32>,
    pub disk_kv_enabled: Option<bool>,
}

impl ResourcePolicy {
    pub const VERSION: u8 = 1;
    pub const DEFAULT_DISK_KV_BYTES: u64 = 10 * 1024 * 1024 * 1024;

    pub fn preset(profile: ResourceProfile, performance_cpus: usize) -> Self {
        let performance_cpus = performance_cpus.max(1);
        let mut policy = match profile {
            ResourceProfile::Stealth => Self {
                version: Self::VERSION,
                profile,
                memory_budget_percent: 50,
                memory_budget_mib: None,
                auto_load: true,
                idle_unload_minutes: 5,
                compute_duty_percent: 25,
                cpu_threads: (performance_cpus / 2).max(1),
                max_parallel_prompts: 1,
                process_priority: -1,
                gguf_gpu_layers: -1,
                disk_kv_enabled: true,
                disk_kv_max_bytes: Self::DEFAULT_DISK_KV_BYTES,
            },
            ResourceProfile::Balanced => Self {
                version: Self::VERSION,
                profile,
                memory_budget_percent: 70,
                memory_budget_mib: None,
                auto_load: true,
                idle_unload_minutes: 15,
                compute_duty_percent: 60,
                cpu_threads: performance_cpus,
                max_parallel_prompts: 2,
                process_priority: 0,
                gguf_gpu_layers: -1,
                disk_kv_enabled: false,
                disk_kv_max_bytes: Self::DEFAULT_DISK_KV_BYTES,
            },
            ResourceProfile::Performance => Self {
                version: Self::VERSION,
                profile,
                memory_budget_percent: 90,
                memory_budget_mib: None,
                auto_load: true,
                idle_unload_minutes: 60,
                compute_duty_percent: 100,
                cpu_threads: std::thread::available_parallelism()
                    .map(usize::from)
                    .unwrap_or(performance_cpus),
                max_parallel_prompts: 4,
                process_priority: 0,
                gguf_gpu_layers: -1,
                disk_kv_enabled: false,
                disk_kv_max_bytes: Self::DEFAULT_DISK_KV_BYTES,
            },
            ResourceProfile::Custom => Self::preset(ResourceProfile::Balanced, performance_cpus),
        };
        policy.profile = profile;
        policy
    }

    pub fn migrated(
        memory_budget_percent: u8,
        idle_unload_minutes: u64,
        logical_cpus: usize,
    ) -> Self {
        let mut policy = Self::preset(ResourceProfile::Custom, logical_cpus);
        policy.memory_budget_percent = memory_budget_percent.clamp(10, 95);
        policy.idle_unload_minutes = idle_unload_minutes;
        policy.auto_load = false;
        policy.compute_duty_percent = 100;
        policy.max_parallel_prompts = 1;
        policy.disk_kv_enabled = false;
        policy
    }

    pub fn resolve(&self, overrides: &ResourceOverrides) -> Self {
        let mut resolved = self.clone();
        resolved.memory_budget_mib = overrides.memory_budget_mib.or(self.memory_budget_mib);
        resolved.auto_load = overrides.auto_load.unwrap_or(self.auto_load);
        resolved.idle_unload_minutes = overrides
            .idle_unload_minutes
            .unwrap_or(self.idle_unload_minutes);
        resolved.compute_duty_percent = overrides
            .compute_duty_percent
            .unwrap_or(self.compute_duty_percent);
        resolved.cpu_threads = overrides.cpu_threads.unwrap_or(self.cpu_threads);
        resolved.max_parallel_prompts = overrides
            .max_parallel_prompts
            .unwrap_or(self.max_parallel_prompts);
        resolved.process_priority = overrides.process_priority.unwrap_or(self.process_priority);
        resolved.gguf_gpu_layers = overrides.gguf_gpu_layers.unwrap_or(self.gguf_gpu_layers);
        resolved.disk_kv_enabled = overrides.disk_kv_enabled.unwrap_or(self.disk_kv_enabled);
        resolved
    }

    pub fn validate(&self) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.version == Self::VERSION,
            "unsupported resource policy version"
        );
        anyhow::ensure!(
            (10..=95).contains(&self.memory_budget_percent),
            "memory budget must be between 10 and 95 percent"
        );
        anyhow::ensure!(
            self.memory_budget_mib.map_or(true, |value| value >= 512),
            "absolute memory budget must be at least 512 MiB"
        );
        anyhow::ensure!(
            (5..=100).contains(&self.compute_duty_percent),
            "compute duty must be between 5 and 100 percent"
        );
        anyhow::ensure!(
            (1..=128).contains(&self.cpu_threads),
            "CPU threads must be between 1 and 128"
        );
        anyhow::ensure!(
            (1..=16).contains(&self.max_parallel_prompts),
            "parallel prompts must be between 1 and 16"
        );
        anyhow::ensure!(
            (-1..=2).contains(&self.process_priority),
            "process priority must be between -1 and 2"
        );
        anyhow::ensure!(
            self.gguf_gpu_layers >= -1,
            "GGUF GPU layers must be auto (-1) or non-negative"
        );
        anyhow::ensure!(
            !self.disk_kv_enabled || self.max_parallel_prompts == 1,
            "disk KV requires parallel prompts to be 1"
        );
        anyhow::ensure!(
            !self.disk_kv_enabled || self.disk_kv_max_bytes >= 256 * 1024 * 1024,
            "disk KV budget must be at least 256 MiB"
        );
        Ok(())
    }

    pub fn memory_budget_bytes(&self, total_memory: u64) -> u64 {
        let percent = total_memory.saturating_mul(self.memory_budget_percent as u64) / 100;
        self.memory_budget_mib
            .map(|mib| percent.min(mib.saturating_mul(1024 * 1024)))
            .unwrap_or(percent)
    }
}

pub fn host_performance_cpu_count() -> usize {
    #[cfg(target_os = "macos")]
    {
        if let Ok(output) = std::process::Command::new("sysctl")
            .args(["-n", "hw.perflevel0.physicalcpu"])
            .output()
        {
            if output.status.success() {
                if let Ok(value) = String::from_utf8_lossy(&output.stdout).trim().parse() {
                    return value;
                }
            }
        }
    }
    std::thread::available_parallelism()
        .map(usize::from)
        .unwrap_or(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stealth_profile_has_the_agreed_background_limits() {
        let policy = ResourcePolicy::preset(ResourceProfile::Stealth, 8);

        assert_eq!(policy.memory_budget_percent, 50);
        assert_eq!(policy.compute_duty_percent, 25);
        assert_eq!(policy.cpu_threads, 4);
        assert_eq!(policy.max_parallel_prompts, 1);
        assert_eq!(policy.idle_unload_minutes, 5);
        assert!(policy.auto_load);
        assert!(policy.disk_kv_enabled);
        assert_eq!(policy.disk_kv_max_bytes, 10 * 1024 * 1024 * 1024);
    }

    #[test]
    fn model_overrides_resolve_without_mutating_global_policy() {
        let global = ResourcePolicy::preset(ResourceProfile::Balanced, 8);
        let resolved = global.resolve(&ResourceOverrides {
            compute_duty_percent: Some(25),
            max_parallel_prompts: Some(1),
            disk_kv_enabled: Some(true),
            ..ResourceOverrides::default()
        });

        assert_eq!(global.compute_duty_percent, 60);
        assert_eq!(resolved.compute_duty_percent, 25);
        assert_eq!(resolved.max_parallel_prompts, 1);
        assert!(resolved.disk_kv_enabled);
        resolved.validate().unwrap();
    }

    #[test]
    fn persistent_kv_rejects_parallel_slots() {
        let mut policy = ResourcePolicy::preset(ResourceProfile::Stealth, 8);
        policy.max_parallel_prompts = 2;

        assert!(policy
            .validate()
            .unwrap_err()
            .to_string()
            .contains("disk KV"));
    }

    #[test]
    fn absolute_memory_budget_caps_the_percentage_budget() {
        let mut policy = ResourcePolicy::preset(ResourceProfile::Balanced, 8);
        policy.memory_budget_mib = Some(4096);

        assert_eq!(
            policy.memory_budget_bytes(32 * 1024 * 1024 * 1024),
            4 * 1024 * 1024 * 1024
        );
    }
}
