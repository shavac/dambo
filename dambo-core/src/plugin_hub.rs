use crate::plugin::PluginRegistry;
use crate::plugin_store::{
    AclLevel, AclRecord, DataMutationResult, MutationOp, PluginStore, StoreKind,
};
use crate::plugin_types::{AclResourceKind, PluginApiError, parse_acl_resource};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

/// Outbound SSE frame sent from `PluginEventHub` to a connected client task.
#[derive(Clone, Debug)]
pub struct SseOutbound {
    pub event: &'static str,
    pub data: String,
}

/// Client-requested data watch subscription (from URL query or `PUT .../subscriptions`).
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct WatchSubInput {
    pub store: String,
    pub owner: String,
    pub namespace: String,
    #[serde(default)]
    pub prefix: Option<String>,
}

/// Client-requested topic subscription (from URL query or `PUT .../subscriptions`).
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct TopicSubInput {
    pub owner: String,
    pub topic: String,
}

/// ACL-verified watch subscription stored on an active SSE connection.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct AcceptedWatch {
    pub store: StoreKind,
    pub owner: String,
    pub requested_owner: String,
    pub namespace: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prefix: Option<String>,
}

/// ACL-verified topic subscription stored on an active SSE connection.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct AcceptedTopic {
    pub owner: String,
    pub requested_owner: String,
    pub topic: String,
}

/// Diagnostic item returned inside `connected` / `PUT .../subscriptions` for denied or invalid subscriptions.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct RejectedSub {
    pub kind: &'static str,
    pub owner: String,
    pub target: String,
    pub code: &'static str,
    pub message: String,
}

/// Combined outcome of validating a batch of watches + topics against `PluginRegistry` and `PluginStore`.
#[derive(Clone, Debug, Default, Serialize)]
pub struct SubscriptionSyncResult {
    pub accepted_watches: Vec<AcceptedWatch>,
    pub accepted_topics: Vec<AcceptedTopic>,
    pub rejected: Vec<RejectedSub>,
}

/// Enriched requester identity metadata attached to `incoming` ACL entries and `acl.changed` events.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct RequesterDisplay {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plugin_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub installed: bool,
}

struct SseClientRecord {
    plugin_name: String,
    namedomain: String,
    #[allow(dead_code)]
    mode: String,
    watches: Vec<AcceptedWatch>,
    topics: Vec<AcceptedTopic>,
    tx: mpsc::Sender<SseOutbound>,
}

#[derive(Serialize)]
struct DataChangedPayload<'a> {
    store: StoreKind,
    owner: &'a str,
    namespace: &'a str,
    op: MutationOp,
    keys: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    values: Option<BTreeMap<String, serde_json::Value>>,
    by: &'a str,
    ts: i64,
}

#[derive(Serialize)]
struct TopicMessagePayload<'a> {
    owner: &'a str,
    topic: &'a str,
    payload: &'a serde_json::Value,
    ts: i64,
}

#[derive(Serialize)]
struct AclChangedPayload<'a> {
    action: &'a str,
    owner: &'a str,
    requester: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    requester_display: Option<&'a RequesterDisplay>,
    resource: &'a str,
    level: AclLevel,
    status: crate::plugin_store::AclStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    requested_level: Option<AclLevel>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    note: Option<&'a str>,
    ts: i64,
}

/// Real-time SSE connection registry and filtered event dispatcher for Layer 1 `/api/v1/events`.
pub struct PluginEventHub {
    clients: RwLock<HashMap<String, SseClientRecord>>,
    next_id: AtomicU64,
}

impl Default for PluginEventHub {
    fn default() -> Self {
        Self::new()
    }
}

impl PluginEventHub {
    pub fn new() -> Self {
        Self {
            clients: RwLock::new(HashMap::new()),
            next_id: AtomicU64::new(1),
        }
    }

    /// Number of currently connected SSE clients (useful for diagnostics & tests).
    pub fn client_count(&self) -> usize {
        self.clients.read().map(|c| c.len()).unwrap_or(0)
    }

    /// Register a new SSE client connection and obtain its outbound receiver.
    pub fn register_client(
        &self,
        plugin_name: String,
        namedomain: String,
        mode: String,
        watches: Vec<AcceptedWatch>,
        topics: Vec<AcceptedTopic>,
    ) -> (String, mpsc::Receiver<SseOutbound>) {
        let seq = self.next_id.fetch_add(1, Ordering::Relaxed);
        let client_id = format!("c_{}", seq);
        let (tx, rx) = mpsc::channel(128);
        if let Ok(mut map) = self.clients.write() {
            map.insert(
                client_id.clone(),
                SseClientRecord {
                    plugin_name,
                    namedomain,
                    mode,
                    watches,
                    topics,
                    tx,
                },
            );
        }
        (client_id, rx)
    }

