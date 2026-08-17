use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TargetKind {
    OpenAi,
    OpenRouter,
    Gguf,
    Mlx,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RouteTarget {
    pub id: String,
    pub kind: TargetKind,
    pub model: String,
    pub priority: i64,
    pub enabled: bool,
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
        targets.sort_by_key(|target| target.priority);
        targets
    }
}

pub fn is_transient_status(status: u16) -> bool {
    status == 429 || status >= 500
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target(id: &str, priority: i64, enabled: bool) -> RouteTarget {
        RouteTarget {
            id: id.into(),
            kind: TargetKind::OpenAi,
            model: id.into(),
            priority,
            enabled,
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
    fn only_rate_limits_and_server_errors_trigger_fallbacks() {
        assert!(is_transient_status(429));
        assert!(is_transient_status(500));
        assert!(is_transient_status(503));
        assert!(!is_transient_status(400));
        assert!(!is_transient_status(401));
        assert!(!is_transient_status(404));
    }
}
