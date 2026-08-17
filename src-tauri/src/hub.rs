use anyhow::Context;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::catalog::{
    classify_ram, estimate_memory, runtime_for_model_type, CatalogCategory, CatalogEntryView,
    ModelTask, RamFit, IMAGE_TYPES, LLM_TYPES, SPEECH_TYPES, VLM_TYPES,
};

#[derive(Debug, Clone)]
pub struct HubClient {
    client: reqwest::Client,
    pub base_url: String,
    token: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchPage {
    pub items: Vec<CatalogEntryView>,
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelInspection {
    pub repo_id: String,
    pub revision: String,
    pub model_type: Option<String>,
    pub pipeline_tag: Option<String>,
    pub license: Option<String>,
    pub gated: bool,
    pub mlx_format: bool,
    pub download_bytes: u64,
    pub files: Vec<String>,
    pub runtime_engine: Option<String>,
    pub task: Option<String>,
    pub category: Option<CatalogCategory>,
    pub capabilities: Vec<String>,
    pub estimated_memory_bytes: u64,
    pub ram_fit: RamFit,
    pub installable: bool,
    pub blockers: Vec<String>,
    pub trust_status: String,
}

#[derive(Debug, Deserialize)]
struct HfModel {
    id: String,
    #[serde(default)]
    sha: Option<String>,
    #[serde(default)]
    pipeline_tag: Option<String>,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default)]
    gated: HfGated,
    #[serde(default)]
    siblings: Vec<HfSibling>,
    #[serde(default, rename = "cardData")]
    card_data: Option<HfCardData>,
    #[serde(default)]
    config: Option<Value>,
}

#[derive(Debug, Deserialize)]
struct HfCardData {
    #[serde(default)]
    license: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(untagged)]
enum HfGated {
    #[default]
    None,
    Flag(bool),
    Mode(String),
}

impl HfGated {
    fn is_gated(&self) -> bool {
        match self {
            Self::None => false,
            Self::Flag(value) => *value,
            Self::Mode(value) => !value.is_empty() && value != "false",
        }
    }
}

#[derive(Debug, Deserialize, Clone)]
pub struct HfSibling {
    #[serde(default, alias = "rfilename")]
    pub rfilename: String,
    #[serde(default)]
    pub size: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct HfTreeItem {
    path: String,
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    size: Option<u64>,
}

pub fn hub_http_client() -> anyhow::Result<reqwest::Client> {
    reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(30))
        .user_agent("LocalAI-Router/0.1")
        .redirect(reqwest::redirect::Policy::limited(10))
        .build()
        .context("unable to build Hugging Face HTTP client")
}

fn next_page_url(headers: &reqwest::header::HeaderMap) -> Option<String> {
    if let Some(cursor) = headers
        .get("x-next-cursor")
        .and_then(|value| value.to_str().ok())
        .filter(|value| !value.is_empty())
    {
        return Some(format!("cursor:{cursor}"));
    }
    let link = headers.get(reqwest::header::LINK)?.to_str().ok()?;
    link.split(',').find_map(|part| {
        let part = part.trim();
        if part.contains("rel=\"next\"") || part.contains("rel=next") {
            let start = part.find('<')? + 1;
            let end = part.find('>')?;
            Some(part[start..end].to_string())
        } else {
            None
        }
    })
}

