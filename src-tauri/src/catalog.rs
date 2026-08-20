use serde::{Deserialize, Serialize};
use std::collections::HashSet;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CatalogCategory {
    ChatVision,
    Image,
    Speech,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RamFit {
    Fits,
    Tight,
    Unsuitable,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelTask {
    Llm,
    Vlm,
    Tts,
    Diffusion,
}

#[derive(Debug, Clone, Serialize)]
pub struct CuratedModel {
    pub id: &'static str,
    pub name: &'static str,
    pub family: &'static str,
    pub repo_id: &'static str,
    pub category: CatalogCategory,
    pub task: &'static str,
    pub runtime_engine: &'static str,
    pub quantization: &'static str,
    pub license: &'static str,
    pub alias: &'static str,
    pub capabilities: &'static [&'static str],
    pub download_bytes: u64,
    pub measured_peak_bytes: u64,
    pub model_type: &'static str,
    pub voices: &'static [&'static str],
    pub installable: bool,
    pub lock_reason: Option<&'static str>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CatalogEntryView {
    pub id: String,
    pub name: String,
    pub family: String,
    pub repo_id: String,
    pub category: CatalogCategory,
    pub task: String,
    pub runtime_engine: String,
    pub quantization: String,
    pub license: String,
    pub alias: String,
    pub capabilities: Vec<String>,
    pub download_bytes: u64,
    pub estimated_memory_bytes: u64,
    pub ram_fit: RamFit,
    pub trust_status: String,
    pub installable: bool,
    pub lock_reason: Option<String>,
    pub voices: Vec<String>,
    pub gated: bool,
    #[serde(default = "huggingface_source")]
    pub source: String,
}

