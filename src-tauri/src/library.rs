use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use anyhow::Context;
use futures_util::StreamExt;
use serde::Deserialize;
use tokio::{
    fs,
    io::{AsyncReadExt, AsyncWriteExt},
};

use crate::{
    domain::TargetKind,
    secrets::{SecretStore, HF_ACCOUNT},
};

#[derive(Debug, Clone, serde::Serialize)]
pub struct ImportedModel {
    pub path: String,
    pub size_bytes: u64,
    pub kind: TargetKind,
}

pub async fn import_model(
    source: &Path,
    library: &Path,
    kind: TargetKind,
) -> anyhow::Result<ImportedModel> {
    validate_model(source, &kind).await?;
    fs::create_dir_all(library).await?;
    let name = source.file_name().context("model path has no filename")?;
    let destination = unique_destination(library.join(name)).await;
    if source.is_dir() {
        copy_dir(source, &destination).await?;
    } else {
        fs::copy(source, &destination).await?;
    }
    let size_bytes = path_size(&destination).await?;
    Ok(ImportedModel {
        path: destination.to_string_lossy().into_owned(),
        size_bytes,
        kind,
    })
}

pub async fn download_hugging_face(
    client: &reqwest::Client,
    secrets: Arc<dyn SecretStore>,
    repo_id: &str,
    filename: Option<&str>,
    library: &Path,
    kind: TargetKind,
) -> anyhow::Result<ImportedModel> {
    let repo_dir = library.join(repo_id.replace('/', "--"));
    fs::create_dir_all(&repo_dir).await?;
    let token = secrets.get(HF_ACCOUNT)?;
    let files = match (&kind, filename) {
        (TargetKind::Gguf, Some(file)) => vec![file.to_owned()],
        (TargetKind::Gguf, None) => anyhow::bail!("GGUF downloads require a filename/quantization"),
        (TargetKind::Mlx, _) => list_mlx_files(client, token.as_deref(), repo_id).await?,
        _ => anyhow::bail!("only MLX and GGUF can be downloaded from Hugging Face"),
    };
    for file in files {
        let destination = repo_dir.join(safe_model_path(&file)?);
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent).await?;
        }
        download_resumable(
            client,
            token.as_deref(),
            &format!("https://huggingface.co/{repo_id}/resolve/main/{file}?download=true"),
            &destination,
        )
        .await?;
    }
    if matches!(kind, TargetKind::Gguf) {
        validate_model(
            &repo_dir.join(safe_model_path(filename.expect("checked above"))?),
            &kind,
        )
        .await?;
    } else {
        validate_model(&repo_dir, &kind).await?;
    }
    let path = if matches!(kind, TargetKind::Gguf) {
        repo_dir.join(safe_model_path(filename.expect("checked above"))?)
    } else {
        repo_dir
    };
    let size_bytes = path_size(&path).await?;
    Ok(ImportedModel {
        path: path.to_string_lossy().into_owned(),
        size_bytes,
        kind,
    })
}

#[derive(Deserialize)]
struct HfFile {
    path: String,
    #[serde(rename = "type")]
    kind: String,
}

pub fn safe_model_path(value: &str) -> anyhow::Result<PathBuf> {
    let path = Path::new(value);
    if path.as_os_str().is_empty()
        || path
            .components()
            .any(|component| !matches!(component, std::path::Component::Normal(_)))
    {
        anyhow::bail!("model filename contains an unsafe path");
    }
    Ok(path.to_owned())
}

async fn list_mlx_files(
    client: &reqwest::Client,
    token: Option<&str>,
    repo_id: &str,
) -> anyhow::Result<Vec<String>> {
    let mut request = client.get(format!(
        "https://huggingface.co/api/models/{repo_id}/tree/main?recursive=true&expand=false"
    ));
    if let Some(token) = token {
        request = request.bearer_auth(token);
    }
    let response = request.send().await?.error_for_status()?;
    let files: Vec<HfFile> = response.json().await?;
    let allowed = [".json", ".safetensors", ".model", ".txt", ".tiktoken"];
    let selected = files
        .into_iter()
        .filter(|file| {
            file.kind == "file"
                && allowed
                    .iter()
                    .any(|extension| file.path.ends_with(extension))
        })
        .map(|file| file.path)
        .collect::<Vec<_>>();
    if !selected.iter().any(|file| file == "config.json")
        || !selected.iter().any(|file| file.ends_with(".safetensors"))
    {
        anyhow::bail!("repository is not a recognizable MLX model");
    }
    Ok(selected)
}