    /// Remove a disconnected SSE client cleanly.
    pub fn unregister_client(&self, client_id: &str) {
        if let Ok(mut map) = self.clients.write() {
            map.remove(client_id);
        }
    }

    /// Atomically replace the active watch/topic subscriptions of an existing SSE client.
    pub fn replace_subscriptions(
        &self,
        client_id: &str,
        expected_plugin_name: &str,
        watches: Vec<AcceptedWatch>,
        topics: Vec<AcceptedTopic>,
    ) -> Result<(), PluginApiError> {
        let mut map = self
            .clients
            .write()
            .map_err(|_| PluginApiError::internal("SSE client registry lock poisoned"))?;
        let Some(rec) = map.get_mut(client_id) else {
            return Err(PluginApiError::client_not_found(format!(
                "SSE client '{}' not found or already closed",
                client_id
            )));
        };
        if rec.plugin_name != expected_plugin_name {
            return Err(PluginApiError::acl_denied(format!(
                "SSE client '{}' belongs to another plugin",
                client_id
            )));
        }
        rec.watches = watches;
        rec.topics = topics;
        Ok(())
    }

    /// Push a `data.changed` event to subscribed SSE clients, trimming keys/inline values by `prefix`.
    pub fn publish_data_changed(&self, mutation: &DataMutationResult, by_namedomain: &str) {
        let ts = Utc::now().timestamp_millis();
        let Ok(map) = self.clients.read() else {
            return;
        };

        for (client_id, rec) in map.iter() {
            let matching: Vec<&AcceptedWatch> = rec
                .watches
                .iter()
                .filter(|w| {
                    w.store == mutation.store
                        && w.owner == mutation.owner_namedomain
                        && w.namespace == mutation.namespace
                })
                .collect();
            if matching.is_empty() {
                continue;
            }

            let wildcard_match =
                mutation.op == MutationOp::Clear || matching.iter().any(|w| w.prefix.is_none());

            let (filtered_keys, filtered_values) = if wildcard_match {
                (mutation.keys.clone(), mutation.inline_values.clone())
            } else {
                let mut matched_keys = Vec::new();
                for k in &mutation.keys {
                    if matching
                        .iter()
                        .any(|w| w.prefix.as_deref().is_some_and(|p| k.starts_with(p)))
                    {
                        matched_keys.push(k.clone());
                    }
                }
                if matched_keys.is_empty() {
                    continue;
                }
                let matched_values = mutation.inline_values.as_ref().map(|all_vals| {
                    let mut out = BTreeMap::new();
                    for k in &matched_keys {
                        if let Some(v) = all_vals.get(k) {
                            out.insert(k.clone(), v.clone());
                        }
                    }
                    out
                });
                (matched_keys, matched_values)
            };

            let payload = DataChangedPayload {
                store: mutation.store,
                owner: &mutation.owner_namedomain,
                namespace: &mutation.namespace,
                op: mutation.op,
                keys: filtered_keys,
                values: filtered_values,
                by: by_namedomain,
                ts,
            };
            let Ok(data) = serde_json::to_string(&payload) else {
                continue;
            };
            if rec
                .tx
                .try_send(SseOutbound {
                    event: "data.changed",
                    data,
                })
                .is_err()
            {
                log::warn!(
                    "Dropped data.changed SSE frame for slow client '{}' ({})",
                    client_id,
                    rec.plugin_name
                );
            }
        }
    }

    /// Push a `topic.message` frame to every SSE client subscribed to `(owner_namedomain, topic)`.
    /// Returns the number of matching clients to which the frame was enqueued.
    pub fn publish_topic_message(
        &self,
        owner_namedomain: &str,
        topic: &str,
        payload: &serde_json::Value,
    ) -> usize {
        let ts = Utc::now().timestamp_millis();
        let msg = TopicMessagePayload {
            owner: owner_namedomain,
            topic,
            payload,
            ts,
        };
        let Ok(data) = serde_json::to_string(&msg) else {
            return 0;
        };
        let Ok(map) = self.clients.read() else {
            return 0;
        };

        let mut delivered = 0usize;
        for (client_id, rec) in map.iter() {
            if !rec
                .topics
                .iter()
                .any(|t| t.owner == owner_namedomain && t.topic == topic)
            {
                continue;
            }
            if rec
                .tx
                .try_send(SseOutbound {
                    event: "topic.message",
                    data: data.clone(),
                })
                .is_ok()
            {
                delivered += 1;
            } else {
                log::warn!(
                    "Dropped topic.message SSE frame for slow client '{}' ({})",
                    client_id,
                    rec.plugin_name
                );
            }
        }
        delivered
    }