fn huggingface_source() -> String {
    "huggingface".into()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MacCompatibility {
    pub apple_silicon: bool,
    pub macos_15_plus: bool,
    pub compatible: bool,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LocalCatalog {
    pub platform: MacCompatibility,
    pub memory_budget_bytes: u64,
    pub memory_budget_percent: u8,
    pub entries: Vec<CatalogEntryView>,
}

pub const LLM_TYPES: &[&str] = &[
    "llama",
    "mistral",
    "qwen2",
    "qwen3",
    "qwen3_moe",
    "qwen3_5",
    "qwen3_5_moe",
    "gemma",
    "gemma2",
    "gemma3",
    "gemma3_text",
    "gemma3n",
    "phi",
    "phi3",
    "phimoe",
    "deepseek_v3",
    "glm4",
    "glm4_moe",
    "glm4_moe_lite",
    "starcoder2",
    "cohere",
    "openelm",
    "internlm2",
    "granite",
    "granitemoehybrid",
    "mimo",
    "mimo_v2_flash",
    "minimax",
    "bitnet",
    "smollm3",
    "ernie4_5",
    "lfm2",
    "lfm2_moe",
    "exaone4",
    "olmo2",
    "olmo3",
    "olmoe",
    "falcon_h1",
    "jamba_3b",
    "apertus",
    "nanochat",
    "nemotron_h",
    "afmoe",
    "bailing_moe",
    "gpt_oss",
    "minicpm",
];

pub const VLM_TYPES: &[&str] = &[
    "qwen2_vl",
    "qwen2_5_vl",
    "qwen3_vl",
    "qwen3_5",
    "qwen3_5_moe",
    "gemma3",
    "paligemma",
    "idefics3",
    "smolvlm",
    "fastvlm",
    "llava_qwen2",
    "pixtral",
    "mistral3",
    "lfm2_vl",
    "lfm2-vl",
];

pub const IMAGE_TYPES: &[&str] = &[
    "flux2",
    "flux",
    "sd",
    "stable-diffusion",
    "sdxl",
    "sdxl_turbo",
];
pub const SPEECH_TYPES: &[&str] = &["kokoro"];

const CHAT: &[&str] = &["chat", "streaming", "tools"];
const CHAT_VISION: &[&str] = &["chat", "streaming", "tools", "vision"];
const IMAGES: &[&str] = &["images"];
const SPEECH: &[&str] = &["speech"];
const KOKORO_VOICES: &[&str] = &[
    "af_heart",
    "af_bella",
    "af_nicole",
    "am_adam",
    "am_michael",
    "bf_emma",
    "bf_isabella",
    "bm_george",
    "bm_lewis",
];

pub fn curated_models() -> Vec<CuratedModel> {
    vec![
        curated(
            "gemma-4-e2b",
            "Gemma 4 E2B Instruct 4-bit",
            "Gemma 4",
            "mlx-community/gemma-4-e2b-it-4bit",
            CatalogCategory::ChatVision,
            "chat",
            "mlx_chat",
            "4-bit",
            "gemma",
            "gemma-4-e2b",
            CHAT_VISION,
            gb(3.55),
            gb(4.5),
            "gemma3n",
            &[],
            true,
            None,
        ),
        curated(
            "gemma-4-e4b",
            "Gemma 4 E4B Instruct 4-bit",
            "Gemma 4",
            "mlx-community/gemma-4-e4b-it-4bit",
            CatalogCategory::ChatVision,
            "chat",
            "mlx_chat",
            "4-bit",
            "gemma",
            "gemma-4-e4b",
            CHAT_VISION,
            gb(5.15),
            gb(6.5),
            "gemma3n",
            &[],
            true,
            None,
        ),
        curated(
            "gemma-4-12b",
            "Gemma 4 12B Instruct 4-bit",
            "Gemma 4",
            "mlx-community/gemma-4-12B-it-4bit",
            CatalogCategory::ChatVision,
            "chat",
            "mlx_chat",
            "4-bit",
            "gemma",
            "gemma-4-12b",
            CHAT_VISION,
            gb(6.74),
            gb(8.5),
            "gemma3",
            &[],
            true,
            None,
        ),
        curated(
            "gemma-4-audio",
            "Gemma 4 audio input",
            "Gemma 4",
            "mlx-community/gemma-4-e4b-it-4bit",
            CatalogCategory::ChatVision,
            "chat",
            "mlx_chat",
            "4-bit",
            "gemma",
            "gemma-4-audio",
            &["chat", "streaming", "vision", "audio_input"],
            gb(5.15),
            gb(6.5),
            "gemma3n",
            &[],
            false,
            Some("Audio input is locked until the pinned MLX Swift LM runtime supports it."),
        ),
        curated(
            "gpt-oss-20b",
            "gpt-oss 20B MXFP4",
            "gpt-oss",
            "mlx-community/gpt-oss-20b-MXFP4-Q4",
            CatalogCategory::ChatVision,
            "chat",
            "mlx_chat",
            "MXFP4",
            "apache-2.0",
            "gpt-oss-20b",
            CHAT,
            gb(11.2),
            gb(14.0),
            "gpt_oss",
            &[],
            true,
            None,
        ),
        curated(
            "qwen-3-0-6b",
            "Qwen 3 0.6B 4-bit",
            "Qwen 3",
            "mlx-community/Qwen3-0.6B-4bit",
            CatalogCategory::ChatVision,
            "chat",
            "mlx_chat",
            "4-bit",
            "apache-2.0",
            "qwen-3-0-6b",
            CHAT,
            mb(335.0),
            mb(420.0),
            "qwen3",
            &[],
            true,
            None,
        ),
        curated(
            "qwen-3-5-0-8b",
            "Qwen 3.5 0.8B 4-bit",
            "Qwen 3.5",
            "mlx-community/Qwen3.5-0.8B-4bit",
            CatalogCategory::ChatVision,
            "chat",
            "mlx_chat",
            "4-bit",
            "apache-2.0",
            "qwen-3-5-0-8b",
            CHAT_VISION,
            mb(625.0),
            mb(940.0),
            "qwen3_5",
            &[],
            true,
            None,
        ),
        curated(
            "qwen-3-5-2b",
            "Qwen 3.5 2B 4-bit",
            "Qwen 3.5",
            "mlx-community/Qwen3.5-2B-4bit",
            CatalogCategory::ChatVision,
            "chat",
            "mlx_chat",
            "4-bit",
            "apache-2.0",
            "qwen-3-5-2b",
            CHAT,
            gb(1.6),
            gb(2.0),
            "qwen3",
            &[],
            true,
            None,
        ),
        curated(
            "qwen-3-5-4b",
            "Qwen 3.5 4B 4-bit",
            "Qwen 3.5",
            "mlx-community/Qwen3.5-4B-MLX-4bit",
            CatalogCategory::ChatVision,
            "chat",
            "mlx_chat",
            "4-bit",
            "apache-2.0",
            "qwen-3-5-4b",
            CHAT,
            gb(2.9),
            gb(3.6),
            "qwen3",
            &[],
            true,
            None,
        ),
        curated(
            "qwen-3-5-9b",
            "Qwen 3.5 9B 4-bit",
            "Qwen 3.5",
            "mlx-community/Qwen3.5-9B-4bit",
            CatalogCategory::ChatVision,
            "chat",
            "mlx_chat",
            "4-bit",
            "apache-2.0",
            "qwen-3-5-9b",
            CHAT,
            gb(5.5),
            gb(7.0),
            "qwen3",
            &[],
            true,
            None,
        ),
        curated(
            "qwen-3-8-27b",
            "Qwen 3.8 27B 4-bit",
            "Qwen 3.8",
            "mlx-community/Qwen3.8-27B-4bit",
            CatalogCategory::ChatVision,
            "chat",
            "mlx_chat",
            "4-bit",
            "apache-2.0",
            "qwen-3-8-27b",
            CHAT_VISION,
            gb(16.1),
            gb(20.0),
            "qwen3_5",
            &[],
            true,
            None,
        ),
        curated(
            "ornith-1-0-35b",
            "Ornith 1.0 35B 4-bit",
            "Ornith",
            "mlx-community/Ornith-1.0-35B-4bit",
            CatalogCategory::ChatVision,
            "chat",
            "mlx_chat",
            "4-bit",
            "mit",
            "ornith-1-0-35b",
            CHAT_VISION,
            gb(20.4),
            gb(20.9),
            "qwen3_5_moe",
            &[],
            true,
            None,
        ),
        curated(
            "mistral-7b",
            "Mistral 7B Instruct 4-bit",
            "Mistral",
            "mlx-community/Mistral-7B-Instruct-v0.3-4bit",
            CatalogCategory::ChatVision,
            "chat",
            "mlx_chat",
            "4-bit",
            "apache-2.0",
            "mistral-7b",
            CHAT,
            gb(4.1),
            gb(5.1),
            "mistral",
            &[],
            true,
            None,
        ),
        curated(
            "mistral-small-24b",
            "Mistral Small 24B Instruct 4-bit",
            "Mistral",
            "mlx-community/Mistral-Small-24B-Instruct-2501-4bit",
            CatalogCategory::ChatVision,
            "chat",
            "mlx_chat",
            "4-bit",
            "apache-2.0",
            "mistral-small-24b",
            CHAT,
            gb(13.0),
            gb(16.2),
            "mistral",
            &[],
            true,
            None,
        ),
        curated(
            "llama-3-2-1b",
            "Llama 3.2 1B Instruct 4-bit",
            "Llama 3",
            "mlx-community/Llama-3.2-1B-Instruct-4bit",
            CatalogCategory::ChatVision,
            "chat",
            "mlx_chat",
            "4-bit",
            "llama3.2",
            "llama-3-2-1b",
            CHAT,
            gb(0.8),
            gb(1.1),
            "llama",
            &[],
            true,
            None,
        ),
        curated(
            "llama-3-2-3b",
            "Llama 3.2 3B Instruct 4-bit",
            "Llama 3",
            "mlx-community/Llama-3.2-3B-Instruct-4bit",
            CatalogCategory::ChatVision,
            "chat",
            "mlx_chat",
            "4-bit",
            "llama3.2",
            "llama-3-2-3b",
            CHAT,
            gb(1.8),
            gb(2.4),
            "llama",
            &[],
            true,
            None,
        ),
        curated(
            "llama-3-1-8b",
            "Llama 3.1 8B Instruct 4-bit",
            "Llama 3",
            "mlx-community/Meta-Llama-3.1-8B-Instruct-4bit",
            CatalogCategory::ChatVision,
            "chat",
            "mlx_chat",
            "4-bit",
            "llama3.1",
            "llama-3-1-8b",
            CHAT,
            gb(4.5),
            gb(5.6),
            "llama",
            &[],
            true,
            None,
        ),
        curated(
            "phi-4-mini",
            "Phi-4 Mini Instruct 4-bit",
            "Phi",
            "mlx-community/Phi-4-mini-instruct-4bit",
            CatalogCategory::ChatVision,
            "chat",
            "mlx_chat",
            "4-bit",
            "mit",
            "phi-4-mini",
            CHAT,
            gb(2.5),
            gb(3.2),
            "phi3",
            &[],
            true,
            None,
        ),
        curated(
            "deepseek-r1-1-5b",
            "DeepSeek-R1 Distill 1.5B 4-bit",
            "DeepSeek-R1",
            "mlx-community/DeepSeek-R1-Distill-Qwen-1.5B-4bit",
            CatalogCategory::ChatVision,
            "chat",
            "mlx_chat",
            "4-bit",
            "mit",
            "deepseek-r1-1-5b",
            CHAT,
            gb(1.1),
            gb(1.4),
            "qwen2",
            &[],
            true,
            None,
        ),
        curated(
            "deepseek-r1-7b",
            "DeepSeek-R1 Distill 7B 4-bit",
            "DeepSeek-R1",
            "mlx-community/DeepSeek-R1-Distill-Qwen-7B-4bit",
            CatalogCategory::ChatVision,
            "chat",
            "mlx_chat",
            "4-bit",
            "mit",
            "deepseek-r1-7b",
            CHAT,
            gb(4.0),
            gb(5.0),
            "qwen2",
            &[],
            true,
            None,
        ),
        curated(
            "flux2-klein-4b",
            "FLUX.2 Klein 4B",
            "FLUX.2",
            "mlx-community/FLUX.2-klein-4B-4bit",
            CatalogCategory::Image,
            "image",
            "mlx_image",
            "4-bit",
            "apache-2.0",
            "flux2-klein-4b",
            IMAGES,
            gb(7.4),
            gb(12.0),
            "flux2",
            &[],
            true,
            None,
        ),
        curated(
            "sdxl-turbo",
            "SDXL Turbo",
            "Stable Diffusion",
            "mlx-community/sdxl-turbo",
            CatalogCategory::Image,
            "image",
            "mlx_image",
            "fp16",
            "openrail++",
            "sdxl-turbo",
            IMAGES,
            gb(6.9),
            gb(7.5),
            "sdxl_turbo",
            &[],
            true,
            None,
        ),
        curated(
            "sd-2-1-base",
            "Stable Diffusion 2.1 Base",
            "Stable Diffusion",
            "stabilityai/stable-diffusion-2-1-base",
            CatalogCategory::Image,
            "image",
            "mlx_image",
            "fp16",
            "openrail++",
            "sd-2-1-base",
            IMAGES,
            gb(5.2),
            gb(6.8),
            "stable-diffusion",
            &[],
            true,
            None,
        ),
        curated(
            "kokoro-82m",
            "Kokoro 82M",
            "Kokoro",
            "mweinbach/Kokoro-82M-Swift",
            CatalogCategory::Speech,
            "speech",
            "mlx_speech",
            "bf16",
            "apache-2.0",
            "kokoro-82m",
            SPEECH,
            mb(330.0),
            mb(520.0),
            "kokoro",
            KOKORO_VOICES,
            true,
            None,
        ),
    ]
}

#[allow(clippy::too_many_arguments)]
fn curated(
    id: &'static str,
    name: &'static str,
    family: &'static str,
    repo_id: &'static str,
    category: CatalogCategory,
    task: &'static str,
    runtime_engine: &'static str,
    quantization: &'static str,
    license: &'static str,
    alias: &'static str,
    capabilities: &'static [&'static str],
    download_bytes: u64,
    measured_peak_bytes: u64,
    model_type: &'static str,
    voices: &'static [&'static str],
    installable: bool,
    lock_reason: Option<&'static str>,
) -> CuratedModel {
    CuratedModel {
        id,
        name,
        family,
        repo_id,
        category,
        task,
        runtime_engine,
        quantization,
        license,
        alias,
        capabilities,
        download_bytes,
        measured_peak_bytes,
        model_type,
        voices,
        installable,
        lock_reason,
    }
}

