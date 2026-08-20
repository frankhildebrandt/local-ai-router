use serde::{Deserialize, Serialize};

pub const SLOW_OUTLIER_FACTOR: u64 = 3;
pub const SLOW_MIN_LATENCY_MS: u64 = 8_000;
pub const SLOW_WINDOW_SECS: i64 = 45;
pub const NOT_FOUND_TRIP: usize = 3;
pub const NOT_FOUND_COOLDOWN_SECS: i64 = 120;
pub const RATE_LIMIT_DEFAULT_SECS: i64 = 30;
pub const UPSTREAM_TIMEOUT_MS: u64 = 120_000;
pub const SAME_TARGET_RETRY_LIMIT: u32 = 1;
pub const SAME_TARGET_RETRY_DELAY_MS: u64 = 400;
pub const SAME_TARGET_RETRY_MAX_WAIT_MS: u64 = 2_000;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum RouteRole {
    #[default]
    Primary,
    Fallback,
}

impl RouteRole {
    pub fn is_fallback(self) -> bool {
        matches!(self, Self::Fallback)
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TargetKind {
    #[serde(alias = "open_ai", alias = "open_router")]
    Cloud,
    Gguf,
    Mlx,
    Alias,
}

impl TargetKind {
    pub fn is_local(&self) -> bool {
        matches!(self, Self::Gguf | Self::Mlx)
    }

    pub fn is_alias(&self) -> bool {
        matches!(self, Self::Alias)
    }
}

pub fn supports_capability(kind: TargetKind, advertised: &[String], required: &str) -> bool {
    if advertised.iter().any(|item| item == required) {
        return true;
    }
    if required == "speech" && !kind.is_local() && advertised.iter().any(|item| item == "audio") {
        return true;
    }
    required == "tools" && kind.is_local() && advertised.iter().any(|item| item == "chat")
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RouteTarget {
    pub id: String,
    pub kind: TargetKind,
    pub model: String,
    pub priority: i64,
    pub enabled: bool,
    #[serde(default)]
    pub role: RouteRole,
}

impl Default for RouteTarget {
    fn default() -> Self {
        Self {
            id: String::new(),
            kind: TargetKind::Cloud,
            model: String::new(),
            priority: 10,
            enabled: true,
            role: RouteRole::Primary,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ModelRoute {
    pub alias: String,
    pub enabled: bool,
    pub capabilities: Vec<String>,
    pub targets: Vec<RouteTarget>,
}

impl ModelRoute {
    pub fn ordered_targets(&self) -> Vec<&RouteTarget> {
        let mut targets: Vec<_> = self
            .targets
            .iter()
            .filter(|target| target.enabled)
            .collect();
        targets.sort_by_key(|target| (target.role.is_fallback(), target.priority));
        targets
    }

    pub fn primaries(&self) -> Vec<&RouteTarget> {
        self.role_targets(RouteRole::Primary)
    }

    pub fn fallbacks(&self) -> Vec<&RouteTarget> {
        self.role_targets(RouteRole::Fallback)
    }

    pub fn primary_ids(&self) -> Vec<String> {
        let mut targets: Vec<_> = self
            .targets
            .iter()
            .filter(|target| target.role == RouteRole::Primary)
            .collect();
        targets.sort_by_key(|target| target.priority);
        targets
            .into_iter()
            .map(|target| target.id.clone())
            .collect()
    }

    fn role_targets(&self, role: RouteRole) -> Vec<&RouteTarget> {
        let mut targets: Vec<_> = self
            .targets
            .iter()
            .filter(|target| target.enabled && target.role == role)
            .collect();
        targets.sort_by_key(|target| target.priority);
        targets
    }
}

pub fn is_transient_status(status: u16) -> bool {
    status == 429 || status >= 500
}

pub fn is_fallback_status(status: u16) -> bool {
    status >= 400
}

pub fn can_retry_same_target(status: u16, same_target_attempt: u32) -> bool {
    is_transient_status(status) && same_target_attempt <= SAME_TARGET_RETRY_LIMIT
}

pub fn is_slow_outlier(latency_ms: u64, peer_median_ms: u64) -> bool {
    latency_ms >= SLOW_MIN_LATENCY_MS
        && latency_ms >= peer_median_ms.saturating_mul(SLOW_OUTLIER_FACTOR)
}

pub fn first_byte_timeout_ms(peer_median_ms: Option<u64>, has_fallback: bool) -> u64 {
    if !has_fallback || peer_median_ms.unwrap_or(0) == 0 {
        return UPSTREAM_TIMEOUT_MS;
    }
    peer_median_ms
        .unwrap_or(0)
        .saturating_mul(SLOW_OUTLIER_FACTOR)
        .max(SLOW_MIN_LATENCY_MS)
        .min(UPSTREAM_TIMEOUT_MS)
}

pub fn median_u64(values: &[u64]) -> Option<u64> {
    if values.is_empty() {
        return None;
    }
    let mut sorted = values.to_vec();
    sorted.sort_unstable();
    Some(sorted[sorted.len() / 2])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target(id: &str, priority: i64, enabled: bool) -> RouteTarget {
        RouteTarget {
            id: id.into(),
            kind: TargetKind::Cloud,
            model: id.into(),
            priority,
            enabled,
            ..Default::default()
        }
    }

    #[test]
    fn route_exposes_only_enabled_targets_in_fallback_order() {
        let route = ModelRoute {
            alias: "daily".into(),
            enabled: true,
            capabilities: vec!["chat".into()],
            targets: vec![
                target("fallback", 20, true),
                target("off", 5, false),
                target("primary", 10, true),
            ],
        };

        let ids: Vec<_> = route
            .ordered_targets()
            .iter()
            .map(|target| target.id.as_str())
            .collect();
        assert_eq!(ids, vec!["primary", "fallback"]);
    }

    #[test]
    fn primaries_sort_ahead_of_fallbacks_regardless_of_priority() {
        let mut fallback = target("vision", 5, true);
        fallback.role = RouteRole::Fallback;
        let route = ModelRoute {
            alias: "daily".into(),
            enabled: true,
            capabilities: vec!["chat".into(), "vision".into()],
            targets: vec![
                fallback,
                target("chat-a", 20, true),
                target("chat-b", 30, true),
            ],
        };

        let ids: Vec<_> = route
            .ordered_targets()
            .iter()
            .map(|target| target.id.as_str())
            .collect();
        assert_eq!(ids, vec!["chat-a", "chat-b", "vision"]);
        assert_eq!(route.primaries().len(), 2);
        assert_eq!(route.fallbacks()[0].id, "vision");
    }

    #[test]
    fn only_rate_limits_and_server_errors_are_transient() {
        assert!(is_transient_status(429));
        assert!(is_transient_status(500));
        assert!(is_transient_status(503));
        assert!(!is_transient_status(400));
        assert!(!is_transient_status(401));
        assert!(!is_transient_status(404));
    }

    #[test]
    fn fallbacks_include_client_and_server_errors() {
        assert!(is_fallback_status(400));
        assert!(is_fallback_status(401));
        assert!(is_fallback_status(403));
        assert!(is_fallback_status(404));
        assert!(is_fallback_status(429));
        assert!(is_fallback_status(500));
        assert!(is_fallback_status(503));
        assert!(!is_fallback_status(200));
        assert!(!is_fallback_status(399));
    }

    #[test]
    fn same_target_retry_covers_one_transient_replay() {
        assert!(can_retry_same_target(503, 1));
        assert!(can_retry_same_target(429, 1));
        assert!(can_retry_same_target(502, 1));
        assert!(!can_retry_same_target(503, 2));
        assert!(!can_retry_same_target(400, 1));
        assert!(!can_retry_same_target(404, 1));
    }

    #[test]
    fn slow_outliers_require_a_huge_gap_and_an_absolute_floor() {
        assert!(!is_slow_outlier(300, 80));
        assert!(!is_slow_outlier(7_999, 1_000));
        assert!(is_slow_outlier(8_000, 1_000));
        assert!(is_slow_outlier(30_000, 2_000));
    }

    #[test]
    fn local_chat_implies_tools_while_legacy_audio_is_not_speech() {
        let chat = vec!["chat".into(), "streaming".into()];
        assert!(supports_capability(TargetKind::Mlx, &chat, "tools"));
        assert!(supports_capability(TargetKind::Gguf, &chat, "chat"));
        assert!(!supports_capability(TargetKind::Cloud, &chat, "tools"));
        assert!(!supports_capability(
            TargetKind::Mlx,
            &["audio".into()],
            "speech"
        ));
        assert!(supports_capability(
            TargetKind::Cloud,
            &["audio".into()],
            "speech"
        ));
    }

    #[test]
    fn first_byte_timeout_tightens_only_when_peers_and_fallback_exist() {
        assert_eq!(first_byte_timeout_ms(None, true), UPSTREAM_TIMEOUT_MS);
        assert_eq!(
            first_byte_timeout_ms(Some(1_000), false),
            UPSTREAM_TIMEOUT_MS
        );
        assert_eq!(
            first_byte_timeout_ms(Some(1_000), true),
            SLOW_MIN_LATENCY_MS
        );
        assert_eq!(first_byte_timeout_ms(Some(10_000), true), 30_000);
    }
}