    /// Broadcast an `acl.changed` notification to connected SSE clients of both `owner` and `requester`.
    pub fn publish_acl_changed(
        &self,
        action: &str,
        record: &AclRecord,
        requester_display: Option<&RequesterDisplay>,
    ) {
        let payload = AclChangedPayload {
            action,
            owner: &record.owner,
            requester: &record.requester,
            requester_display,
            resource: &record.resource,
            level: record.level,
            status: record.status,
            requested_level: record.requested_level,
            reason: record.reason.as_deref(),
            note: record.note.as_deref(),
            ts: record.updated_at,
        };
        let Ok(data) = serde_json::to_string(&payload) else {
            return;
        };
        let Ok(map) = self.clients.read() else {
            return;
        };

        for (client_id, rec) in map.iter() {
            if rec.namedomain != record.owner && rec.namedomain != record.requester {
                continue;
            }
            if rec
                .tx
                .try_send(SseOutbound {
                    event: "acl.changed",
                    data: data.clone(),
                })
                .is_err()
            {
                log::warn!(
                    "Dropped acl.changed SSE frame for slow client '{}' ({})",
                    client_id,
                    rec.plugin_name
                );
            }
        }
    }

    /// Re-verify all cross-plugin SSE watches and topic subscriptions targeting `owner_namedomain`,
    /// immediately removing any subscription whose ACL was revoked or topic was made private.
    pub async fn prune_unauthorized_for_owner(
        &self,
        store: &PluginStore,
        owner_namedomain: &str,
    ) {
        let candidates: Vec<(String, String, Vec<AcceptedWatch>, Vec<AcceptedTopic>)> = {
            let Ok(map) = self.clients.read() else {
                return;
            };
            map.iter()
                .filter(|(_, rec)| {
                    rec.namedomain != owner_namedomain
                        && (rec.watches.iter().any(|w| w.owner == owner_namedomain)
                            || rec.topics.iter().any(|t| t.owner == owner_namedomain))
                })
                .map(|(cid, rec)| {
                    (
                        cid.clone(),
                        rec.namedomain.clone(),
                        rec.watches
                            .iter()
                            .filter(|w| w.owner == owner_namedomain)
                            .cloned()
                            .collect(),
                        rec.topics
                            .iter()
                            .filter(|t| t.owner == owner_namedomain)
                            .cloned()
                            .collect(),
                    )
                })
                .collect()
        };

        if candidates.is_empty() {
            return;
        }

        let mut revoked_watches: HashMap<String, Vec<(StoreKind, String)>> = HashMap::new();
        let mut revoked_topics: HashMap<String, Vec<String>> = HashMap::new();

        for (cid, caller_domain, watches, topics) in candidates {
            for w in watches {
                let res = format!("{}:{}", w.store.as_str(), w.namespace);
                let allowed = match store
                    .check_effective_acl(&caller_domain, owner_namedomain, &res)
                    .await
                {
                    Ok(chk) => parse_acl_level(&chk.effective_level)
                        .satisfies(AclLevel::Read, &AclResourceKind::Persist),
                    Err(_) => false,
                };
                if !allowed {
                    revoked_watches
                        .entry(cid.clone())
                        .or_default()
                        .push((w.store, w.namespace));
                }
            }
            for t in topics {
                let res = format!("topic:{}", t.topic);
                let allowed = match store
                    .check_effective_acl(&caller_domain, owner_namedomain, &res)
                    .await
                {
                    Ok(chk) => parse_acl_level(&chk.effective_level)
                        .satisfies(AclLevel::Subscribe, &AclResourceKind::Topic),
                    Err(_) => false,
                };
                if !allowed {
                    revoked_topics.entry(cid.clone()).or_default().push(t.topic);
                }
            }
        }

        if revoked_watches.is_empty() && revoked_topics.is_empty() {
            return;
        }

        if let Ok(mut map) = self.clients.write() {
            for (cid, pairs) in revoked_watches {
                if let Some(rec) = map.get_mut(&cid) {
                    rec.watches.retain(|w| {
                        !(w.owner == owner_namedomain
                            && pairs.iter().any(|(s, ns)| *s == w.store && ns == &w.namespace))
                    });
                }
            }
            for (cid, names) in revoked_topics {
                if let Some(rec) = map.get_mut(&cid) {
                    rec.topics
                        .retain(|t| !(t.owner == owner_namedomain && names.contains(&t.topic)));
                }
            }
        }
    }