fn gb(value: f64) -> u64 {
    (value * 1024.0 * 1024.0 * 1024.0) as u64
}

fn mb(value: f64) -> u64 {
    (value * 1024.0 * 1024.0) as u64
}

pub fn curated_by_id(id: &str) -> Option<CuratedModel> {
    curated_models().into_iter().find(|model| model.id == id)
}

pub fn classify_ram(estimated_bytes: u64, budget_bytes: u64) -> RamFit {
    if budget_bytes == 0 {
        return RamFit::Unsuitable;
    }
    if estimated_bytes == 0 {
        return RamFit::Unknown;
    }
    if estimated_bytes <= budget_bytes.saturating_mul(80) / 100 {
        RamFit::Fits
    } else if estimated_bytes <= budget_bytes {
        RamFit::Tight
    } else {
        RamFit::Unsuitable
    }
}

pub fn estimate_memory(task: ModelTask, weight_bytes: u64, measured_peak: Option<u64>) -> u64 {
    if let Some(peak) = measured_peak {
        return peak;
    }
    let factor = match task {
        ModelTask::Llm => 1.25,
        ModelTask::Vlm | ModelTask::Tts => 1.5,
        ModelTask::Diffusion => 2.0,
    };
    (weight_bytes as f64 * factor).ceil() as u64
}