impl HubClient {
    pub fn new(
        client: reqwest::Client,
        base_url: impl Into<String>,
        token: Option<String>,
    ) -> Self {
        Self {
            client,
            base_url: base_url.into().trim_end_matches('/').to_owned(),
            token,
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
        let offset = cursor
            .and_then(|value| value.parse::<u32>().ok())
            .unwrap_or(0);
        let mut url = format!(
            "{}/api/models?filter=mlx&limit=20&sort=downloads&direction=-1&full=true&offset={offset}",
            self.base_url
        );
        if !query.trim().is_empty() {
            url.push_str(&format!("&search={}", urlencoding_lite(query.trim())));
        }
        let response = self.request(url).send().await?.error_for_status()?;
        let models: Vec<HfModel> = response.json().await?;
        let next_cursor = (models.len() == 20).then(|| (offset + 20).to_string());
        let items = models
            .into_iter()
            .map(|model| search_view(&model, budget_bytes))
            .collect();
        Ok(SearchPage { items, next_cursor })
    }

    pub async fn inspect(
        &self,
        repo_id: &str,
        revision: Option<&str>,
        budget_bytes: u64,
        has_token: bool,
    ) -> anyhow::Result<ModelInspection> {
        let repo_url = format!("{}/api/models/{repo_id}?full=true", self.base_url);
        let response = self.request(repo_url).send().await?;
        if response.status() == reqwest::StatusCode::UNAUTHORIZED
            || response.status() == reqwest::StatusCode::FORBIDDEN
        {
            anyhow::bail!("this repository is gated; add a Hugging Face token in Settings");
        }
        let model: HfModel = response.error_for_status()?.json().await?;
        let revision = revision
            .map(str::to_owned)
            .or(model.sha.clone())
            .unwrap_or_else(|| "main".into());
        let (files, download_bytes) = self.list_files(repo_id, &revision, &model.siblings).await?;
        let config = self
            .fetch_json(repo_id, &revision, "config.json")
            .await
            .ok()
            .or(model.config.clone());
        let model_type = config
            .as_ref()
            .and_then(|value| value.get("model_type"))
            .and_then(Value::as_str)
            .map(str::to_owned);
        let license = model
            .card_data
            .as_ref()
            .and_then(|card| card.license.clone())
            .or_else(|| {
                model
                    .tags
                    .iter()
                    .find_map(|tag| tag.strip_prefix("license:").map(str::to_owned))
            });
        let mlx_format = model.tags.iter().any(|tag| tag == "mlx")
            || files.iter().any(|file| file.ends_with(".safetensors"));
        let runtime = model_type
            .as_deref()
            .and_then(|value| runtime_for_model_type(value, model.pipeline_tag.as_deref()));
        let mut blockers = Vec::new();
        if model.gated.is_gated() && !has_token {
            blockers.push("this repository is gated; add a Hugging Face token in Settings".into());
        }
        if !mlx_format {
            blockers.push("repository is not an MLX-format model".into());
        }
        if !files.iter().any(|file| file.ends_with("config.json")) {
            blockers.push("repository is missing config.json".into());
        }
        if !files.iter().any(|file| file.ends_with(".safetensors")) {
            blockers.push("repository is missing safetensors weights".into());
        }
        if license.as_deref().unwrap_or("").is_empty() {
            blockers.push("repository does not declare a license".into());
        }
        if runtime.is_none() {
            blockers.push(format!(
                "architecture {} is not supported by the pinned MLX Swift LM runtime",
                model_type.as_deref().unwrap_or("unknown")
            ));
        }
        let (engine, task, model_task) = runtime.unwrap_or(("mlx_chat", "chat", ModelTask::Llm));
        let estimated = estimate_memory(model_task, download_bytes, None);
        let capabilities = match model_task {
            ModelTask::Vlm => vec!["chat".into(), "streaming".into(), "vision".into()],
            ModelTask::Diffusion => vec!["images".into()],
            ModelTask::Tts => vec!["speech".into()],
            ModelTask::Llm => vec!["chat".into(), "streaming".into()],
        };
        Ok(ModelInspection {
            repo_id: repo_id.into(),
            revision,
            model_type,
            pipeline_tag: model.pipeline_tag,
            license,
            gated: model.gated.is_gated(),
            mlx_format,
            download_bytes,
            files,
            runtime_engine: runtime.map(|(engine, _, _)| engine.into()),
            task: Some(task.into()),
            category: Some(match engine {
                "mlx_image" => CatalogCategory::Image,
                "mlx_speech" => CatalogCategory::Speech,
                _ => CatalogCategory::ChatVision,
            }),
            capabilities,
            estimated_memory_bytes: estimated,
            ram_fit: classify_ram(estimated, budget_bytes),
            installable: blockers.is_empty(),
            blockers,
            trust_status: "untested".into(),
        })
    }

    async fn list_files(
        &self,
        repo_id: &str,
        revision: &str,
        siblings: &[HfSibling],
    ) -> anyhow::Result<(Vec<String>, u64)> {
        let mut files = Vec::new();
        let mut total = 0u64;
        let mut url = format!(
            "{}/api/models/{repo_id}/tree/{revision}?recursive=true",
            self.base_url
        );
        let mut first = true;
        for _ in 0..64 {
            let response = self.request(url).send().await?.error_for_status()?;
            let next = next_page_url(response.headers());
            let page: Vec<HfTreeItem> = response.json().await.unwrap_or_default();
            if page.is_empty() && files.is_empty() && first {
                let names = siblings
                    .iter()
                    .map(|item| item.rfilename.clone())
                    .collect::<Vec<_>>();
                let size = siblings.iter().filter_map(|item| item.size).sum();
                return Ok((names, size));
            }
            first = false;
            for item in page {
                if item.kind == "file" {
                    total += item.size.unwrap_or(0);
                    files.push(item.path);
                }
            }
            match next {
                Some(value) if value.starts_with("cursor:") => {
                    url = format!(
                        "{}/api/models/{repo_id}/tree/{revision}?recursive=true&cursor={}",
                        self.base_url,
                        &value[7..]
                    );
                }
                Some(value) => url = value,
                None => break,
            }
        }
        if files.is_empty() {
            files = siblings.iter().map(|item| item.rfilename.clone()).collect();
            total = siblings.iter().filter_map(|item| item.size).sum();
        }
        Ok((files, total))
    }

    pub async fn fetch_json(
        &self,
        repo_id: &str,
        revision: &str,
        path: &str,
    ) -> anyhow::Result<Value> {
        let url = format!("{}/{repo_id}/resolve/{revision}/{path}", self.base_url);
        let response = self.request(url).send().await?.error_for_status()?;
        Ok(response.json().await?)
    }

    pub async fn download_url(&self, repo_id: &str, revision: &str, path: &str) -> String {
        let encoded = path
            .split('/')
            .map(urlencoding_lite)
            .collect::<Vec<_>>()
            .join("/");
        format!(
            "{}/{repo_id}/resolve/{revision}/{encoded}?download=true",
            self.base_url
        )
    }
}

fn search_view(model: &HfModel, budget_bytes: u64) -> CatalogEntryView {
    let download_bytes = model.siblings.iter().filter_map(|item| item.size).sum();
    let model_type = model
        .config
        .as_ref()
        .and_then(|value| value.get("model_type"))
        .and_then(Value::as_str);
    let runtime =
        model_type.and_then(|value| runtime_for_model_type(value, model.pipeline_tag.as_deref()));
    let task = runtime.map(|(_, _, task)| task).unwrap_or(ModelTask::Llm);
    let estimated = estimate_memory(task, download_bytes, None);
    let (engine, task_name) = runtime
        .map(|(engine, task, _)| (engine, task))
        .unwrap_or(("mlx_chat", "chat"));
    CatalogEntryView {
        id: model.id.clone(),
        name: model.id.split('/').next_back().unwrap_or(&model.id).into(),
        family: "Hugging Face".into(),
        repo_id: model.id.clone(),
        category: match engine {
            "mlx_image" => CatalogCategory::Image,
            "mlx_speech" => CatalogCategory::Speech,
            _ => CatalogCategory::ChatVision,
        },
        task: task_name.into(),
        runtime_engine: engine.into(),
        quantization: model
            .tags
            .iter()
            .find(|tag| tag.contains("bit") || *tag == "mlx")
            .cloned()
            .unwrap_or_else(|| "mlx".into()),
        license: model
            .card_data
            .as_ref()
            .and_then(|card| card.license.clone())
            .unwrap_or_else(|| "unknown".into()),
        alias: slug(&model.id),
        capabilities: vec!["chat".into()],
        download_bytes,
        estimated_memory_bytes: estimated,
        ram_fit: classify_ram(estimated, budget_bytes),
        trust_status: "untested".into(),
        installable: false,
        lock_reason: Some(
            "Search hits stay untested until architecture, files and license are inspected.".into(),
        ),
        voices: Vec::new(),
        gated: model.gated.is_gated(),
    }
}

pub fn slug(value: &str) -> String {
    let name = value.split('/').next_back().unwrap_or(value);
    let mut alias = String::new();
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() {
            alias.push(ch.to_ascii_lowercase());
        } else if !alias.ends_with('-') && !alias.is_empty() {
            alias.push('-');
        }
    }
    alias.trim_matches('-').to_owned()
}

