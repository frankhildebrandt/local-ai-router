use anyhow::Context;
use serde::Deserialize;

use crate::catalog::{classify_ram, estimate_memory, CatalogCategory, CatalogEntryView, ModelTask};
use crate::hub::{slug, ModelInspection, SearchPage};

pub const CIVITAI_COM: &str = "https://civitai.com";
pub const CIVITAI_RED: &str = "https://civitai.red";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CivitaiHost {
    Com,
    Red,
}

impl CivitaiHost {
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "civitai" | "civitai.com" => Some(Self::Com),
            "civitai.red" => Some(Self::Red),
            _ => None,
        }
    }

    pub fn base_url(self) -> &'static str {
        match self {
            Self::Com => CIVITAI_COM,
            Self::Red => CIVITAI_RED,
        }
    }

    pub fn source_id(self) -> &'static str {
        match self {
            Self::Com => "civitai",
            Self::Red => "civitai.red",
        }
    }

    pub fn nsfw(self) -> bool {
        matches!(self, Self::Red)
    }
}

pub fn host_from_repo(repo_id: &str) -> CivitaiHost {
    if let Some((host, _, _)) = parse_repo(repo_id) {
        return host;
    }
    if repo_id.to_ascii_lowercase().contains("civitai.red") {
        CivitaiHost::Red
    } else {
        CivitaiHost::Com
    }
}

pub fn is_civitai_repo(repo_id: &str) -> bool {
    let lower = repo_id.to_ascii_lowercase();
    parse_repo(repo_id).is_some()
        || lower.contains("civitai.com")
        || lower.contains("civitai.red")
        || (parse_model_id(repo_id).is_some() && !lower.contains('/'))
}

pub fn parse_repo(repo_id: &str) -> Option<(CivitaiHost, u64, u64)> {
    let trimmed = repo_id
        .trim()
        .trim_start_matches("https://")
        .trim_start_matches("http://");
    let host = if trimmed.starts_with("civitai.red") {
        CivitaiHost::Red
    } else if trimmed.starts_with("civitai.com") || trimmed.starts_with("civitai/") {
        CivitaiHost::Com
    } else {
        return None;
    };
    let rest = trimmed
        .split_once('/')
        .map(|(_, rest)| rest)
        .unwrap_or(trimmed);
    let rest = rest.trim_start_matches("models/");
    let (model, version) = match rest.split_once('@') {
        Some(parts) => parts,
        None => {
            let model = rest.split(['/', '?', '#']).next()?;
            let version = trimmed
                .split("modelVersionId=")
                .nth(1)?
                .split(['&', '#', '/'])
                .next()?;
            (model, version)
        }
    };
    let model_id = model.split(['/', '?', '#']).next()?.parse().ok()?;
    let version_id = version.split(['/', '?', '#']).next()?.parse().ok()?;
    Some((host, model_id, version_id))
}

fn parse_model_id(value: &str) -> Option<u64> {
    let trimmed = value
        .trim()
        .trim_start_matches("https://")
        .trim_start_matches("http://");
    let rest = trimmed
        .split_once("models/")
        .map(|(_, rest)| rest)
        .unwrap_or(trimmed);
    rest.split(['/', '?', '#', '@']).next()?.parse().ok()
}

pub fn repo_id(host: CivitaiHost, model_id: u64, version_id: u64) -> String {
    format!("{}/models/{model_id}@{version_id}", host.source_id())
}

pub fn pipeline_for_base_model(base: &str) -> Option<&'static str> {
    let normalized = base.trim().to_ascii_lowercase();
    if normalized.contains("sdxl")
        || normalized.starts_with("pony")
        || normalized.contains("illustrious")
    {
        Some("sdxl")
    } else if normalized.starts_with("sd 1")
        || normalized.starts_with("sd 2")
        || normalized == "sd"
        || normalized.starts_with("stable diffusion 1")
        || normalized.starts_with("stable diffusion 2")
    {
        Some("sd")
    } else {
        None
    }
}

pub fn rewrite_download_url(url: &str, host: CivitaiHost) -> String {
    url.replace("https://civitai.com", host.base_url())
        .replace("http://civitai.com", host.base_url())
        .replace("https://civitai.red", host.base_url())
        .replace("http://civitai.red", host.base_url())
}

#[derive(Debug, Deserialize)]
struct ModelsPage {
    #[serde(default)]
    items: Vec<CivitaiModel>,
    #[serde(default)]
    metadata: PageMeta,
}