pub fn unique_alias(preferred: &str, taken: &HashSet<String>) -> String {
    if !taken.contains(preferred) {
        return preferred.to_owned();
    }
    for index in 2..10_000 {
        let candidate = format!("{preferred}-{index}");
        if !taken.contains(&candidate) {
            return candidate;
        }
    }
    format!("{preferred}-x")
}

pub fn memory_budget(total_memory: u64, percent: u8) -> u64 {
    total_memory.saturating_mul(percent.clamp(10, 95) as u64) / 100
}

pub fn mac_compatibility() -> MacCompatibility {
    let apple_silicon = cfg!(all(target_os = "macos", target_arch = "aarch64"));
    let macos_15_plus = macos_major().map(|major| major >= 15).unwrap_or(false);
    let compatible = apple_silicon && macos_15_plus;
    let reason = if compatible {
        None
    } else if !apple_silicon {
        Some("Local MLX runtimes require Apple Silicon.".into())
    } else {
        Some("Local MLX runtimes require macOS 15 or later.".into())
    };
    MacCompatibility {
        apple_silicon,
        macos_15_plus,
        compatible,
        reason,
    }
}

fn macos_major() -> Option<u32> {
    #[cfg(target_os = "macos")]
    {
        let output = std::process::Command::new("sw_vers")
            .arg("-productVersion")
            .output()
            .ok()?;
        let text = String::from_utf8_lossy(&output.stdout);
        text.trim().split('.').next()?.parse().ok()
    }
    #[cfg(not(target_os = "macos"))]
    {
        None
    }
}

