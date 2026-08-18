use std::collections::HashSet;

use serde::Serialize;

use crate::{
    catalog::unique_alias,
    domain::{ModelRoute, RouteTarget},
    hub::slug,
    routing::{
        default_task_rules, PolicyStatus, PrivacyMode, RoutingMode, RoutingPolicy, RoutingWeights,
        ROUTING_POLICY_VERSION,
    },
    storage::{ModelTarget, Store},
};

pub const GLOBAL_ADAPTIVE_MODEL_ID: &str = "adaptive-routing";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PublicModelSource {
    Adaptive,
    Target,
    Alias,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct PublicModel {
    pub id: String,
    pub source: PublicModelSource,
    pub capabilities: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct ResolvedPublicModel {
    pub route: ModelRoute,
    pub policy: Option<RoutingPolicy>,
}

pub fn preferred_public_id(provider_model: &str, name: &str) -> String {
    let candidate = provider_model.trim();
    if !candidate.is_empty() && !candidate.chars().any(char::is_whitespace) {
        return candidate.to_owned();
    }
    let from_name = slug(name);
    if from_name.is_empty() {
        "model".into()
    } else {
        from_name
    }
}

pub fn is_reserved_public_model_id(id: &str) -> bool {
    id == GLOBAL_ADAPTIVE_MODEL_ID
}

pub fn global_adaptive_policy(candidate_target_ids: Vec<String>) -> RoutingPolicy {
    RoutingPolicy {
        version: ROUTING_POLICY_VERSION,
        alias: GLOBAL_ADAPTIVE_MODEL_ID.into(),
        mode: RoutingMode::Adaptive,
        status: PolicyStatus::Active,
        privacy: PrivacyMode::CloudAllowed,
        default_task: "general".into(),
        weights: RoutingWeights {
            quality: 0.50,
            cost: 0.35,
            latency: 0.05,
            reliability: 0.05,
            locality: 0.05,
        },
        max_estimated_cost_usd: None,
        preferred_latency_ms: 2_000,
        preferred_cost_usd: 0.01,
        rules: default_task_rules(),
        candidate_target_ids,
    }
}

pub fn compose_public_models(
    targets: &[ModelTarget],
    custom_routes: &[ModelRoute],
) -> Vec<ModelRoute> {
    compose_public_model_entries(targets, custom_routes)
        .into_iter()
        .map(|entry| entry.route)
        .collect()
}

pub fn list_composed_public_models(
    targets: &[ModelTarget],
    custom_routes: &[ModelRoute],
) -> Vec<PublicModel> {
    compose_public_model_entries(targets, custom_routes)
        .into_iter()
        .map(|entry| PublicModel {
            id: entry.route.alias,
            source: entry.source,
            capabilities: entry.route.capabilities,
        })
        .collect()
}

struct PublicModelEntry {
    source: PublicModelSource,
    route: ModelRoute,
}

fn compose_public_model_entries(
    targets: &[ModelTarget],
    custom_routes: &[ModelRoute],
) -> Vec<PublicModelEntry> {
    let enabled_targets: Vec<_> = targets
        .iter()
        .filter(|target| target.enabled)
        .cloned()
        .collect();
    let mut reserved = HashSet::from([GLOBAL_ADAPTIVE_MODEL_ID.to_owned()]);
    for route in custom_routes.iter().filter(|route| route.enabled) {
        reserved.insert(route.alias.clone());
    }
    let assigned = assign_target_public_ids(&enabled_targets, &reserved);
    let mut models = Vec::new();
    if !assigned.is_empty() {
        models.push(PublicModelEntry {
            source: PublicModelSource::Adaptive,
            route: adaptive_route(&assigned),
        });
    }
    for (public_id, target) in &assigned {
        models.push(PublicModelEntry {
            source: PublicModelSource::Target,
            route: singleton_route(public_id.clone(), target),
        });
    }
    for route in custom_routes {
        if route.enabled && !is_reserved_public_model_id(&route.alias) {
            models.push(PublicModelEntry {
                source: PublicModelSource::Alias,
                route: route.clone(),
            });
        }
    }
    models
}

fn assign_target_public_ids<'a>(
    targets: &'a [ModelTarget],
    reserved: &HashSet<String>,
) -> Vec<(String, &'a ModelTarget)> {
    let mut taken = reserved.clone();
    let mut ordered: Vec<_> = targets.iter().collect();
    ordered.sort_by(|left, right| left.name.cmp(&right.name).then(left.id.cmp(&right.id)));
    let mut assigned = Vec::with_capacity(ordered.len());
    for target in ordered {
        let preferred = preferred_public_id(&target.provider_model, &target.name);
        let public_id = unique_alias(&preferred, &taken);
        taken.insert(public_id.clone());
        assigned.push((public_id, target));
    }
    assigned
}

fn singleton_route(public_id: String, target: &ModelTarget) -> ModelRoute {
    ModelRoute {
        alias: public_id,
        enabled: target.enabled,
        capabilities: target.capabilities.clone(),
        targets: vec![RouteTarget {
            id: target.id.clone(),
            kind: target.kind.clone(),
            model: target.provider_model.clone(),
            priority: 10,
            enabled: true,
        }],
    }
}

fn adaptive_route(assigned: &[(String, &ModelTarget)]) -> ModelRoute {
    let mut capabilities = Vec::new();
    let mut targets = Vec::with_capacity(assigned.len());
    for (index, (_, target)) in assigned.iter().enumerate() {
        for capability in &target.capabilities {
            if !capabilities.contains(capability) {
                capabilities.push(capability.clone());
            }
        }
        targets.push(RouteTarget {
            id: target.id.clone(),
            kind: target.kind.clone(),
            model: target.provider_model.clone(),
            priority: ((index + 1) * 10) as i64,
            enabled: true,
        });
    }
    capabilities.sort();
    ModelRoute {
        alias: GLOBAL_ADAPTIVE_MODEL_ID.into(),
        enabled: true,
        capabilities,
        targets,
    }
}

pub async fn advertised_public_models(store: &Store) -> anyhow::Result<Vec<ModelRoute>> {
    let targets = store.targets().await?;
    let mut custom_routes = store.routes().await?;
    for route in &mut custom_routes {
        expand_adaptive_alias_capabilities(store, route).await?;
    }
    Ok(compose_public_models(&targets, &custom_routes))
}

pub async fn list_public_models(store: &Store) -> anyhow::Result<Vec<PublicModel>> {
    let targets = store.targets().await?;
    let custom_routes = store.routes().await?;
    Ok(list_composed_public_models(&targets, &custom_routes))
}

pub async fn resolve_public_model(
    store: &Store,
    model: &str,
) -> anyhow::Result<Option<ResolvedPublicModel>> {
    let targets = store.targets().await?;
    let custom_routes = store.routes().await?;
    if model == GLOBAL_ADAPTIVE_MODEL_ID {
        let Some(route) = compose_public_models(&targets, &custom_routes)
            .into_iter()
            .find(|route| route.alias == GLOBAL_ADAPTIVE_MODEL_ID)
        else {
            return Ok(None);
        };
        let candidate_target_ids = route
            .targets
            .iter()
            .map(|target| target.id.clone())
            .collect();
        return Ok(Some(ResolvedPublicModel {
            route,
            policy: Some(global_adaptive_policy(candidate_target_ids)),
        }));
    }
    if let Some(route) = custom_routes
        .iter()
        .find(|route| route.alias == model)
        .cloned()
    {
        if !route.enabled {
            return Ok(None);
        }
        let policy = store.routing_policy(model).await?;
        return Ok(Some(ResolvedPublicModel { route, policy }));
    }
    let Some(route) = compose_public_models(&targets, &custom_routes)
        .into_iter()
        .find(|route| route.alias == model)
    else {
        return Ok(None);
    };
    Ok(Some(ResolvedPublicModel {
        route,
        policy: None,
    }))
}

pub async fn expand_route_targets(
    store: &Store,
    route: &ModelRoute,
    visited: &mut HashSet<String>,
) -> anyhow::Result<Vec<RouteTarget>> {
    if !visited.insert(route.alias.clone()) {
        return Ok(Vec::new());
    }
    let mut expanded = Vec::new();
    let mut priority = 10_i64;
    for hop in route.ordered_targets() {
        let concrete = store.target(&hop.id).await?;
        if hop.kind.is_alias() || concrete.is_none() {
            let Some(resolved) = resolve_public_model(store, &hop.id).await? else {
                continue;
            };
            let nested = Box::pin(expand_route_targets(store, &resolved.route, visited)).await?;
            for mut target in nested {
                target.priority = priority;
                expanded.push(target);
                priority += 10;
            }
            continue;
        }
        let mut target = hop.clone();
        target.priority = priority;
        expanded.push(target);
        priority += 10;
    }
    Ok(expanded)
}

async fn expand_adaptive_alias_capabilities(
    store: &Store,
    route: &mut ModelRoute,
) -> anyhow::Result<()> {
    let active = store
        .routing_policy(&route.alias)
        .await?
        .is_some_and(|policy| {
            policy.mode == RoutingMode::Adaptive && policy.status == PolicyStatus::Active
        });
    if !active {
        return Ok(());
    }
    let mut capabilities = Vec::new();
    for route_target in expand_route_targets(store, route, &mut HashSet::new()).await? {
        if !route_target.enabled {
            continue;
        }
        let Some(target) = store.target(&route_target.id).await? else {
            continue;
        };
        if !target.enabled {
            continue;
        }
        for capability in target.capabilities {
            if !capabilities.contains(&capability) {
                capabilities.push(capability);
            }
        }
    }
    capabilities.sort();
    route.capabilities = capabilities;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{domain::TargetKind, providers::WireProtocol, storage::LocalModelMeta};

    fn target(id: &str, name: &str, provider_model: &str, enabled: bool) -> ModelTarget {
        ModelTarget {
            id: id.into(),
            provider_id: None,
            name: name.into(),
            kind: TargetKind::Cloud,
            provider_model: provider_model.into(),
            local_path: None,
            runtime_url: None,
            wire_protocol: WireProtocol::OpenAiChat,
            capabilities: vec!["chat".into()],
            enabled,
            state: "ready".into(),
            size_bytes: None,
            local: LocalModelMeta::default(),
        }
    }

    fn alias(name: &str, target_id: &str) -> ModelRoute {
        ModelRoute {
            alias: name.into(),
            enabled: true,
            capabilities: vec!["chat".into()],
            targets: vec![RouteTarget {
                id: target_id.into(),
                kind: TargetKind::Cloud,
                model: target_id.into(),
                priority: 10,
                enabled: true,
            }],
        }
    }

    #[test]
    fn enabled_targets_are_published_without_a_custom_alias() {
        let models = list_composed_public_models(&[target("t1", "GPT-4o", "gpt-4o", true)], &[]);
        let ids: Vec<_> = models.iter().map(|model| model.id.as_str()).collect();
        assert_eq!(ids, vec!["adaptive-routing", "gpt-4o"]);
        assert_eq!(models[0].source, PublicModelSource::Adaptive);
        assert_eq!(models[1].source, PublicModelSource::Target);
    }

    #[test]
    fn custom_aliases_keep_their_name_and_suffix_colliding_targets() {
        let models = list_composed_public_models(
            &[target("t1", "GPT-4o", "gpt-4o", true)],
            &[alias("gpt-4o", "other")],
        );
        let ids: Vec<_> = models.iter().map(|model| model.id.as_str()).collect();
        assert_eq!(ids, vec!["adaptive-routing", "gpt-4o-2", "gpt-4o"]);
        assert_eq!(models[1].source, PublicModelSource::Target);
        assert_eq!(models[2].source, PublicModelSource::Alias);
    }

    #[test]
    fn disabled_targets_are_not_published() {
        let models = list_composed_public_models(&[target("t1", "Off", "hidden", false)], &[]);
        assert!(models.is_empty());
    }

    #[test]
    fn whitespace_provider_models_fall_back_to_a_name_slug() {
        assert_eq!(
            preferred_public_id("open ai", "My Local Chat"),
            "my-local-chat"
        );
        assert_eq!(preferred_public_id("gpt-4o", "Anything"), "gpt-4o");
    }

    #[test]
    fn global_adaptive_policy_ranks_by_quality_price_and_task() {
        let policy = global_adaptive_policy(vec!["a".into(), "b".into()]);
        assert_eq!(policy.alias, GLOBAL_ADAPTIVE_MODEL_ID);
        assert_eq!(policy.mode, RoutingMode::Adaptive);
        assert_eq!(policy.status, PolicyStatus::Active);
        assert!(policy.weights.quality > policy.weights.cost);
        assert!(policy.weights.cost > policy.weights.latency);
        assert_eq!(policy.candidate_target_ids, vec!["a", "b"]);
    }

    #[tokio::test]
    async fn alias_hops_expand_and_cycles_stop() {
        let store = crate::storage::Store::memory().await.unwrap();
        store
            .upsert_target(&target("one", "One", "one-model", true))
            .await
            .unwrap();
        store
            .upsert_target(&target("two", "Two", "two-model", true))
            .await
            .unwrap();
        store
            .upsert_route(&ModelRoute {
                alias: "inner".into(),
                enabled: true,
                capabilities: vec!["chat".into()],
                targets: vec![
                    RouteTarget {
                        id: "two".into(),
                        kind: TargetKind::Cloud,
                        model: "two-model".into(),
                        priority: 10,
                        enabled: true,
                    },
                    RouteTarget {
                        id: "outer".into(),
                        kind: TargetKind::Alias,
                        model: "outer".into(),
                        priority: 20,
                        enabled: true,
                    },
                ],
            })
            .await
            .unwrap();
        store
            .upsert_route(&ModelRoute {
                alias: "outer".into(),
                enabled: true,
                capabilities: vec!["chat".into()],
                targets: vec![
                    RouteTarget {
                        id: "one".into(),
                        kind: TargetKind::Cloud,
                        model: "one-model".into(),
                        priority: 10,
                        enabled: true,
                    },
                    RouteTarget {
                        id: "inner".into(),
                        kind: TargetKind::Alias,
                        model: "inner".into(),
                        priority: 20,
                        enabled: true,
                    },
                ],
            })
            .await
            .unwrap();
        let outer = store.route("outer").await.unwrap().unwrap();
        let ids: Vec<_> = expand_route_targets(&store, &outer, &mut HashSet::new())
            .await
            .unwrap()
            .into_iter()
            .map(|target| target.id)
            .collect();
        assert_eq!(ids, vec!["one", "two"]);
    }
}
