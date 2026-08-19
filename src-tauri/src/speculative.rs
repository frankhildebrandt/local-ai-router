use anyhow::Context;
use serde::{Deserialize, Serialize};

use crate::{
    domain::TargetKind,
    storage::{ModelTarget, Store},
};

pub const MIN_N_MAX: u8 = 1;
pub const MAX_N_MAX: u8 = 128;
pub const DEFAULT_DRAFT_N_MAX_GGUF: u8 = 16;
pub const DEFAULT_DRAFT_N_MAX_MLX: u8 = 5;
pub const DEFAULT_NGRAM_N_MAX: u8 = 64;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SpeculativeMode {
    DraftModel,
    Ngram,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SpeculativeConfig {
    pub mode: SpeculativeMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub draft_target_id: Option<String>,
    pub n_max: u8,
}

impl SpeculativeConfig {
    pub fn default_n_max(mode: SpeculativeMode, kind: TargetKind) -> u8 {
        match mode {
            SpeculativeMode::Ngram => DEFAULT_NGRAM_N_MAX,
            SpeculativeMode::DraftModel if kind == TargetKind::Mlx => DEFAULT_DRAFT_N_MAX_MLX,
            SpeculativeMode::DraftModel => DEFAULT_DRAFT_N_MAX_GGUF,
        }
    }

    pub fn normalized(mut self, kind: TargetKind) -> Self {
        if self.n_max == 0 {
            self.n_max = Self::default_n_max(self.mode, kind);
        }
        if self.mode == SpeculativeMode::Ngram {
            self.draft_target_id = None;
        }
        self
    }
}

pub fn is_local_chat_target(target: &ModelTarget) -> bool {
    if !target
        .capabilities
        .iter()
        .any(|capability| capability == "chat")
    {
        return false;
    }
    match target.kind {
        TargetKind::Gguf => {
            matches!(
                target.local.runtime_engine.as_deref().unwrap_or("llama"),
                "llama"
            )
        }
        TargetKind::Mlx => {
            target.local.runtime_engine.as_deref().unwrap_or("mlx_chat") == "mlx_chat"
        }
        _ => false,
    }
}

pub fn estimated_bytes(target: &ModelTarget) -> u64 {
    target
        .local
        .estimated_memory_bytes
        .or(target.size_bytes)
        .unwrap_or(0)
        .max(0) as u64
}

pub fn fingerprint(config: Option<&SpeculativeConfig>, draft_path: Option<&str>) -> String {
    match config {
        Some(config) => format!(
            "{:?}\0{}\0{}\0{}",
            config.mode,
            config.draft_target_id.as_deref().unwrap_or_default(),
            draft_path.unwrap_or_default(),
            config.n_max
        ),
        None => String::new(),
    }
}

pub fn validate(
    target: &ModelTarget,
    config: &SpeculativeConfig,
    draft: Option<&ModelTarget>,
) -> anyhow::Result<()> {
    anyhow::ensure!(
        is_local_chat_target(target),
        "speculative decoding is only available for local chat models"
    );
    anyhow::ensure!(
        (MIN_N_MAX..=MAX_N_MAX).contains(&config.n_max),
        "draft tokens must be between {MIN_N_MAX} and {MAX_N_MAX}"
    );
    match config.mode {
        SpeculativeMode::Ngram => {
            anyhow::ensure!(
                target.kind == TargetKind::Gguf,
                "n-gram speculative decoding is only available for GGUF models"
            );
        }
        SpeculativeMode::DraftModel => {
            let draft = draft.context("select a smaller draft model from the library")?;
            anyhow::ensure!(draft.id != target.id, "a model cannot draft for itself");
            anyhow::ensure!(
                draft.kind == target.kind,
                "draft model must use the same engine as the target"
            );
            anyhow::ensure!(
                is_local_chat_target(draft),
                "draft model must be a local chat model"
            );
            anyhow::ensure!(
                draft
                    .local_path
                    .as_deref()
                    .is_some_and(|value| !value.is_empty()),
                "draft model path is missing"
            );
        }
    }
    Ok(())
}

pub async fn resolve_draft(
    store: &Store,
    target: &ModelTarget,
) -> anyhow::Result<Option<ModelTarget>> {
    let Some(config) = target.local.speculative_config.as_ref() else {
        return Ok(None);
    };
    let draft = match config.mode {
        SpeculativeMode::DraftModel => {
            let id = config
                .draft_target_id
                .as_deref()
                .context("select a smaller draft model from the library")?;
            Some(
                store
                    .target(id)
                    .await?
                    .context("draft model is no longer in the library")?,
            )
        }
        SpeculativeMode::Ngram => None,
    };
    validate(target, config, draft.as_ref())?;
    Ok(draft)
}

pub fn gguf_speculative_args(
    config: &SpeculativeConfig,
    draft_path: Option<&str>,
    gpu_layers: i32,
) -> anyhow::Result<Vec<String>> {
    let n_max = config.n_max.to_string();
    match config.mode {
        SpeculativeMode::DraftModel => {
            let path = draft_path.context("draft model path missing")?;
            let ngl = if gpu_layers < 0 {
                "99".to_string()
            } else {
                gpu_layers.to_string()
            };
            Ok(vec![
                "-md".into(),
                path.into(),
                "--spec-type".into(),
                "draft-simple".into(),
                "--spec-draft-n-max".into(),
                n_max,
                "--spec-draft-ngl".into(),
                ngl,
            ])
        }
        SpeculativeMode::Ngram => Ok(vec![
            "--spec-type".into(),
            "ngram-simple".into(),
            "--spec-draft-n-max".into(),
            n_max,
        ]),
    }
}

pub fn mlx_speculative_args(
    config: &SpeculativeConfig,
    draft_path: Option<&str>,
) -> anyhow::Result<Vec<String>> {
    match config.mode {
        SpeculativeMode::Ngram => {
            anyhow::bail!("n-gram speculative decoding is only available for GGUF models")
        }
        SpeculativeMode::DraftModel => {
            let path = draft_path.context("draft model path missing")?;
            Ok(vec![
                "--draft-model".into(),
                path.into(),
                "--draft-tokens".into(),
                config.n_max.to_string(),
            ])
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{providers::WireProtocol, storage::LocalModelMeta};

    fn chat(id: &str, kind: TargetKind, path: &str) -> ModelTarget {
        ModelTarget {
            id: id.into(),
            provider_id: None,
            name: id.into(),
            kind,
            provider_model: id.into(),
            local_path: Some(path.into()),
            runtime_url: None,
            wire_protocol: WireProtocol::OpenAiChat,
            capabilities: vec!["chat".into()],
            enabled: true,
            state: "stopped".into(),
            size_bytes: Some(1_000),
            local: LocalModelMeta {
                runtime_engine: Some(match kind {
                    TargetKind::Mlx => "mlx_chat".into(),
                    _ => "llama".into(),
                }),
                estimated_memory_bytes: Some(2_000),
                ..Default::default()
            },
        }
    }

    fn image() -> ModelTarget {
        ModelTarget {
            id: "img".into(),
            provider_id: None,
            name: "Image".into(),
            kind: TargetKind::Mlx,
            provider_model: "image".into(),
            local_path: Some("/tmp/image".into()),
            runtime_url: None,
            wire_protocol: WireProtocol::OpenAiChat,
            capabilities: vec!["images".into()],
            enabled: true,
            state: "stopped".into(),
            size_bytes: None,
            local: LocalModelMeta {
                runtime_engine: Some("mlx_image".into()),
                ..Default::default()
            },
        }
    }

    #[test]
    fn gguf_draft_args_include_spec_type_and_gpu_layers() {
        let config = SpeculativeConfig {
            mode: SpeculativeMode::DraftModel,
            draft_target_id: Some("small".into()),
            n_max: 16,
        };
        assert_eq!(
            gguf_speculative_args(&config, Some("/models/draft.gguf"), -1).unwrap(),
            vec![
                "-md",
                "/models/draft.gguf",
                "--spec-type",
                "draft-simple",
                "--spec-draft-n-max",
                "16",
                "--spec-draft-ngl",
                "99",
            ]
        );
    }

    #[test]
    fn gguf_ngram_args_omit_draft_model() {
        let config = SpeculativeConfig {
            mode: SpeculativeMode::Ngram,
            draft_target_id: None,
            n_max: 64,
        };
        assert_eq!(
            gguf_speculative_args(&config, None, 12).unwrap(),
            vec!["--spec-type", "ngram-simple", "--spec-draft-n-max", "64"]
        );
    }

    #[test]
    fn mlx_args_pass_draft_path_and_tokens() {
        let config = SpeculativeConfig {
            mode: SpeculativeMode::DraftModel,
            draft_target_id: Some("small".into()),
            n_max: 5,
        };
        assert_eq!(
            mlx_speculative_args(&config, Some("/models/draft")).unwrap(),
            vec!["--draft-model", "/models/draft", "--draft-tokens", "5"]
        );
        assert!(mlx_speculative_args(
            &SpeculativeConfig {
                mode: SpeculativeMode::Ngram,
                draft_target_id: None,
                n_max: 64,
            },
            None
        )
        .is_err());
    }

    #[test]
    fn rejects_image_models_and_self_drafts() {
        let target = chat("big", TargetKind::Gguf, "/tmp/big.gguf");
        let config = SpeculativeConfig {
            mode: SpeculativeMode::DraftModel,
            draft_target_id: Some("big".into()),
            n_max: 16,
        };
        assert!(validate(&image(), &config, None).is_err());
        assert!(validate(&target, &config, Some(&target)).is_err());
        assert!(validate(
            &target,
            &SpeculativeConfig {
                mode: SpeculativeMode::Ngram,
                draft_target_id: None,
                n_max: 64,
            },
            None
        )
        .is_ok());
        let mlx = chat("mlx", TargetKind::Mlx, "/tmp/mlx");
        assert!(validate(
            &mlx,
            &SpeculativeConfig {
                mode: SpeculativeMode::Ngram,
                draft_target_id: None,
                n_max: 64,
            },
            None
        )
        .is_err());
    }

    #[test]
    fn estimated_bytes_prefer_memory_estimate() {
        let target = chat("big", TargetKind::Gguf, "/tmp/big.gguf");
        assert_eq!(estimated_bytes(&target), 2_000);
    }

    #[test]
    fn fingerprint_changes_with_mode_and_draft() {
        let draft = SpeculativeConfig {
            mode: SpeculativeMode::DraftModel,
            draft_target_id: Some("small".into()),
            n_max: 16,
        };
        let ngram = SpeculativeConfig {
            mode: SpeculativeMode::Ngram,
            draft_target_id: None,
            n_max: 64,
        };
        assert_ne!(
            fingerprint(Some(&draft), Some("/a.gguf")),
            fingerprint(Some(&ngram), None)
        );
        assert_ne!(
            fingerprint(Some(&draft), Some("/a.gguf")),
            fingerprint(Some(&draft), Some("/b.gguf"))
        );
        assert!(fingerprint(None, None).is_empty());
    }
}