pub fn runtime_for_model_type(
    model_type: &str,
    pipeline_tag: Option<&str>,
) -> Option<(&'static str, &'static str, ModelTask)> {
    let normalized = model_type.to_ascii_lowercase();
    if IMAGE_TYPES.iter().any(|item| *item == normalized)
        || matches!(pipeline_tag, Some("text-to-image"))
    {
        return Some(("mlx_image", "image", ModelTask::Diffusion));
    }
    if SPEECH_TYPES.iter().any(|item| *item == normalized)
        || matches!(pipeline_tag, Some("text-to-speech"))
    {
        return Some(("mlx_speech", "speech", ModelTask::Tts));
    }
    if VLM_TYPES.iter().any(|item| *item == normalized)
        || matches!(pipeline_tag, Some("image-text-to-text" | "any-to-any"))
    {
        if LLM_TYPES.iter().any(|item| *item == normalized)
            && !VLM_TYPES.iter().any(|item| *item == normalized)
        {
            return Some(("mlx_chat", "chat", ModelTask::Llm));
        }
        return Some(("mlx_chat", "chat", ModelTask::Vlm));
    }
    if LLM_TYPES.iter().any(|item| *item == normalized) {
        return Some(("mlx_chat", "chat", ModelTask::Llm));
    }
    None
}