#[derive(Debug, Deserialize, Default)]
struct PageMeta {
    #[serde(default, alias = "nextCursor", alias = "nextPage")]
    next_cursor: Option<String>,
}

#[derive(Debug, Deserialize)]
struct CivitaiModel {
    id: u64,
    name: String,
    #[serde(default, alias = "modelVersions")]
    model_versions: Vec<CivitaiVersion>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct CivitaiVersion {
    id: u64,
    #[serde(default)]
    name: String,
    #[serde(default, alias = "baseModel")]
    base_model: Option<String>,
    #[serde(default)]
    files: Vec<CivitaiFile>,
}

#[derive(Debug, Deserialize)]
struct CivitaiFile {
    #[serde(default)]
    name: String,
    #[serde(default, rename = "type")]
    kind: String,
    #[serde(default, alias = "sizeKB")]
    size_kb: Option<f64>,
    #[serde(default, alias = "downloadUrl")]
    download_url: Option<String>,
    #[serde(default)]
    metadata: CivitaiFileMeta,
}

#[derive(Debug, Deserialize, Default)]
struct CivitaiFileMeta {
    #[serde(default)]
    format: Option<String>,
    #[serde(default)]
    fp: Option<String>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct VersionResponse {
    id: u64,
    #[serde(default)]
    name: String,
    #[serde(default, alias = "baseModel")]
    base_model: Option<String>,
    #[serde(default)]
    files: Vec<CivitaiFile>,
    #[serde(default)]
    model: Option<VersionModel>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct VersionModel {
    id: u64,
    #[serde(default)]
    name: String,
}

pub struct CivitaiClient {
    client: reqwest::Client,
    host: CivitaiHost,
    base_url: String,
    token: Option<String>,
}

impl CivitaiClient {
    pub fn new(client: reqwest::Client, host: CivitaiHost, token: Option<String>) -> Self {
        Self {
            client,
            host,
            base_url: host.base_url().trim_end_matches('/').to_owned(),
            token,
        }
    }

    #[cfg(test)]
    fn with_base_url(self, base_url: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into().trim_end_matches('/').to_owned(),
            ..self
        }
    }

    fn request(&self, url: String) -> reqwest::RequestBuilder {
        let mut request = self.client.get(url);
        if let Some(token) = &self.token {
            request = request.bearer_auth(token);
        }
        request
    }

    pub async fn search(
        &self,
        query: &str,
        cursor: Option<&str>,
        budget_bytes: u64,
    ) -> anyhow::Result<SearchPage> {
        let mut url = format!(
            "{}/api/v1/models?types=Checkpoint&limit=20&nsfw={}",
            self.base_url,
            self.host.nsfw()
        );
        if !query.trim().is_empty() {
            url.push_str(&format!(
                "&query={}",
                url::form_urlencoded::byte_serialize(query.trim().as_bytes()).collect::<String>()
            ));
        }
        if let Some(cursor) = cursor.filter(|value| !value.is_empty()) {
            if cursor.starts_with("http://") || cursor.starts_with("https://") {
                url = rewrite_download_url(cursor, self.host);
            } else {
                url.push_str(&format!(
                    "&cursor={}",
                    url::form_urlencoded::byte_serialize(cursor.as_bytes()).collect::<String>()
                ));
            }
        }
        let page: ModelsPage = self
            .request(url)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await
            .context("civitai search returned invalid JSON")?;
        let items = page
            .items
            .iter()
            .filter_map(|model| search_view(model, self.host, budget_bytes))
            .collect();
        Ok(SearchPage {
            items,
            next_cursor: page.metadata.next_cursor.and_then(|value| {
                let trimmed = value.trim();
                (!trimmed.is_empty()).then(|| trimmed.to_owned())
            }),
        })
    }

    pub async fn resolve(&self, value: &str) -> anyhow::Result<String> {
        if let Some((_, model_id, version_id)) = parse_repo(value) {
            return Ok(repo_id(self.host, model_id, version_id));
        }
        let model_id = parse_model_id(value).context("not a CivitAI model id")?;
        if let Some(version_id) = value
            .split("modelVersionId=")
            .nth(1)
            .and_then(|part| part.split(['&', '#', '/']).next())
            .and_then(|part| part.parse().ok())
        {
            return Ok(repo_id(self.host, model_id, version_id));
        }
        let url = format!("{}/api/v1/models/{model_id}", self.base_url);
        let model: CivitaiModel = self
            .request(url)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await
            .context("civitai model returned invalid JSON")?;
        let version = model
            .model_versions
            .first()
            .context("this CivitAI model has no versions")?;
        Ok(repo_id(self.host, model.id, version.id))
    }

    pub async fn inspect(
        &self,
        repo_id: &str,
        budget_bytes: u64,
    ) -> anyhow::Result<ModelInspection> {
        let resolved = self.resolve(repo_id).await?;
        let (host, _, version_id) = parse_repo(&resolved).context("not a CivitAI model id")?;
        let url = format!("{}/api/v1/model-versions/{version_id}", self.base_url);
        let version: VersionResponse = self
            .request(url)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await
            .context("civitai model version returned invalid JSON")?;
        listing_from_version(host, &resolved, version, budget_bytes)
    }
}

fn primary_file(files: &[CivitaiFile]) -> Option<&CivitaiFile> {
    files.iter().find(|file| {
        let format = file.metadata.format.as_deref().unwrap_or_default();
        (file.kind.eq_ignore_ascii_case("Model") || file.kind.is_empty())
            && (format.eq_ignore_ascii_case("SafeTensor")
                || file.name.ends_with(".safetensors")
                || format.is_empty() && file.download_url.is_some())
            && file.download_url.is_some()
    })
}

fn search_view(
    model: &CivitaiModel,
    host: CivitaiHost,
    budget_bytes: u64,
) -> Option<CatalogEntryView> {
    let version = model.model_versions.first()?;
    let file = primary_file(&version.files)?;
    let base = version.base_model.as_deref().unwrap_or_default();
    let pipeline = pipeline_for_base_model(base)?;
    let bytes = file
        .size_kb
        .map(|value| (value * 1024.0) as u64)
        .unwrap_or(0);
    let estimated = estimate_memory(ModelTask::Diffusion, bytes, None);
    let repo = repo_id(host, model.id, version.id);
    Some(CatalogEntryView {
        id: repo.clone(),
        name: model.name.clone(),
        family: if base.is_empty() {
            "Stable Diffusion".into()
        } else {
            base.into()
        },
        repo_id: repo,
        category: CatalogCategory::Image,
        task: "image".into(),
        runtime_engine: "mlx_image".into(),
        quantization: file.metadata.fp.clone().unwrap_or_else(|| "fp16".into()),
        license: "civitai".into(),
        alias: slug(&model.name),
        capabilities: vec!["images".into()],
        download_bytes: bytes,
        estimated_memory_bytes: estimated,
        ram_fit: classify_ram(estimated, budget_bytes),
        trust_status: "untested".into(),
        installable: true,
        lock_reason: None,
        voices: Vec::new(),
        gated: false,
        source: host.source_id().into(),
    })
    .filter(|_| pipeline == "sd" || pipeline == "sdxl")
}

fn listing_from_version(
    host: CivitaiHost,
    repo_id: &str,
    version: VersionResponse,
    budget_bytes: u64,
) -> anyhow::Result<ModelInspection> {
    let file =
        primary_file(&version.files).context("this CivitAI version has no SafeTensor weights")?;
    let base = version.base_model.as_deref().unwrap_or_default();
    let pipeline =
        pipeline_for_base_model(base).context("this CivitAI checkpoint is not SD or SDXL")?;
    let bytes = file
        .size_kb
        .map(|value| (value * 1024.0) as u64)
        .unwrap_or(0);
    let filename = if file.name.ends_with(".safetensors") {
        file.name.clone()
    } else {
        format!("{}.safetensors", slug(&file.name))
    };
    Ok(ModelInspection {
        repo_id: repo_id.into(),
        revision: version.id.to_string(),
        model_type: Some(if pipeline == "sdxl" {
            "sdxl".into()
        } else {
            "stable-diffusion".into()
        }),
        pipeline_tag: Some("text-to-image".into()),
        license: Some("civitai".into()),
        gated: false,
        mlx_format: true,
        download_bytes: bytes,
        files: vec![filename],
        runtime_engine: Some("mlx_image".into()),
        task: Some("image".into()),
        category: Some(CatalogCategory::Image),
        capabilities: vec!["images".into()],
        estimated_memory_bytes: estimate_memory(ModelTask::Diffusion, bytes, None),
        ram_fit: classify_ram(
            estimate_memory(ModelTask::Diffusion, bytes, None),
            budget_bytes,
        ),
        installable: true,
        blockers: Vec::new(),
        trust_status: "untested".into(),
        file_url: Some(rewrite_download_url(
            file.download_url.as_deref().unwrap_or_default(),
            host,
        )),
    })
}

pub fn catalog_id_from_inspection(inspection: &ModelInspection) -> &'static str {
    match inspection.model_type.as_deref() {
        Some("sdxl" | "sdxl_turbo") => "sdxl",
        _ => "sd",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{routing::get, Json, Router};
    use serde_json::json;

    #[test]
    fn sd_and_sdxl_base_models_map_to_image_pipelines() {
        assert_eq!(pipeline_for_base_model("SD 1.5"), Some("sd"));
        assert_eq!(pipeline_for_base_model("SD 2.1"), Some("sd"));
        assert_eq!(pipeline_for_base_model("SDXL 1.0"), Some("sdxl"));
        assert_eq!(pipeline_for_base_model("SDXL Turbo"), Some("sdxl"));
        assert_eq!(pipeline_for_base_model("Flux.1 D"), None);
    }

    #[test]
    fn civitai_red_rewrites_download_hosts() {
        assert_eq!(
            rewrite_download_url(
                "https://civitai.com/api/download/models/130072",
                CivitaiHost::Red
            ),
            "https://civitai.red/api/download/models/130072"
        );
        assert_eq!(
            parse_repo("civitai/models/4201@130072"),
            Some((CivitaiHost::Com, 4201, 130072))
        );
        assert_eq!(
            parse_repo("civitai.red/models/4201@130072"),
            Some((CivitaiHost::Red, 4201, 130072))
        );
        assert_eq!(
            parse_repo("https://civitai.com/models/4201@130072"),
            Some((CivitaiHost::Com, 4201, 130072))
        );
    }

    #[tokio::test]
    async fn search_returns_installable_sd_and_sdxl_checkpoints() {
        let server = mock_civitai().await;
        let page = CivitaiClient::new(reqwest::Client::new(), CivitaiHost::Com, None)
            .with_base_url(server)
            .search("realistic", None, 32 * 1024 * 1024 * 1024)
            .await
            .unwrap();
        let names = page
            .items
            .iter()
            .map(|item| item.name.as_str())
            .collect::<Vec<_>>();
        assert_eq!(names, ["Realistic Vision", "DreamShaper XL"]);
        assert_eq!(page.items[0].family, "SD 1.5");
        assert_eq!(page.items[1].family, "SDXL 1.0");
        assert!(page.items.iter().all(|item| item.installable));
        assert_eq!(page.next_cursor.as_deref(), Some("next-token"));
    }

    async fn mock_civitai() -> String {
        let app = Router::new().route(
            "/api/v1/models",
            get(|| async {
                Json(json!({
                    "items": [{
                        "id": 4201,
                        "name": "Realistic Vision",
                        "modelVersions": [{
                            "id": 130072,
                            "name": "V5.1",
                            "baseModel": "SD 1.5",
                            "files": [{
                                "name": "realisticVision.safetensors",
                                "type": "Model",
                                "sizeKB": 1024.0,
                                "downloadUrl": "https://civitai.com/api/download/models/130072",
                                "metadata": { "format": "SafeTensor", "fp": "fp16" }
                            }]
                        }]
                    }, {
                        "id": 1126,
                        "name": "DreamShaper XL",
                        "modelVersions": [{
                            "id": 126688,
                            "name": "SDXL",
                            "baseModel": "SDXL 1.0",
                            "files": [{
                                "name": "dreamshaperXL.safetensors",
                                "type": "Model",
                                "sizeKB": 2048.0,
                                "downloadUrl": "https://civitai.com/api/download/models/126688",
                                "metadata": { "format": "SafeTensor", "fp": "fp16" }
                            }]
                        }]
                    }, {
                        "id": 999,
                        "name": "Flux Dev",
                        "modelVersions": [{
                            "id": 1,
                            "baseModel": "Flux.1 D",
                            "files": [{
                                "name": "flux.safetensors",
                                "type": "Model",
                                "sizeKB": 4096.0,
                                "downloadUrl": "https://civitai.com/api/download/models/1",
                                "metadata": { "format": "SafeTensor" }
                            }]
                        }]
                    }],
                    "metadata": { "nextCursor": "next-token" }
                }))
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        format!("http://{address}")
    }
}
