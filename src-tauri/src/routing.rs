use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use regex::RegexBuilder;
use serde::{Deserialize, Serialize};

use crate::{
    domain::{supports_capability as advertised_capability, ModelRoute, RouteRole, TargetKind},
    protocol::{CanonicalRequest, ContentBlock},
    storage::Store,
};

pub const ROUTING_POLICY_VERSION: u8 = 1;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PolicyStatus {
    Draft,
    Shadow,
    Active,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum RoutingMode {
    #[default]
    Fixed,
    Adaptive,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PrivacyMode {
    LocalOnly,
    LocalPreferred,
    CloudAllowed,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RoutingWeights {
    pub quality: f64,
    pub cost: f64,
    pub latency: f64,
    pub reliability: f64,
    pub locality: f64,
}

impl Default for RoutingWeights {
    fn default() -> Self {
        Self {
            quality: 0.55,
            cost: 0.15,
            latency: 0.15,
            reliability: 0.10,
            locality: 0.05,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct TaskRule {
    pub id: String,
    pub task: String,
    pub priority: i64,
    pub endpoint_contains: Option<String>,
    pub has_tools: Option<bool>,
    pub modalities_any: Vec<String>,
    pub reasoning: Option<bool>,
    pub min_input_tokens: Option<u64>,
    pub max_input_tokens: Option<u64>,
    pub text_pattern: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RoutingPolicy {
    pub version: u8,
    pub alias: String,
    #[serde(default)]
    pub mode: RoutingMode,
    pub status: PolicyStatus,
    pub privacy: PrivacyMode,
    pub default_task: String,
    pub weights: RoutingWeights,
    pub max_estimated_cost_usd: Option<f64>,
    pub preferred_latency_ms: u64,
    pub preferred_cost_usd: f64,
    pub rules: Vec<TaskRule>,
    #[serde(default)]
    pub candidate_target_ids: Vec<String>,
}

impl RoutingPolicy {
    pub fn new(alias: impl Into<String>) -> Self {
        Self {
            version: ROUTING_POLICY_VERSION,
            alias: alias.into(),
            mode: RoutingMode::Fixed,
            status: PolicyStatus::Draft,
            privacy: PrivacyMode::LocalPreferred,
            default_task: "general".into(),
            weights: RoutingWeights::default(),
            max_estimated_cost_usd: None,
            preferred_latency_ms: 2_000,
            preferred_cost_usd: 0.01,
            rules: default_task_rules(),
            candidate_target_ids: Vec::new(),
        }
    }

    pub fn validate(&self, known_tasks: &[String]) -> Result<(), String> {
        if self.version != ROUTING_POLICY_VERSION || self.alias.trim().is_empty() {
            return Err("invalid routing policy version or alias".into());
        }
        if !known_tasks.contains(&self.default_task) {
            return Err(format!("unknown default task: {}", self.default_task));
        }
        if !self.preferred_cost_usd.is_finite()
            || self.preferred_cost_usd <= 0.0
            || self.preferred_latency_ms == 0
        {
            return Err("routing cost and latency references must be positive".into());
        }
        if [
            self.weights.quality,
            self.weights.cost,
            self.weights.latency,
            self.weights.reliability,
            self.weights.locality,
        ]
        .into_iter()
        .any(|value| !value.is_finite() || value < 0.0)
        {
            return Err("routing weights must be finite and non-negative".into());
        }
        if self
            .max_estimated_cost_usd
            .is_some_and(|value| !value.is_finite() || value < 0.0)
        {
            return Err("maximum estimated cost must be non-negative".into());
        }
        let unique_candidates = self
            .candidate_target_ids
            .iter()
            .collect::<std::collections::HashSet<_>>();
        if unique_candidates.len() != self.candidate_target_ids.len() {
            return Err("routing policy candidate targets must be unique".into());
        }
        if self.mode == RoutingMode::Adaptive && self.candidate_target_ids.is_empty() {
            return Err("adaptive routing policy requires at least one candidate target".into());
        }
        for rule in &self.rules {
            if rule.id.trim().is_empty() || !known_tasks.contains(&rule.task) {
                return Err(format!("invalid routing rule: {}", rule.id));
            }
            if let Some(pattern) = rule.text_pattern.as_deref() {
                compile_rule_pattern(&rule.id, pattern).map_err(|_| {
                    format!("invalid pattern in routing rule {}: {pattern}", rule.id)
                })?;
            }
        }
        Ok(())
    }

    pub fn retain_route_candidates(&mut self, hop_ids: &[String]) {
        self.candidate_target_ids
            .retain(|id| hop_ids.iter().any(|hop| hop == id));
    }
}

pub fn default_task_rules() -> Vec<TaskRule> {
    vec![
        TaskRule { id: "builtin-tools".into(), task: "tool_use".into(), priority: 10, has_tools: Some(true), ..Default::default() },
        TaskRule { id: "builtin-audio-video".into(), task: "audio_video".into(), priority: 20, modalities_any: vec!["audio".into(), "video".into()], ..Default::default() },
        TaskRule { id: "builtin-vision".into(), task: "vision".into(), priority: 30, modalities_any: vec!["vision".into()], ..Default::default() },
        TaskRule { id: "builtin-reasoning".into(), task: "reasoning".into(), priority: 40, reasoning: Some(true), ..Default::default() },
        TaskRule { id: "builtin-coding".into(), task: "coding".into(), priority: 50, text_pattern: Some(r"\b(code|coding|function|class|debug|refactor|rust|python|typescript|javascript|sql|programm|implement)\b".into()), ..Default::default() },
        TaskRule { id: "builtin-summary".into(), task: "summarization".into(), priority: 60, text_pattern: Some(r"\b(summarize|summary|summarise|zusammenfass|tl;?dr)\b".into()), ..Default::default() },
        TaskRule { id: "builtin-extraction".into(), task: "extraction".into(), priority: 70, text_pattern: Some(r"\b(extract|parse|entities|extrahier|strukturier)\b".into()), ..Default::default() },
        TaskRule { id: "builtin-translation".into(), task: "translation".into(), priority: 80, text_pattern: Some(r"\b(translate|translation|übersetz)\b".into()), ..Default::default() },
        TaskRule { id: "builtin-creative".into(), task: "creative".into(), priority: 90, text_pattern: Some(r"\b(story|poem|creative|geschichte|gedicht|brainstorm)\b".into()), ..Default::default() },
    ]
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TargetRoutingProfile {
    pub version: u8,
    pub target_id: String,
    pub context_window: u64,
    pub input_price_per_million: Option<f64>,
    pub output_price_per_million: Option<f64>,
    pub latency_prior_ms: u64,
    pub reliability_prior: f64,
    pub task_quality: BTreeMap<String, f64>,
}

impl TargetRoutingProfile {
    pub fn neutral(target_id: impl Into<String>, kind: TargetKind) -> Self {
        let mut task_quality = BTreeMap::new();
        task_quality.insert("general".into(), 50.0);
        let local = !matches!(kind, TargetKind::Cloud);
        Self {
            version: ROUTING_POLICY_VERSION,
            target_id: target_id.into(),
            context_window: 8_192,
            input_price_per_million: local.then_some(0.0),
            output_price_per_million: local.then_some(0.0),
            latency_prior_ms: if local { 1_500 } else { 2_000 },
            reliability_prior: 0.95,
            task_quality,
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.version != ROUTING_POLICY_VERSION
            || self.target_id.trim().is_empty()
            || self.context_window == 0
            || self.latency_prior_ms == 0
        {
            return Err("invalid target routing profile".into());
        }
        if !(0.0..=1.0).contains(&self.reliability_prior)
            || self
                .task_quality
                .values()
                .any(|value| !value.is_finite() || !(0.0..=100.0).contains(value))
            || [self.input_price_per_million, self.output_price_per_million]
                .into_iter()
                .flatten()
                .any(|value| !value.is_finite() || value < 0.0)
        {
            return Err("routing profile values are out of range".into());
        }
        Ok(())
    }

    pub fn for_target(target: &crate::storage::ModelTarget) -> Self {
        let meta = crate::model_catalog::resolve_model_metadata(&target.provider_model, None);
        let mut profile = Self::neutral(&target.id, target.kind.clone());
        profile.context_window = meta.context_window;
        if matches!(target.kind, TargetKind::Cloud) {
            profile.input_price_per_million = meta.input_price_per_million;
            profile.output_price_per_million = meta.output_price_per_million;
        }
        if !meta.task_quality.is_empty() {
            profile.task_quality = meta.task_quality;
        }
        profile
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RoutingTaskDefinition {
    pub id: String,
    pub label: String,
    pub builtin: bool,
}

pub fn builtin_tasks() -> Vec<RoutingTaskDefinition> {
    builtin_task_ids()
        .iter()
        .map(|id| RoutingTaskDefinition {
            id: (*id).into(),
            label: id.replace('_', " "),
            builtin: true,
        })
        .collect()
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RoutingAttemptRecord {
    pub id: String,
    pub request_id: String,
    pub created_at: DateTime<Utc>,
    pub alias: String,
    pub task: String,
    pub task_source: String,
    pub target_id: String,
    pub routing_mode: String,
    pub status: u16,
    pub transient_failure: bool,
    pub retry_after_until: Option<DateTime<Utc>>,
    pub latency_ms: u64,
    pub ttft_ms: Option<u64>,
    pub streaming: bool,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub estimated_cost_usd: Option<f64>,
    pub cost_verified: bool,
    pub score: Option<ScoreComponents>,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RoutingConfigExport {
    pub schema: String,
    pub tasks: Vec<RoutingTaskDefinition>,
    pub profiles: Vec<TargetRoutingProfile>,
    pub policies: Vec<RoutingPolicy>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RoutingEvaluation {
    pub alias: String,
    pub mode: String,
    pub task: String,
    pub task_source: String,
    #[serde(default)]
    pub task_rule_id: Option<String>,
    pub decision: RoutingDecision,
    pub ordered_target_ids: Vec<String>,
    #[serde(default)]
    pub primary_target_ids: Vec<String>,
    #[serde(default)]
    pub fallback_target_ids: Vec<String>,
    pub shadow_target_id: Option<String>,
    pub half_open_target_ids: Vec<String>,
    pub estimated_input_tokens: u64,
    pub peer_latency_ms: Option<u64>,
}

impl RoutingEvaluation {
    pub fn is_fallback_hop(&self, target_id: &str) -> bool {
        self.fallback_target_ids.iter().any(|id| id == target_id)
    }

    pub fn has_later_hop(&self, target_id: &str) -> bool {
        self.hop_index(target_id)
            .is_some_and(|index| index + 1 < self.ordered_target_ids.len())
    }

    pub fn has_later_primary(&self, target_id: &str) -> bool {
        let Some(index) = self.hop_index(target_id) else {
            return false;
        };
        self.ordered_target_ids[index + 1..]
            .iter()
            .any(|id| self.primary_target_ids.iter().any(|primary| primary == id))
    }

    fn hop_index(&self, target_id: &str) -> Option<usize> {
        self.ordered_target_ids
            .iter()
            .position(|id| id == target_id)
    }
}

pub struct RouteEvaluationInput<'a> {
    pub policy: Option<&'a RoutingPolicy>,
    pub explicit_task: Option<&'a str>,
    pub endpoint: &'a str,
    pub canonical: Option<&'a CanonicalRequest>,
    pub required_capabilities: Vec<String>,
    pub streaming: bool,
}

pub async fn evaluate_route(
    store: &Store,
    route: &ModelRoute,
    input: RouteEvaluationInput<'_>,
) -> anyhow::Result<RoutingEvaluation> {
    let fallback_policy = RoutingPolicy::new(&route.alias);
    let policy = input.policy.unwrap_or(&fallback_policy);
    let mut tasks = builtin_tasks();
    tasks.extend(store.custom_routing_tasks().await?);
    let known = tasks.into_iter().map(|task| task.id).collect::<Vec<_>>();
    let signals = task_signals(input.endpoint, input.canonical);
    let selected = determine_task(policy, input.explicit_task, &known, &signals)
        .map_err(anyhow::Error::msg)?;
    let request = RoutingRequest {
        required_capabilities: input.required_capabilities,
        estimated_input_tokens: signals.estimated_input_tokens,
        max_output_tokens: input
            .canonical
            .and_then(|request| request.max_tokens)
            .unwrap_or(4_096),
    };
    let mut scoped_route = route.clone();
    if policy.mode == RoutingMode::Adaptive
        && policy.status == PolicyStatus::Active
        && !policy.candidate_target_ids.is_empty()
    {
        scoped_route
            .targets
            .retain(|hop| hop.role.is_fallback() || policy.candidate_target_ids.contains(&hop.id));
    }
    let expanded =
        crate::public_models::expand_route_targets(store, &scoped_route, &mut Default::default())
            .await?;
    let mut primary_candidates = Vec::new();
    let mut fallback_candidates = Vec::new();
    let mut half_open_target_ids = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for route_target in &expanded {
        if !seen.insert(route_target.id.clone()) {
            continue;
        }
        let Some(target) = store.target(&route_target.id).await? else {
            continue;
        };
        let profile = store
            .target_routing_profile(&target.id)
            .await?
            .unwrap_or_else(|| TargetRoutingProfile::for_target(&target));
        let stats = store
            .routing_stats(&target.id, &selected.task, input.streaming)
            .await?;
        if stats.half_open_required {
            half_open_target_ids.push(target.id.clone());
        }
        let force_tool_support = crate::tool_emulation::force_tool_support(&target);
        let candidate = CandidateInput {
            target_id: target.id,
            kind: target.kind,
            role: route_target.role,
            priority: route_target.priority,
            enabled: target.enabled && route_target.enabled,
            capabilities: target.capabilities,
            force_tool_support,
            profile,
            stats,
        };
        if candidate.role.is_fallback() {
            fallback_candidates.push(candidate);
        } else {
            primary_candidates.push(candidate);
        }
    }
    let recent_by_target: std::collections::HashMap<_, _> = primary_candidates
        .iter()
        .chain(fallback_candidates.iter())
        .map(|candidate| {
            (
                candidate.target_id.clone(),
                candidate.stats.recent_latency_ms,
            )
        })
        .collect();
    let mut decision = rank_candidates(policy, &selected.task, &request, primary_candidates);
    fallback_candidates.sort_by_key(|candidate| candidate.priority);
    let mut fallback_order = Vec::new();
    for candidate in fallback_candidates {
        if let Some(reason) = reserve_exclude_reason(policy, &candidate) {
            decision.excluded.push(ExcludedCandidate {
                target_id: candidate.target_id,
                reason: reason.into(),
            });
            continue;
        }
        fallback_order.push(candidate.target_id);
    }
    let blocked: std::collections::HashSet<_> = decision
        .excluded
        .iter()
        .filter(|candidate| candidate.reason == "circuit_open" || candidate.reason == "slow")
        .map(|candidate| candidate.target_id.clone())
        .collect();
    let ranked_primary_ids = decision
        .ranked
        .iter()
        .map(|candidate| candidate.target_id.clone())
        .collect::<Vec<_>>();
    let primary_fixed = expanded
        .iter()
        .filter(|target| !target.role.is_fallback() && !blocked.contains(&target.id))
        .map(|target| target.id.clone())
        .collect::<Vec<_>>();
    let peer_latency_ms = crate::domain::median_u64(
        &decision
            .ranked
            .iter()
            .filter_map(|candidate| {
                recent_by_target
                    .get(&candidate.target_id)
                    .copied()
                    .flatten()
            })
            .collect::<Vec<_>>(),
    );
    let (mode, primary_target_ids, shadow_target_id) = match (policy.mode, policy.status) {
        (RoutingMode::Adaptive, PolicyStatus::Active) => ("adaptive", ranked_primary_ids, None),
        (RoutingMode::Adaptive, PolicyStatus::Shadow) => (
            "shadow",
            primary_fixed,
            ranked_primary_ids
                .first()
                .cloned()
                .or_else(|| fallback_order.first().cloned()),
        ),
        _ => ("fixed", primary_fixed, None),
    };
    let fallback_target_ids = fallback_order;
    let ordered_target_ids = primary_target_ids
        .iter()
        .cloned()
        .chain(fallback_target_ids.iter().cloned())
        .collect::<Vec<_>>();
    Ok(RoutingEvaluation {
        alias: route.alias.clone(),
        mode: mode.into(),
        task: selected.task,
        task_source: selected.source.into(),
        task_rule_id: selected.rule_id,
        decision,
        ordered_target_ids,
        primary_target_ids,
        fallback_target_ids,
        shadow_target_id,
        half_open_target_ids,
        estimated_input_tokens: signals.estimated_input_tokens,
        peer_latency_ms,
    })
}

fn task_signals(endpoint: &str, request: Option<&CanonicalRequest>) -> TaskSignals {
    let Some(request) = request else {
        return TaskSignals {
            endpoint: endpoint.into(),
            has_tools: false,
            modalities: vec![],
            reasoning: false,
            estimated_input_tokens: 0,
            text: String::new(),
        };
    };
    let mut text = String::new();
    let mut estimated_chars = 0_u64;
    let mut modalities = Vec::new();
    for block in request
        .system
        .iter()
        .chain(request.messages.iter().flat_map(|message| &message.content))
    {
        match block {
            ContentBlock::Text { text: value } | ContentBlock::Reasoning { text: value } => {
                estimated_chars = estimated_chars.saturating_add(value.chars().count() as u64);
                if text.len() < 32 * 1024 {
                    let remaining = 32 * 1024 - text.len();
                    text.push_str(&value.chars().take(remaining).collect::<String>());
                    text.push('\n');
                }
            }
            ContentBlock::Image { .. } => {
                modalities.push("vision".into());
                estimated_chars = estimated_chars.saturating_add(4_096);
            }
            ContentBlock::Audio { .. } => {
                modalities.push("audio".into());
                estimated_chars = estimated_chars.saturating_add(16_384);
            }
            ContentBlock::Video { .. } => {
                modalities.push("video".into());
                estimated_chars = estimated_chars.saturating_add(65_536);
            }
            ContentBlock::ToolUse { input, name, .. } => {
                estimated_chars = estimated_chars
                    .saturating_add(name.chars().count() as u64)
                    .saturating_add(input.to_string().chars().count() as u64);
            }
            ContentBlock::ToolResult { content, .. } => {
                estimated_chars =
                    estimated_chars.saturating_add(content.to_string().chars().count() as u64);
            }
        }
    }
    for tool in &request.tools {
        estimated_chars = estimated_chars
            .saturating_add(tool.name.chars().count() as u64)
            .saturating_add(
                tool.description
                    .as_deref()
                    .map_or(0, |value| value.chars().count() as u64),
            )
            .saturating_add(tool.input_schema.to_string().chars().count() as u64);
    }
    modalities.sort();
    modalities.dedup();
    let estimated_input_tokens = estimated_chars.saturating_add(2) / 3;
    TaskSignals {
        endpoint: endpoint.into(),
        has_tools: !request.tools.is_empty(),
        modalities,
        reasoning: request.reasoning.is_some(),
        estimated_input_tokens,
        text,
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct TaskSignals {
    pub endpoint: String,
    pub has_tools: bool,
    pub modalities: Vec<String>,
    pub reasoning: bool,
    pub estimated_input_tokens: u64,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TaskSelection {
    pub task: String,
    pub source: &'static str,
    pub rule_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RoutingStats {
    pub samples: usize,
    pub p90_latency_ms: u64,
    pub recent_latency_ms: Option<u64>,
    pub reliability: f64,
    pub circuit_open: bool,
    pub half_open_required: bool,
}

impl Default for RoutingStats {
    fn default() -> Self {
        Self {
            samples: 0,
            p90_latency_ms: 0,
            recent_latency_ms: None,
            reliability: 0.95,
            circuit_open: false,
            half_open_required: false,
        }
    }
}

#[derive(Debug, Clone)]
pub struct CandidateInput {
    pub target_id: String,
    pub kind: TargetKind,
    pub role: RouteRole,
    pub priority: i64,
    pub enabled: bool,
    pub capabilities: Vec<String>,
    pub force_tool_support: bool,
    pub profile: TargetRoutingProfile,
    pub stats: RoutingStats,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RoutingRequest {
    pub required_capabilities: Vec<String>,
    pub estimated_input_tokens: u64,
    pub max_output_tokens: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ScoreComponents {
    pub quality: f64,
    pub cost: f64,
    pub latency: f64,
    pub reliability: f64,
    pub locality: f64,
    pub total: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RankedCandidate {
    pub target_id: String,
    pub score: ScoreComponents,
    pub estimated_cost_usd: Option<f64>,
    pub cost_verified: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ExcludedCandidate {
    pub target_id: String,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RoutingDecision {
    pub task: String,
    pub ranked: Vec<RankedCandidate>,
    pub excluded: Vec<ExcludedCandidate>,
}

pub fn rank_candidates(
    policy: &RoutingPolicy,
    task: &str,
    request: &RoutingRequest,
    candidates: Vec<CandidateInput>,
) -> RoutingDecision {
    let weights = normalized_weights(&policy.weights);
    let mut pending = Vec::new();
    let mut excluded = Vec::new();
    for candidate in candidates {
        let exclude = hard_exclude_reason(policy, request, &candidate);
        if let Some(reason) = exclude {
            excluded.push(ExcludedCandidate {
                target_id: candidate.target_id,
                reason: reason.into(),
            });
            continue;
        }
        pending.push(candidate);
    }
    let peer_median = crate::domain::median_u64(
        &pending
            .iter()
            .filter_map(|candidate| candidate.stats.recent_latency_ms)
            .collect::<Vec<_>>(),
    );
    let peer_count = pending
        .iter()
        .filter(|candidate| candidate.stats.recent_latency_ms.is_some())
        .count();
    let mut ranked = Vec::new();
    for candidate in pending {
        if peer_count >= 2
            && peer_median.is_some_and(|median| {
                candidate
                    .stats
                    .recent_latency_ms
                    .is_some_and(|latency| crate::domain::is_slow_outlier(latency, median))
            })
        {
            excluded.push(ExcludedCandidate {
                target_id: candidate.target_id,
                reason: "slow".into(),
            });
            continue;
        }
        let estimated_cost_usd = match (
            candidate.profile.input_price_per_million,
            candidate.profile.output_price_per_million,
        ) {
            (Some(input), Some(output)) => Some(
                request.estimated_input_tokens as f64 * input / 1_000_000.0
                    + request.max_output_tokens as f64 * output / 1_000_000.0,
            ),
            _ => None,
        };
        if estimated_cost_usd
            .zip(policy.max_estimated_cost_usd)
            .is_some_and(|(cost, limit)| cost > limit)
        {
            excluded.push(ExcludedCandidate {
                target_id: candidate.target_id,
                reason: "cost_limit".into(),
            });
            continue;
        }
        let quality = candidate
            .profile
            .task_quality
            .get(task)
            .or_else(|| candidate.profile.task_quality.get("general"))
            .copied()
            .unwrap_or(50.0)
            .clamp(0.0, 100.0)
            / 100.0;
        let latency_ms = if candidate.stats.samples >= 10 {
            candidate.stats.p90_latency_ms
        } else {
            candidate.profile.latency_prior_ms
        };
        let latency = 1.0 / (1.0 + latency_ms as f64 / policy.preferred_latency_ms.max(1) as f64);
        let cost = estimated_cost_usd
            .map(|value| 1.0 / (1.0 + value / policy.preferred_cost_usd.max(0.000_001)))
            .unwrap_or(0.0);
        let reliability = if candidate.stats.samples >= 10 {
            candidate.stats.reliability
        } else {
            candidate.profile.reliability_prior
        }
        .clamp(0.0, 1.0);
        let locality = match policy.privacy {
            PrivacyMode::LocalPreferred if !matches!(candidate.kind, TargetKind::Cloud) => 1.0,
            PrivacyMode::LocalPreferred => 0.0,
            _ => 0.5,
        };
        let total = weights.quality * quality
            + weights.cost * cost
            + weights.latency * latency
            + weights.reliability * reliability
            + weights.locality * locality;
        ranked.push((
            candidate.priority,
            RankedCandidate {
                target_id: candidate.target_id,
                score: ScoreComponents {
                    quality,
                    cost,
                    latency,
                    reliability,
                    locality,
                    total,
                },
                estimated_cost_usd,
                cost_verified: estimated_cost_usd.is_some(),
            },
        ));
    }
    ranked.sort_by(|(priority_a, a), (priority_b, b)| {
        b.cost_verified
            .cmp(&a.cost_verified)
            .then_with(|| b.score.total.total_cmp(&a.score.total))
            .then_with(|| priority_a.cmp(priority_b))
            .then_with(|| a.target_id.cmp(&b.target_id))
    });
    RoutingDecision {
        task: task.into(),
        ranked: ranked.into_iter().map(|(_, candidate)| candidate).collect(),
        excluded,
    }
}

fn hard_exclude_reason(
    policy: &RoutingPolicy,
    request: &RoutingRequest,
    candidate: &CandidateInput,
) -> Option<&'static str> {
    if !candidate.enabled {
        Some("disabled")
    } else if candidate.stats.circuit_open {
        Some("circuit_open")
    } else if matches!(policy.privacy, PrivacyMode::LocalOnly)
        && matches!(candidate.kind, TargetKind::Cloud)
    {
        Some("privacy_local_only")
    } else if request
        .estimated_input_tokens
        .saturating_add(request.max_output_tokens)
        > candidate.profile.context_window
    {
        Some("context_window")
    } else if request
        .required_capabilities
        .iter()
        .any(|required| !supports_capability(candidate, required))
    {
        Some("capability")
    } else {
        None
    }
}

fn reserve_exclude_reason(
    policy: &RoutingPolicy,
    candidate: &CandidateInput,
) -> Option<&'static str> {
    if !candidate.enabled {
        Some("disabled")
    } else if candidate.stats.circuit_open {
        Some("circuit_open")
    } else if matches!(policy.privacy, PrivacyMode::LocalOnly)
        && matches!(candidate.kind, TargetKind::Cloud)
    {
        Some("privacy_local_only")
    } else {
        None
    }
}

fn supports_capability(candidate: &CandidateInput, required: &str) -> bool {
    if required == "tools" && candidate.kind == TargetKind::Mlx && !candidate.force_tool_support {
        return false;
    }
    advertised_capability(candidate.kind, &candidate.capabilities, required)
}

fn normalized_weights(weights: &RoutingWeights) -> RoutingWeights {
    let values = [
        weights.quality,
        weights.cost,
        weights.latency,
        weights.reliability,
        weights.locality,
    ]
    .map(|value| value.max(0.0));
    let total = values.iter().sum::<f64>();
    if total <= f64::EPSILON {
        return RoutingWeights::default();
    }
    RoutingWeights {
        quality: values[0] / total,
        cost: values[1] / total,
        latency: values[2] / total,
        reliability: values[3] / total,
        locality: values[4] / total,
    }
}

pub fn builtin_task_ids() -> &'static [&'static str] {
    &[
        "general",
        "coding",
        "reasoning",
        "summarization",
        "extraction",
        "translation",
        "creative",
        "tool_use",
        "vision",
        "audio_video",
    ]
}

pub fn determine_task(
    policy: &RoutingPolicy,
    explicit: Option<&str>,
    known_tasks: &[String],
    signals: &TaskSignals,
) -> Result<TaskSelection, String> {
    if let Some(task) = explicit {
        if !known_tasks.iter().any(|known| known == task) {
            return Err(format!("unknown routing task: {task}"));
        }
        return Ok(TaskSelection {
            task: task.to_owned(),
            source: "header",
            rule_id: None,
        });
    }
    let mut rules = policy.rules.iter().collect::<Vec<_>>();
    rules.sort_by_key(|rule| (rule.priority, rule.id.as_str()));
    for rule in rules {
        if rule_matches(rule, signals)? {
            return Ok(TaskSelection {
                task: rule.task.clone(),
                source: "rule",
                rule_id: Some(rule.id.clone()),
            });
        }
    }
    Ok(TaskSelection {
        task: policy.default_task.clone(),
        source: "default",
        rule_id: None,
    })
}

fn rule_matches(rule: &TaskRule, signals: &TaskSignals) -> Result<bool, String> {
    if rule
        .endpoint_contains
        .as_ref()
        .is_some_and(|value| !signals.endpoint.contains(value))
        || rule
            .has_tools
            .is_some_and(|value| value != signals.has_tools)
        || rule
            .reasoning
            .is_some_and(|value| value != signals.reasoning)
        || rule
            .min_input_tokens
            .is_some_and(|value| signals.estimated_input_tokens < value)
        || rule
            .max_input_tokens
            .is_some_and(|value| signals.estimated_input_tokens > value)
        || (!rule.modalities_any.is_empty()
            && !rule
                .modalities_any
                .iter()
                .any(|value| signals.modalities.contains(value)))
    {
        return Ok(false);
    }
    if let Some(pattern) = rule.text_pattern.as_deref() {
        let regex = compile_rule_pattern(&rule.id, pattern)?;
        if !regex.is_match(&signals.text) {
            return Ok(false);
        }
    }
    Ok(true)
}

fn compile_rule_pattern(id: &str, pattern: &str) -> Result<regex::Regex, String> {
    if pattern.len() > 512 {
        return Err(format!("routing rule {id} pattern is too long"));
    }
    RegexBuilder::new(pattern)
        .case_insensitive(true)
        .size_limit(256 * 1024)
        .build()
        .map_err(|error| format!("routing rule {id} has an invalid pattern: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retain_route_candidates_drops_ids_that_are_not_alias_hops() {
        let mut policy = RoutingPolicy::new("assistant");
        policy.candidate_target_ids = vec![
            "cloud".into(),
            "dae9cea9-c842-4a88-9d23-e0562d2d7646".into(),
            "other".into(),
        ];

        policy.retain_route_candidates(&["cloud".into(), "other".into()]);

        assert_eq!(
            policy.candidate_target_ids,
            vec!["cloud".to_string(), "other".to_string()]
        );
    }

    #[test]
    fn explicit_known_task_wins_over_rules_and_default() {
        let mut policy = RoutingPolicy::new("assistant");
        policy.rules.push(TaskRule {
            id: "tools".into(),
            task: "tool_use".into(),
            priority: 1,
            has_tools: Some(true),
            ..Default::default()
        });
        let signals = TaskSignals {
            endpoint: "/v1/chat/completions".into(),
            has_tools: true,
            modalities: vec![],
            reasoning: false,
            estimated_input_tokens: 20,
            text: "write code".into(),
        };
        let known = builtin_task_ids()
            .iter()
            .map(|value| (*value).to_owned())
            .collect::<Vec<_>>();

        let selected = determine_task(&policy, Some("coding"), &known, &signals).unwrap();

        assert_eq!(
            selected,
            TaskSelection {
                task: "coding".into(),
                source: "header",
                rule_id: None,
            }
        );
    }

    #[test]
    fn adaptive_ranking_prefers_task_quality_and_keeps_unknown_price_last() {
        let policy = RoutingPolicy {
            status: PolicyStatus::Active,
            privacy: PrivacyMode::CloudAllowed,
            ..RoutingPolicy::new("assistant")
        };
        let mut coding = TargetRoutingProfile::neutral("coding", TargetKind::Cloud);
        coding.task_quality.insert("coding".into(), 95.0);
        coding.input_price_per_million = Some(1.0);
        coding.output_price_per_million = Some(2.0);
        let mut unknown = TargetRoutingProfile::neutral("unknown", TargetKind::Cloud);
        unknown.task_quality.insert("coding".into(), 100.0);
        let request = RoutingRequest {
            required_capabilities: vec!["chat".into()],
            estimated_input_tokens: 100,
            max_output_tokens: 100,
        };

        let decision = rank_candidates(
            &policy,
            "coding",
            &request,
            vec![
                CandidateInput {
                    target_id: "unknown".into(),
                    kind: TargetKind::Cloud,
                    role: RouteRole::Primary,
                    priority: 10,
                    enabled: true,
                    capabilities: vec!["chat".into()],
                    force_tool_support: false,
                    profile: unknown,
                    stats: RoutingStats::default(),
                },
                CandidateInput {
                    target_id: "coding".into(),
                    kind: TargetKind::Cloud,
                    role: RouteRole::Primary,
                    priority: 20,
                    enabled: true,
                    capabilities: vec!["chat".into()],
                    force_tool_support: false,
                    profile: coding,
                    stats: RoutingStats::default(),
                },
            ],
        );

        assert_eq!(
            decision
                .ranked
                .iter()
                .map(|item| item.target_id.as_str())
                .collect::<Vec<_>>(),
            vec!["coding", "unknown"]
        );
        assert!(decision.ranked[0].cost_verified);
        assert!(!decision.ranked[1].cost_verified);
    }

    #[test]
    fn built_in_rules_classify_tools_before_prompt_keywords() {
        let policy = RoutingPolicy::new("assistant");
        let signals = TaskSignals {
            endpoint: "/v1/chat/completions".into(),
            has_tools: true,
            modalities: vec![],
            reasoning: false,
            estimated_input_tokens: 10,
            text: "write code".into(),
        };
        let known = builtin_task_ids()
            .iter()
            .map(|value| (*value).to_owned())
            .collect::<Vec<_>>();
        let selected = determine_task(&policy, None, &known, &signals).unwrap();
        assert_eq!(selected.task, "tool_use");
        assert_eq!(selected.source, "rule");
        assert_eq!(selected.rule_id.as_deref(), Some("builtin-tools"));
    }

    #[test]
    fn token_estimate_uses_full_prompt_and_tool_schema_beyond_regex_limit() {
        let long_text = "x".repeat(96 * 1024);
        let request = CanonicalRequest {
            system: vec![ContentBlock::Text {
                text: long_text.clone(),
            }],
            messages: vec![],
            tools: vec![crate::protocol::CanonicalTool {
                name: "lookup".into(),
                description: Some("y".repeat(3_000)),
                input_schema: serde_json::json!({"type":"object","description":"z".repeat(3_000)}),
            }],
            tool_choice: None,
            parallel_tool_calls: None,
            max_tokens: None,
            temperature: None,
            top_p: None,
            stop: None,
            reasoning: None,
            response_format: None,
            stream: false,
        };

        let signals = task_signals("/v1/chat/completions", Some(&request));

        assert!(signals.text.len() <= 32 * 1024 + 1);
        assert!(signals.estimated_input_tokens > long_text.len() as u64 / 3);
    }

    #[test]
    fn hard_filters_and_tie_breaks_are_deterministic() {
        let mut policy = RoutingPolicy::new("assistant");
        policy.privacy = PrivacyMode::LocalOnly;
        policy.max_estimated_cost_usd = Some(0.001);
        let request = RoutingRequest {
            required_capabilities: vec!["chat".into()],
            estimated_input_tokens: 4_000,
            max_output_tokens: 1_000,
        };
        let candidate = |id: &str, kind: TargetKind, priority: i64, context_window: u64| {
            let mut profile = TargetRoutingProfile::neutral(id, kind.clone());
            profile.context_window = context_window;
            CandidateInput {
                target_id: id.into(),
                kind,
                role: RouteRole::Primary,
                priority,
                enabled: true,
                capabilities: vec!["chat".into()],
                force_tool_support: false,
                profile,
                stats: RoutingStats::default(),
            }
        };

        let decision = rank_candidates(
            &policy,
            "general",
            &request,
            vec![
                candidate("cloud", TargetKind::Cloud, 1, 10_000),
                candidate("small", TargetKind::Gguf, 2, 4_999),
                candidate("beta", TargetKind::Gguf, 10, 10_000),
                candidate("alpha", TargetKind::Gguf, 10, 10_000),
            ],
        );

        assert_eq!(decision.ranked[0].target_id, "alpha");
        assert_eq!(decision.ranked[1].target_id, "beta");
        assert!(decision
            .excluded
            .iter()
            .any(|item| item.target_id == "cloud" && item.reason == "privacy_local_only"));
        assert!(decision
            .excluded
            .iter()
            .any(|item| item.target_id == "small" && item.reason == "context_window"));
    }

    #[test]
    fn known_cloud_targets_get_catalog_prices_without_a_saved_profile() {
        let target = crate::storage::ModelTarget {
            id: "t".into(),
            provider_id: None,
            name: "GPT-4o".into(),
            kind: TargetKind::Cloud,
            provider_model: "openai/gpt-4o".into(),
            local_path: None,
            runtime_url: None,
            wire_protocol: crate::providers::WireProtocol::OpenAiChat,
            capabilities: vec!["chat".into()],
            enabled: true,
            state: "ready".into(),
            size_bytes: None,
            local: crate::storage::LocalModelMeta::default(),
        };
        let profile = TargetRoutingProfile::for_target(&target);
        assert_eq!(profile.input_price_per_million, Some(2.50));
        assert_eq!(profile.output_price_per_million, Some(10.00));
        assert!(profile.task_quality.get("coding").copied().unwrap() > 50.0);
    }

    #[test]
    fn ranking_excludes_targets_with_huge_recent_latency_gaps() {
        let policy = RoutingPolicy {
            status: PolicyStatus::Active,
            privacy: PrivacyMode::CloudAllowed,
            ..RoutingPolicy::new("assistant")
        };
        let request = RoutingRequest {
            required_capabilities: vec!["chat".into()],
            estimated_input_tokens: 10,
            max_output_tokens: 10,
        };
        let candidate = |id: &str, recent: Option<u64>| {
            let mut stats = RoutingStats::default();
            stats.recent_latency_ms = recent;
            stats.samples = if recent.is_some() { 3 } else { 0 };
            CandidateInput {
                target_id: id.into(),
                kind: TargetKind::Cloud,
                role: RouteRole::Primary,
                priority: 10,
                enabled: true,
                capabilities: vec!["chat".into()],
                force_tool_support: false,
                profile: TargetRoutingProfile::neutral(id, TargetKind::Cloud),
                stats,
            }
        };

        let decision = rank_candidates(
            &policy,
            "general",
            &request,
            vec![
                candidate("fast-a", Some(1_000)),
                candidate("fast-b", Some(1_200)),
                candidate("slow", Some(30_000)),
            ],
        );

        assert!(decision
            .excluded
            .iter()
            .any(|item| item.target_id == "slow" && item.reason == "slow"));
        assert_eq!(
            decision
                .ranked
                .iter()
                .map(|item| item.target_id.as_str())
                .collect::<Vec<_>>(),
            vec!["fast-a", "fast-b"]
        );
    }

    fn sample_target(id: &str, capabilities: &[&str]) -> crate::storage::ModelTarget {
        crate::storage::ModelTarget {
            id: id.into(),
            provider_id: None,
            name: id.into(),
            kind: TargetKind::Gguf,
            provider_model: id.into(),
            local_path: None,
            runtime_url: None,
            wire_protocol: crate::providers::WireProtocol::OpenAiChat,
            capabilities: capabilities.iter().map(|item| (*item).to_owned()).collect(),
            enabled: true,
            state: "ready".into(),
            size_bytes: None,
            local: crate::storage::LocalModelMeta::default(),
        }
    }

    fn hop(id: &str, priority: i64, role: RouteRole) -> crate::domain::RouteTarget {
        crate::domain::RouteTarget {
            id: id.into(),
            kind: TargetKind::Gguf,
            model: id.into(),
            priority,
            enabled: true,
            role,
        }
    }

    async fn pool_fixture() -> (crate::storage::Store, ModelRoute) {
        let store = crate::storage::Store::memory().await.unwrap();
        for (id, caps) in [
            ("chat-a", &["chat"][..]),
            ("chat-b", &["chat"][..]),
            ("vision", &["chat", "vision"][..]),
        ] {
            store.upsert_target(&sample_target(id, caps)).await.unwrap();
        }
        let mut low = TargetRoutingProfile::neutral("chat-a", TargetKind::Gguf);
        low.task_quality.insert("coding".into(), 40.0);
        store.upsert_target_routing_profile(&low).await.unwrap();
        let mut high = TargetRoutingProfile::neutral("chat-b", TargetKind::Gguf);
        high.task_quality.insert("coding".into(), 90.0);
        store.upsert_target_routing_profile(&high).await.unwrap();
        let mut vision = TargetRoutingProfile::neutral("vision", TargetKind::Gguf);
        vision.task_quality.insert("coding".into(), 100.0);
        store.upsert_target_routing_profile(&vision).await.unwrap();
        let route = ModelRoute {
            alias: "assistant".into(),
            enabled: true,
            capabilities: vec!["chat".into(), "vision".into()],
            targets: vec![
                hop("chat-a", 10, RouteRole::Primary),
                hop("chat-b", 20, RouteRole::Primary),
                hop("vision", 10, RouteRole::Fallback),
            ],
        };
        store.upsert_route(&route).await.unwrap();
        (store, route)
    }

    #[tokio::test]
    async fn adaptive_ranks_primaries_and_keeps_fallback_in_reserve() {
        let (store, route) = pool_fixture().await;
        let mut policy = RoutingPolicy::new("assistant");
        policy.mode = RoutingMode::Adaptive;
        policy.status = PolicyStatus::Active;
        policy.privacy = PrivacyMode::CloudAllowed;
        policy.candidate_target_ids = vec!["chat-a".into(), "chat-b".into()];

        let evaluation = evaluate_route(
            &store,
            &route,
            RouteEvaluationInput {
                policy: Some(&policy),
                explicit_task: Some("coding"),
                endpoint: "/v1/chat/completions",
                canonical: None,
                required_capabilities: vec!["chat".into()],
                streaming: false,
            },
        )
        .await
        .unwrap();

        assert_eq!(
            evaluation.ordered_target_ids,
            vec!["chat-b", "chat-a", "vision"]
        );
        assert_eq!(
            evaluation
                .decision
                .ranked
                .iter()
                .map(|item| item.target_id.as_str())
                .collect::<Vec<_>>(),
            vec!["chat-b", "chat-a"]
        );
    }

    #[tokio::test]
    async fn performance_keeps_primary_order_then_fallbacks() {
        let (store, route) = pool_fixture().await;
        let evaluation = evaluate_route(
            &store,
            &route,
            RouteEvaluationInput {
                policy: None,
                explicit_task: None,
                endpoint: "/v1/chat/completions",
                canonical: None,
                required_capabilities: vec!["chat".into()],
                streaming: false,
            },
        )
        .await
        .unwrap();

        assert_eq!(evaluation.mode, "fixed");
        assert_eq!(
            evaluation.ordered_target_ids,
            vec!["chat-a", "chat-b", "vision"]
        );
        assert_eq!(
            evaluation.primary_target_ids,
            vec!["chat-a".to_string(), "chat-b".to_string()]
        );
        assert_eq!(evaluation.fallback_target_ids, vec!["vision".to_string()]);
        assert!(evaluation.has_later_primary("chat-a"));
        assert!(!evaluation.has_later_primary("chat-b"));
        assert!(evaluation.has_later_hop("chat-b"));
        assert!(evaluation.is_fallback_hop("vision"));
        assert!(!evaluation.has_later_hop("vision"));
    }

    #[tokio::test]
    async fn performance_keeps_every_primary_when_candidate_ids_are_a_subset() {
        let (store, route) = pool_fixture().await;
        let mut policy = RoutingPolicy::new("assistant");
        policy.mode = RoutingMode::Fixed;
        policy.status = PolicyStatus::Draft;
        policy.candidate_target_ids = vec!["chat-a".into()];

        let evaluation = evaluate_route(
            &store,
            &route,
            RouteEvaluationInput {
                policy: Some(&policy),
                explicit_task: None,
                endpoint: "/v1/chat/completions",
                canonical: None,
                required_capabilities: vec!["chat".into()],
                streaming: false,
            },
        )
        .await
        .unwrap();

        assert_eq!(evaluation.mode, "fixed");
        assert_eq!(
            evaluation.ordered_target_ids,
            vec!["chat-a", "chat-b", "vision"]
        );
        assert_eq!(
            evaluation.primary_target_ids,
            vec!["chat-a".to_string(), "chat-b".to_string()]
        );
    }

    #[tokio::test]
    async fn open_circuit_on_one_primary_leaves_the_rest_of_the_pool() {
        let (store, route) = pool_fixture().await;
        for index in 0..3 {
            store
                .insert_routing_attempt(&RoutingAttemptRecord {
                    id: format!("missing-{index}"),
                    request_id: "seed".into(),
                    created_at: Utc::now(),
                    alias: "assistant".into(),
                    task: "general".into(),
                    task_source: "default".into(),
                    target_id: "chat-a".into(),
                    routing_mode: "fixed".into(),
                    status: 404,
                    transient_failure: false,
                    retry_after_until: None,
                    latency_ms: 5,
                    ttft_ms: None,
                    streaming: false,
                    input_tokens: None,
                    output_tokens: None,
                    estimated_cost_usd: None,
                    cost_verified: false,
                    score: None,
                    reason: "default".into(),
                })
                .await
                .unwrap();
        }

        let evaluation = evaluate_route(
            &store,
            &route,
            RouteEvaluationInput {
                policy: None,
                explicit_task: None,
                endpoint: "/v1/chat/completions",
                canonical: None,
                required_capabilities: vec!["chat".into()],
                streaming: false,
            },
        )
        .await
        .unwrap();

        assert_eq!(evaluation.primary_target_ids, vec!["chat-b".to_string()]);
        assert_eq!(evaluation.fallback_target_ids, vec!["vision".to_string()]);
        assert_eq!(evaluation.ordered_target_ids, vec!["chat-b", "vision"]);
        assert!(evaluation
            .decision
            .excluded
            .iter()
            .any(|item| item.target_id == "chat-a" && item.reason == "circuit_open"));
    }

    #[tokio::test]
    async fn missing_primary_capability_uses_matching_fallback() {
        let (store, route) = pool_fixture().await;
        let evaluation = evaluate_route(
            &store,
            &route,
            RouteEvaluationInput {
                policy: None,
                explicit_task: None,
                endpoint: "/v1/chat/completions",
                canonical: None,
                required_capabilities: vec!["chat".into(), "vision".into()],
                streaming: false,
            },
        )
        .await
        .unwrap();

        assert_eq!(
            evaluation.ordered_target_ids,
            vec!["chat-a", "chat-b", "vision"]
        );
        assert!(evaluation
            .decision
            .excluded
            .iter()
            .any(|item| item.target_id == "chat-a" && item.reason == "capability"));
        assert!(evaluation
            .decision
            .excluded
            .iter()
            .any(|item| item.target_id == "chat-b" && item.reason == "capability"));
    }

    #[tokio::test]
    async fn adaptive_drops_incapable_primaries_and_keeps_feature_fallback() {
        let (store, route) = pool_fixture().await;
        let mut policy = RoutingPolicy::new("assistant");
        policy.mode = RoutingMode::Adaptive;
        policy.status = PolicyStatus::Active;
        policy.privacy = PrivacyMode::CloudAllowed;
        policy.candidate_target_ids = vec!["chat-a".into(), "chat-b".into()];
        let evaluation = evaluate_route(
            &store,
            &route,
            RouteEvaluationInput {
                policy: Some(&policy),
                explicit_task: Some("coding"),
                endpoint: "/v1/chat/completions",
                canonical: None,
                required_capabilities: vec!["chat".into(), "vision".into()],
                streaming: false,
            },
        )
        .await
        .unwrap();

        assert_eq!(evaluation.ordered_target_ids, vec!["vision"]);
        assert!(evaluation
            .decision
            .excluded
            .iter()
            .any(|item| item.target_id == "chat-a" && item.reason == "capability"));
    }

    #[tokio::test]
    async fn local_chat_models_stay_eligible_when_tools_are_required() {
        let (store, route) = pool_fixture().await;
        let evaluation = evaluate_route(
            &store,
            &route,
            RouteEvaluationInput {
                policy: None,
                explicit_task: None,
                endpoint: "/v1/responses",
                canonical: None,
                required_capabilities: vec!["chat".into(), "tools".into()],
                streaming: false,
            },
        )
        .await
        .unwrap();

        assert_eq!(
            evaluation.ordered_target_ids,
            vec!["chat-a", "chat-b", "vision"]
        );
        assert!(evaluation.decision.excluded.is_empty());
    }
}