pub fn catalog_views(budget_bytes: u64) -> Vec<CatalogEntryView> {
    curated_models()
        .into_iter()
        .map(|model| {
            let estimated = model.measured_peak_bytes;
            CatalogEntryView {
                id: model.id.into(),
                name: model.name.into(),
                family: model.family.into(),
                repo_id: model.repo_id.into(),
                category: model.category,
                task: model.task.into(),
                runtime_engine: model.runtime_engine.into(),
                quantization: model.quantization.into(),
                license: model.license.into(),
                alias: model.alias.into(),
                capabilities: model
                    .capabilities
                    .iter()
                    .map(|item| (*item).to_string())
                    .collect(),
                download_bytes: model.download_bytes,
                estimated_memory_bytes: estimated,
                ram_fit: classify_ram(estimated, budget_bytes),
                trust_status: "curated".into(),
                installable: model.installable,
                lock_reason: model.lock_reason.map(str::to_owned),
                voices: model
                    .voices
                    .iter()
                    .map(|item| (*item).to_string())
                    .collect(),
                gated: false,
                source: "huggingface".into(),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mlx_catalog_is_locked_off_apple_silicon() {
        let platform = mac_compatibility();
        #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
        {
            assert!(platform.apple_silicon);
            assert_eq!(platform.compatible, platform.macos_15_plus);
        }
        #[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
        {
            assert!(!platform.apple_silicon);
            assert!(!platform.compatible);
            assert!(platform
                .reason
                .as_deref()
                .unwrap_or("")
                .contains("Apple Silicon"));
        }
    }

    #[test]
    fn ram_classes_use_eighty_and_one_hundred_percent_thresholds() {
        let budget = 10 * 1024 * 1024 * 1024;
        assert_eq!(classify_ram(budget * 80 / 100, budget), RamFit::Fits);
        assert_eq!(classify_ram(budget * 80 / 100 + 1, budget), RamFit::Tight);
        assert_eq!(classify_ram(budget, budget), RamFit::Tight);
        assert_eq!(classify_ram(budget + 1, budget), RamFit::Unsuitable);
        assert_eq!(classify_ram(0, budget), RamFit::Unknown);
    }

    #[test]
    fn search_hits_use_conservative_weight_factors() {
        assert_eq!(estimate_memory(ModelTask::Llm, 100, None), 125);
        assert_eq!(estimate_memory(ModelTask::Vlm, 100, None), 150);
        assert_eq!(estimate_memory(ModelTask::Tts, 100, None), 150);
        assert_eq!(estimate_memory(ModelTask::Diffusion, 100, None), 200);
        assert_eq!(estimate_memory(ModelTask::Llm, 100, Some(80)), 80);
    }

    #[test]
    fn aliases_append_deterministic_suffixes_for_conflicts() {
        let taken = HashSet::from(["qwen-3-5-4b".into(), "qwen-3-5-4b-2".into()]);
        assert_eq!(unique_alias("qwen-3-5-4b", &taken), "qwen-3-5-4b-3");
        assert_eq!(unique_alias("phi-4-mini", &taken), "phi-4-mini");
    }

    #[test]
    fn curated_catalog_covers_requested_families_and_locks_gemma_audio() {
        let models = curated_models();
        for family in [
            "Gemma 4",
            "gpt-oss",
            "Qwen 3.5",
            "Qwen 3.8",
            "Ornith",
            "Mistral",
            "Llama 3",
            "Phi",
            "DeepSeek-R1",
            "FLUX.2",
            "Stable Diffusion",
            "Kokoro",
        ] {
            assert!(
                models.iter().any(|model| model.family == family),
                "missing {family}"
            );
        }
        let audio = models
            .iter()
            .find(|model| model.id == "gemma-4-audio")
            .unwrap();
        assert!(!audio.installable);
        assert!(audio.lock_reason.unwrap().contains("Audio input"));
        assert_eq!(
            models
                .iter()
                .find(|model| model.id == "qwen-3-5-4b")
                .unwrap()
                .alias,
            "qwen-3-5-4b"
        );
        assert!(models
            .iter()
            .find(|model| model.id == "qwen-3-5-4b")
            .unwrap()
            .capabilities
            .contains(&"tools"));
        let qwen38 = models
            .iter()
            .find(|model| model.id == "qwen-3-8-27b")
            .expect("Qwen 3.8 27B");
        assert_eq!(qwen38.model_type, "qwen3_5");
        assert!(qwen38.capabilities.contains(&"vision"));
        let ornith = models
            .iter()
            .find(|model| model.id == "ornith-1-0-35b")
            .expect("Ornith");
        assert_eq!(ornith.model_type, "qwen3_5_moe");
        assert!(models.iter().any(|model| model.id == "qwen-3-0-6b"));
        assert!(models.iter().any(|model| model.id == "qwen-3-5-0-8b"));
    }

    #[test]
    fn unknown_architectures_are_not_in_the_pinned_runtime_registry() {
        assert!(runtime_for_model_type("gemma4", None).is_none());
        assert_eq!(
            runtime_for_model_type("gpt_oss", Some("text-generation"))
                .unwrap()
                .0,
            "mlx_chat"
        );
        assert_eq!(
            runtime_for_model_type("flux2", Some("text-to-image"))
                .unwrap()
                .0,
            "mlx_image"
        );
        assert_eq!(
            runtime_for_model_type("kokoro", Some("text-to-speech"))
                .unwrap()
                .0,
            "mlx_speech"
        );
        assert_eq!(
            runtime_for_model_type("stable-diffusion", Some("text-to-image"))
                .unwrap()
                .0,
            "mlx_image"
        );
        assert_eq!(
            runtime_for_model_type("sd", Some("text-to-image"))
                .unwrap()
                .0,
            "mlx_image"
        );
        assert_eq!(
            runtime_for_model_type("qwen3_5", Some("image-text-to-text"))
                .unwrap()
                .2,
            ModelTask::Vlm
        );
        assert_eq!(
            runtime_for_model_type("qwen3_5_moe", Some("image-text-to-text"))
                .unwrap()
                .2,
            ModelTask::Vlm
        );
    }

    #[test]
    fn curated_image_catalog_includes_sd_and_sdxl() {
        let models = curated_models();
        assert!(models.iter().any(|model| model.id == "sdxl-turbo"));
        let sd = models
            .iter()
            .find(|model| model.id == "sd-2-1-base")
            .expect("SD 2.1 base");
        assert_eq!(sd.runtime_engine, "mlx_image");
        assert_eq!(sd.model_type, "stable-diffusion");
        assert!(!sd.repo_id.to_ascii_lowercase().contains("sdxl"));
    }
}