async fn download_resumable(
    client: &reqwest::Client,
    token: Option<&str>,
    url: &str,
    destination: &Path,
) -> anyhow::Result<()> {
    let part = destination.with_extension(format!(
        "{}part",
        destination
            .extension()
            .and_then(|value| value.to_str())
            .map(|value| format!("{value}."))
            .unwrap_or_default()
    ));
    let offset = fs::metadata(&part)
        .await
        .map(|metadata| metadata.len())
        .unwrap_or(0);
    let mut request = client.get(url);
    if offset > 0 {
        request = request.header(reqwest::header::RANGE, format!("bytes={offset}-"));
    }
    if let Some(token) = token {
        request = request.bearer_auth(token);
    }
    let response = request.send().await?;
    if response.status().is_redirection() {
        anyhow::bail!(
            "Hugging Face returned a redirect instead of file bytes; downloads must follow the CDN"
        );
    }
    let response = response.error_for_status()?;
    let append = offset > 0 && response.status() == reqwest::StatusCode::PARTIAL_CONTENT;
    let mut file = fs::OpenOptions::new()
        .create(true)
        .write(true)
        .append(append)
        .truncate(!append)
        .open(&part)
        .await?;
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        file.write_all(&chunk?).await?;
    }
    file.flush().await?;
    fs::rename(part, destination).await?;
    Ok(())
}

pub async fn validate_model(path: &Path, kind: &TargetKind) -> anyhow::Result<()> {
    match kind {
        TargetKind::Gguf => {
            if !path.is_file()
                || path
                    .extension()
                    .and_then(|value| value.to_str())
                    .map(|value| value.eq_ignore_ascii_case("gguf"))
                    != Some(true)
            {
                anyhow::bail!("GGUF model must be a .gguf file");
            }
            let mut file = fs::File::open(path).await?;
            let mut magic = [0u8; 4];
            file.read_exact(&mut magic).await?;
            if &magic != b"GGUF" {
                anyhow::bail!("invalid GGUF header");
            }
        }
        TargetKind::Mlx => {
            if !path.is_dir() || !path.join("config.json").is_file() {
                anyhow::bail!("MLX model must be a directory containing config.json");
            }
            let has_weights = std::fs::read_dir(path)?.flatten().any(|entry| {
                entry.path().extension().and_then(|value| value.to_str()) == Some("safetensors")
            });
            if !has_weights {
                anyhow::bail!("MLX model has no safetensors weights");
            }
        }
        _ => anyhow::bail!("not a local model kind"),
    }
    Ok(())
}

async fn unique_destination(mut path: PathBuf) -> PathBuf {
    if !path.exists() {
        return path;
    }
    let original = path.clone();
    for index in 2..1000 {
        let name = original
            .file_stem()
            .and_then(|value| value.to_str())
            .unwrap_or("model");
        let extension = original
            .extension()
            .and_then(|value| value.to_str())
            .map(|value| format!(".{value}"))
            .unwrap_or_default();
        path.set_file_name(format!("{name}-{index}{extension}"));
        if !path.exists() {
            break;
        }
    }
    path
}

async fn copy_dir(source: &Path, destination: &Path) -> anyhow::Result<()> {
    let source = source.to_owned();
    let destination = destination.to_owned();
    tokio::task::spawn_blocking(move || copy_dir_sync(&source, &destination)).await??;
    Ok(())
}

fn copy_dir_sync(source: &Path, destination: &Path) -> anyhow::Result<()> {
    std::fs::create_dir_all(destination)?;
    for entry in std::fs::read_dir(source)? {
        let entry = entry?;
        let target = destination.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir_sync(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), target)?;
        }
    }
    Ok(())
}

async fn path_size(path: &Path) -> anyhow::Result<u64> {
    let path = path.to_owned();
    tokio::task::spawn_blocking(move || path_size_sync(&path)).await?
}

fn path_size_sync(path: &Path) -> anyhow::Result<u64> {
    if path.is_file() {
        return Ok(path.metadata()?.len());
    }
    let mut total = 0;
    for entry in std::fs::read_dir(path)? {
        let path = entry?.path();
        total += path_size_sync(&path)?;
    }
    Ok(total)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn rejects_a_renamed_non_gguf_file() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("fake.gguf");
        fs::write(&path, b"nope").await.unwrap();
        assert!(validate_model(&path, &TargetKind::Gguf).await.is_err());
    }

    #[test]
    fn downloaded_model_paths_cannot_escape_the_library() {
        assert!(safe_model_path("../../outside.gguf").is_err());
        assert!(safe_model_path("/tmp/outside.gguf").is_err());
        assert_eq!(
            safe_model_path("weights/model-01.safetensors").unwrap(),
            PathBuf::from("weights/model-01.safetensors")
        );
    }
}