fn urlencoding_lite(value: &str) -> String {
    url::form_urlencoded::byte_serialize(value.as_bytes()).collect()
}

pub fn required_weight_files(files: &[String]) -> anyhow::Result<Vec<String>> {
    let allowed = [
        ".json",
        ".safetensors",
        ".model",
        ".txt",
        ".tiktoken",
        ".jinja",
        ".npy",
        ".npz",
    ];
    let selected = files
        .iter()
        .filter(|file| {
            crate::library::safe_model_path(file).is_ok()
                && allowed.iter().any(|extension| file.ends_with(extension))
        })
        .cloned()
        .collect::<Vec<_>>();
    if !selected.iter().any(|file| file.ends_with("config.json")) {
        anyhow::bail!("repository is missing config.json");
    }
    if !selected.iter().any(|file| file.ends_with(".safetensors")) {
        anyhow::bail!("repository is missing safetensors weights");
    }
    Ok(selected)
}

pub fn supported_architecture(model_type: &str) -> bool {
    let value = model_type.to_ascii_lowercase();
    LLM_TYPES.contains(&value.as_str())
        || VLM_TYPES.contains(&value.as_str())
        || IMAGE_TYPES.contains(&value.as_str())
        || SPEECH_TYPES.contains(&value.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        extract::Query,
        http::{header::HeaderMap, StatusCode},
        response::IntoResponse,
        routing::get,
        Json, Router,
    };
    use serde_json::json;
    use std::{collections::HashMap, sync::Arc};
    use tokio::sync::Mutex;

    #[tokio::test]
    async fn search_paginates_mlx_models_and_marks_hits_untested() {
        let server = mock_hub(MockHub::default()).await;
        let hub = HubClient::new(reqwest::Client::new(), server, None);
        let page = hub.search("qwen", None, gb(16)).await.unwrap();
        assert_eq!(page.items.len(), 20);
        assert_eq!(page.next_cursor.as_deref(), Some("20"));
        assert!(page
            .items
            .iter()
            .all(|item| item.trust_status == "untested"));
        assert!(page.items.iter().all(|item| !item.installable));
        let second = hub
            .search("qwen", page.next_cursor.as_deref(), gb(16))
            .await
            .unwrap();
        assert_eq!(second.items[0].repo_id, "org/model-20");
        assert!(second.next_cursor.is_none());
    }

    #[tokio::test]
    async fn inspect_rejects_unknown_architectures_gated_repos_and_missing_files() {
        let mut state = MockHub::default();
        state.models.insert(
            "org/unknown".into(),
            json!({"id":"org/unknown","sha":"abc","tags":["mlx"],"pipeline_tag":"text-generation","cardData":{"license":"mit"},"siblings":[{"rfilename":"config.json","size":12},{"rfilename":"model.safetensors","size":100}]}),
        );
        state
            .configs
            .insert("org/unknown".into(), json!({"model_type":"totally_new"}));
        state.trees.insert(
            "org/unknown".into(),
            vec![
                json!({"path":"config.json","type":"file","size":12}),
                json!({"path":"model.safetensors","type":"file","size":100}),
            ],
        );
        state.models.insert(
            "org/gated".into(),
            json!({"id":"org/gated","sha":"def","gated":true,"tags":["mlx"],"pipeline_tag":"text-generation","cardData":{"license":"mit"}}),
        );
        state
            .configs
            .insert("org/gated".into(), json!({"model_type":"llama"}));
        state.trees.insert(
            "org/gated".into(),
            vec![
                json!({"path":"config.json","type":"file","size":1}),
                json!({"path":"model.safetensors","type":"file","size":1}),
            ],
        );
        let server = mock_hub(state).await;
        let hub = HubClient::new(reqwest::Client::new(), server, None);
        let unknown = hub
            .inspect("org/unknown", None, gb(16), true)
            .await
            .unwrap();
        assert!(!unknown.installable);
        assert!(unknown.blockers.join(" ").contains("not supported"));
        assert_eq!(unknown.revision, "abc");
        let gated = hub.inspect("org/gated", None, gb(16), false).await.unwrap();
        assert!(gated.gated);
        assert!(!gated.installable);
    }

    #[tokio::test]
    async fn inspect_walks_tree_cursors_and_pins_the_commit() {
        let mut state = MockHub::default();
        state.models.insert(
            "org/ok".into(),
            json!({"id":"org/ok","sha":"commit-1","tags":["mlx"],"pipeline_tag":"text-generation","cardData":{"license":"apache-2.0"}}),
        );
        state
            .configs
            .insert("org/ok".into(), json!({"model_type":"qwen3"}));
        state.paged_trees.insert(
            "org/ok".into(),
            vec![
                vec![json!({"path":"config.json","type":"file","size":20})],
                vec![json!({"path":"model.safetensors","type":"file","size":80})],
            ],
        );
        let server = mock_hub(state).await;
        let hub = HubClient::new(reqwest::Client::new(), server, None);
        let inspected = hub.inspect("org/ok", None, gb(16), true).await.unwrap();
        assert!(inspected.installable);
        assert_eq!(inspected.revision, "commit-1");
        assert_eq!(inspected.download_bytes, 100);
        assert!(inspected.files.contains(&"model.safetensors".into()));
    }

    fn gb(value: u64) -> u64 {
        value * 1024 * 1024 * 1024
    }

    #[derive(Default, Clone)]
    struct MockHub {
        models: HashMap<String, Value>,
        configs: HashMap<String, Value>,
        trees: HashMap<String, Vec<Value>>,
        paged_trees: HashMap<String, Vec<Vec<Value>>>,
    }

    async fn mock_hub(state: MockHub) -> String {
        let state = Arc::new(Mutex::new(state));
        let app = Router::new()
            .route("/api/models", get({
                let state = state.clone();
                move |Query(query): Query<HashMap<String, String>>| {
                    let state = state.clone();
                    async move {
                        let offset = query.get("offset").and_then(|value| value.parse::<usize>().ok()).unwrap_or(0);
                        let _guard = state.lock().await;
                        let items = (offset..offset + 20)
                            .take_while(|index| *index < 25)
                            .map(|index| json!({"id":format!("org/model-{index}"),"tags":["mlx"],"siblings":[{"rfilename":"config.json","size":1}]}))
                            .collect::<Vec<_>>();
                        Json(items)
                    }
                }
            }))
            .route("/api/models/{*rest}", get({
                let state = state.clone();
                move |axum::extract::Path(rest): axum::extract::Path<String>, Query(query): Query<HashMap<String, String>>| {
                    let state = state.clone();
                    async move {
                        let state = state.lock().await;
                        if let Some(repo) = rest.strip_suffix("/tree/main").or_else(|| rest.strip_suffix("/tree/commit-1")) {
                            let repo = repo.trim_end_matches("/tree/abc").trim_end_matches("/tree/def");
                            if let Some(pages) = state.paged_trees.get(repo) {
                                let page = if query.get("cursor").map(String::as_str) == Some("1") { 1 } else { 0 };
                                let mut headers = HeaderMap::new();
                                if page == 0 {
                                    headers.insert("x-next-cursor", "1".parse().unwrap());
                                }
                                return (StatusCode::OK, headers, Json(pages[page].clone())).into_response();
                            }
                            if let Some(tree) = state.trees.get(repo) {
                                return Json(tree.clone()).into_response();
                            }
                        }
                        let repo = rest.split('/').take(2).collect::<Vec<_>>().join("/");
                        if let Some(model) = state.models.get(&repo) {
                            return Json(model.clone()).into_response();
                        }
                        StatusCode::NOT_FOUND.into_response()
                    }
                }
            }))
            .route("/{*path}", get({
                let state = state.clone();
                move |axum::extract::Path(path): axum::extract::Path<String>| {
                    let state = state.clone();
                    async move {
                        let state = state.lock().await;
                        if let Some(repo) = path.strip_suffix("/resolve/commit-1/config.json")
                            .or_else(|| path.strip_suffix("/resolve/abc/config.json"))
                            .or_else(|| path.strip_suffix("/resolve/def/config.json"))
                            .or_else(|| path.strip_suffix("/resolve/main/config.json"))
                        {
                            if let Some(config) = state.configs.get(repo) {
                                return Json(config.clone()).into_response();
                            }
                        }
                        StatusCode::NOT_FOUND.into_response()
                    }
                }
            }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        format!("http://{address}")
    }
}