    /// Remove all SSE topic subscriptions targeting a deleted topic.
    pub fn remove_topic_subscriptions(&self, owner_namedomain: &str, topic: &str) {
        if let Ok(mut map) = self.clients.write() {
            for rec in map.values_mut() {
                rec.topics
                    .retain(|t| !(t.owner == owner_namedomain && t.topic == topic));
            }
        }
    }

    /// Force-close every SSE connection belonging to `plugin_name` (used on hot-remove or `namedomain` change).
    pub fn disconnect_plugin_clients(&self, plugin_name: &str) {
        let Ok(mut map) = self.clients.write() else {
            return;
        };
        let to_remove: Vec<String> = map
            .iter()
            .filter(|(_, rec)| rec.plugin_name == plugin_name)
            .map(|(cid, _)| cid.clone())
            .collect();
        for cid in &to_remove {
            map.remove(cid);
        }
        if !to_remove.is_empty() {
            log::info!(
                "🔌 Disconnected {} SSE client(s) for plugin '{}' after directory/namedomain change",
                to_remove.len(),
                plugin_name
            );
        }
    }

    /// Background task watching `PluginRegistry` and disconnecting SSE clients whenever
    /// a plugin directory is removed or its `namedomain` changes across a hot reload.
    pub fn spawn_registry_watcher(
        self: &Arc<Self>,
        plugins: Arc<PluginRegistry>,
        cancel: CancellationToken,
    ) {
        let hub = Arc::clone(self);
        let mut rx = plugins.subscribe();
        let mut known_domains: HashMap<String, String> = rx
            .borrow()
            .iter()
            .map(|p| (p.name.clone(), p.namedomain.clone()))
            .collect();

        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = cancel.cancelled() => break,
                    changed = rx.changed() => {
                        if changed.is_err() {
                            break;
                        }
                        let next_domains: HashMap<String, String> = rx
                            .borrow()
                            .iter()
                            .map(|p| (p.name.clone(), p.namedomain.clone()))
                            .collect();
                        for (name, prev_domain) in &known_domains {
                            match next_domains.get(name) {
                                Some(next_domain) if next_domain == prev_domain => {}
                                _ => hub.disconnect_plugin_clients(name),
                            }
                        }
                        known_domains = next_domains;
                    }
                }
            }
        });
    }
}

/// Evaluate ACL permissions for a batch of watch & topic inputs and partition into accepted/rejected.
pub async fn evaluate_subscriptions(
    plugins: &PluginRegistry,
    store: &PluginStore,
    caller_plugin_name: &str,
    caller_namedomain: &str,
    raw_watches: Vec<WatchSubInput>,
    raw_topics: Vec<TopicSubInput>,
) -> SubscriptionSyncResult {
    let mut result = SubscriptionSyncResult::default();
    let mut seen_watches: HashMap<(StoreKind, String, String, Option<String>), ()> = HashMap::new();
    let mut seen_topics: HashMap<(String, String), ()> = HashMap::new();

    for w in raw_watches {
        let store_kind = match StoreKind::parse(&w.store) {
            Ok(s) => s,
            Err(err) => {
                result.rejected.push(RejectedSub {
                    kind: "watch",
                    owner: w.owner,
                    target: w.namespace,
                    code: err.code,
                    message: err.message,
                });
                continue;
            }
        };
        let Some(target_info) = plugins.resolve_target(Some(caller_plugin_name), &w.owner) else {
            result.rejected.push(RejectedSub {
                kind: "watch",
                owner: w.owner,
                target: w.namespace,
                code: "PLUGIN_NOT_FOUND",
                message: "Target plugin not found".to_string(),
            });
            continue;
        };
        let resource_str = format!("{}:{}", store_kind.as_str(), w.namespace);
        let parsed = match parse_acl_resource(&resource_str) {
            Ok(p) if !p.is_wildcard() => p,
            Ok(_) => {
                result.rejected.push(RejectedSub {
                    kind: "watch",
                    owner: w.owner,
                    target: w.namespace,
                    code: "INVALID_ARGUMENT",
                    message: "Wildcard namespaces cannot be watched".to_string(),
                });
                continue;
            }
            Err(err) => {
                result.rejected.push(RejectedSub {
                    kind: "watch",
                    owner: w.owner,
                    target: w.namespace,
                    code: err.code,
                    message: err.message,
                });
                continue;
            }
        };

        let allowed = match store
            .check_effective_acl(caller_namedomain, &target_info.namedomain, &parsed.canonical())
            .await
        {
            Ok(chk) => {
                chk.is_self
                    || parse_acl_level(&chk.effective_level)
                        .satisfies(AclLevel::Read, &parsed.kind)
            }
            Err(err) => {
                result.rejected.push(RejectedSub {
                    kind: "watch",
                    owner: w.owner,
                    target: w.namespace,
                    code: err.code,
                    message: err.message,
                });
                continue;
            }
        };

        if !allowed {
            result.rejected.push(RejectedSub {
                kind: "watch",
                owner: w.owner,
                target: w.namespace,
                code: "ACL_DENIED",
                message: format!(
                    "Permission 'read' required on '{}' for target '{}'",
                    parsed.canonical(),
                    target_info.namedomain
                ),
            });
            continue;
        }

        let clean_prefix = w.prefix.filter(|p| !p.is_empty());
        let dedup_key = (
            store_kind,
            target_info.namedomain.clone(),
            w.namespace.clone(),
            clean_prefix.clone(),
        );
        if seen_watches.insert(dedup_key, ()).is_none() {
            result.accepted_watches.push(AcceptedWatch {
                store: store_kind,
                owner: target_info.namedomain,
                requested_owner: w.owner,
                namespace: w.namespace,
                prefix: clean_prefix,
            });
        }
    }

    for t in raw_topics {
        let Some(target_info) = plugins.resolve_target(Some(caller_plugin_name), &t.owner) else {
            result.rejected.push(RejectedSub {
                kind: "topic",
                owner: t.owner,
                target: t.topic,
                code: "PLUGIN_NOT_FOUND",
                message: "Target plugin not found".to_string(),
            });
            continue;
        };
        let resource_str = format!("topic:{}", t.topic);
        let parsed = match parse_acl_resource(&resource_str) {
            Ok(p) if !p.is_wildcard() => p,
            Ok(_) => {
                result.rejected.push(RejectedSub {
                    kind: "topic",
                    owner: t.owner,
                    target: t.topic,
                    code: "INVALID_ARGUMENT",
                    message: "Wildcard topics cannot be subscribed directly".to_string(),
                });
                continue;
            }
            Err(err) => {
                result.rejected.push(RejectedSub {
                    kind: "topic",
                    owner: t.owner,
                    target: t.topic,
                    code: err.code,
                    message: err.message,
                });
                continue;
            }
        };

        let allowed = match store
            .check_effective_acl(caller_namedomain, &target_info.namedomain, &parsed.canonical())
            .await
        {
            Ok(chk) => {
                chk.is_self
                    || parse_acl_level(&chk.effective_level)
                        .satisfies(AclLevel::Subscribe, &parsed.kind)
            }
            Err(err) => {
                result.rejected.push(RejectedSub {
                    kind: "topic",
                    owner: t.owner,
                    target: t.topic,
                    code: err.code,
                    message: err.message,
                });
                continue;
            }
        };

        if !allowed {
            result.rejected.push(RejectedSub {
                kind: "topic",
                owner: t.owner,
                target: t.topic,
                code: "ACL_DENIED",
                message: format!(
                    "Permission 'subscribe' required on '{}' for target '{}'",
                    parsed.canonical(),
                    target_info.namedomain
                ),
            });
            continue;
        }

        let dedup_key = (target_info.namedomain.clone(), t.topic.clone());
        if seen_topics.insert(dedup_key, ()).is_none() {
            result.accepted_topics.push(AcceptedTopic {
                owner: target_info.namedomain,
                requested_owner: t.owner,
                topic: t.topic,
            });
        }
    }

    result
}

/// Look up UI display metadata for a requester `namedomain` from currently installed plugins.
pub fn resolve_requester_display(
    plugins: &PluginRegistry,
    requester_namedomain: &str,
) -> RequesterDisplay {
    let best = plugins
        .list()
        .into_iter()
        .filter(|p| p.namedomain == requester_namedomain)
        .max_by(|a, b| {
            crate::plugin::compare_version_strs(&a.version, &b.version)
                .then_with(|| b.name.cmp(&a.name))
        });

    match best {
        Some(info) => RequesterDisplay {
            plugin_name: Some(info.name.clone()),
            name: Some(info.display_title().to_string()),
            installed: true,
        },
        None => RequesterDisplay {
            plugin_name: None,
            name: None,
            installed: false,
        },
    }
}

fn parse_acl_level(raw: &str) -> AclLevel {
    match raw {
        "read" => AclLevel::Read,
        "write" => AclLevel::Write,
        "subscribe" => AclLevel::Subscribe,
        _ => AclLevel::None,
    }
}
