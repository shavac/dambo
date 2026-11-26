use super::{PluginStore, TOPIC_META_TABLE, exclusive_prefix_end};
use crate::plugin_types::{PluginApiError, is_valid_topic_name};
use chrono::Utc;
use redb::ReadableTable;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(super) struct TopicMetaValue {
    pub(super) public: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) description: Option<String>,
    pub(super) updated_at: i64,
}

/// Persisted topic declaration owned by a plugin `namedomain`.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct TopicMeta {
    pub name: String,
    pub public: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub updated_at: i64,
}

impl PluginStore {
    /// Create or update persisted topic metadata. Returns `(TopicMeta, demoted_to_private)`.
    pub async fn configure_topic(
        &self,
        owner_namedomain: &str,
        topic_name: &str,
        public: bool,
        description: Option<String>,
    ) -> Result<(TopicMeta, bool), PluginApiError> {
        self.upsert_topic(owner_namedomain, topic_name, Some(public), description)
            .await
    }

    /// Ensure topic metadata exists on publish, preserving existing visibility if `public_opt` is `None`.
    pub async fn ensure_topic_for_publish(
        &self,
        owner_namedomain: &str,
        topic_name: &str,
        public_opt: Option<bool>,
        description: Option<String>,
    ) -> Result<(TopicMeta, bool), PluginApiError> {
        self.upsert_topic(owner_namedomain, topic_name, public_opt, description)
            .await
    }

    async fn upsert_topic(
        &self,
        owner_namedomain: &str,
        topic_name: &str,
        public_opt: Option<bool>,
        description: Option<String>,
    ) -> Result<(TopicMeta, bool), PluginApiError> {
        if !is_valid_topic_name(topic_name) {
            return Err(PluginApiError::invalid_argument(format!(
                "Invalid topic name '{}': must match [A-Za-z0-9_.-] (1..=128 bytes)",
                topic_name
            )));
        }
        let now_ms = Utc::now().timestamp_millis();
        let db = self.db.clone();
        let owner = owner_namedomain.to_string();
        let name = topic_name.to_string();
        let clean_desc = description.and_then(|s| {
            let trimmed = s.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_string())
            }
        });

        self.run_blocking(move || {
            let write_txn = db
                .begin_write()
                .map_err(|e| PluginApiError::internal(format!("redb begin_write failed: {}", e)))?;
            let (meta, demoted) = {
                let mut table = write_txn.open_table(TOPIC_META_TABLE).map_err(|e| {
                    PluginApiError::internal(format!("redb open topic_meta failed: {}", e))
                })?;
                let prev: Option<TopicMetaValue> = table
                    .get((owner.as_str(), name.as_str()))
                    .map_err(|e| PluginApiError::internal(format!("redb get failed: {}", e)))?
                    .map(|g| serde_json::from_slice(g.value()))
                    .transpose()
                    .map_err(|e| PluginApiError::internal(format!("Corrupt topic_meta: {}", e)))?;

                let was_public = prev.as_ref().is_some_and(|p| p.public);
                let next_public = public_opt.unwrap_or(was_public);
                let next_desc = clean_desc.or_else(|| prev.and_then(|p| p.description));

                let val = TopicMetaValue {
                    public: next_public,
                    description: next_desc.clone(),
                    updated_at: now_ms,
                };
                let bytes = serde_json::to_vec(&val).map_err(|e| {
                    PluginApiError::internal(format!("Failed to serialize topic_meta: {}", e))
                })?;
                table
                    .insert((owner.as_str(), name.as_str()), bytes.as_slice())
                    .map_err(|e| PluginApiError::internal(format!("redb insert failed: {}", e)))?;
                (
                    TopicMeta {
                        name: name.clone(),
                        public: next_public,
                        description: next_desc,
                        updated_at: now_ms,
                    },
                    was_public && !next_public,
                )
            };
            write_txn
                .commit()
                .map_err(|e| PluginApiError::internal(format!("redb commit failed: {}", e)))?;
            Ok((meta, demoted))
        })
        .await
    }

    /// Delete persisted topic metadata. Returns whether the topic existed.
    pub async fn delete_topic(
        &self,
        owner_namedomain: &str,
        topic_name: &str,
    ) -> Result<bool, PluginApiError> {
        if !is_valid_topic_name(topic_name) {
            return Err(PluginApiError::invalid_argument(format!(
                "Invalid topic name '{}'",
                topic_name
            )));
        }
        let db = self.db.clone();
        let owner = owner_namedomain.to_string();
        let name = topic_name.to_string();
        self.run_blocking(move || {
            let write_txn = db
                .begin_write()
                .map_err(|e| PluginApiError::internal(format!("redb begin_write failed: {}", e)))?;
            let existed = {
                let mut table = write_txn.open_table(TOPIC_META_TABLE).map_err(|e| {
                    PluginApiError::internal(format!("redb open topic_meta failed: {}", e))
                })?;
                let removed = table
                    .remove((owner.as_str(), name.as_str()))
                    .map_err(|e| PluginApiError::internal(format!("redb remove failed: {}", e)))?;
                removed.is_some()
            };
            write_txn
                .commit()
                .map_err(|e| PluginApiError::internal(format!("redb commit failed: {}", e)))?;
            Ok(existed)
        })
        .await
    }

    /// List all persisted topics declared by `owner_namedomain`.
    pub async fn list_topics(
        &self,
        owner_namedomain: &str,
    ) -> Result<Vec<TopicMeta>, PluginApiError> {
        let db = self.db.clone();
        let owner = owner_namedomain.to_string();
        self.run_blocking(move || {
            let read_txn = db
                .begin_read()
                .map_err(|e| PluginApiError::internal(format!("redb begin_read failed: {}", e)))?;
            let table = read_txn.open_table(TOPIC_META_TABLE).map_err(|e| {
                PluginApiError::internal(format!("redb open topic_meta failed: {}", e))
            })?;
            let owner_end = exclusive_prefix_end(&owner);
            let mut list = Vec::new();
            for item in table
                .range((owner.as_str(), "")..(owner_end.as_str(), ""))
                .map_err(|e| PluginApiError::internal(format!("redb range failed: {}", e)))?
            {
                let (k_guard, v_guard) = item
                    .map_err(|e| PluginApiError::internal(format!("redb entry failed: {}", e)))?;
                let (_, topic_name) = k_guard.value();
                let val: TopicMetaValue = serde_json::from_slice(v_guard.value()).map_err(|e| {
                    PluginApiError::internal(format!("Corrupt topic_meta JSON: {}", e))
                })?;
                list.push(TopicMeta {
                    name: topic_name.to_string(),
                    public: val.public,
                    description: val.description,
                    updated_at: val.updated_at,
                });
            }
            Ok(list)
        })
        .await
    }

    /// List all persisted topics across all plugin owners in a single `redb` read transaction.
    pub async fn list_all_topics_grouped(
        &self,
    ) -> Result<HashMap<String, Vec<TopicMeta>>, PluginApiError> {
        let db = self.db.clone();
        self.run_blocking(move || {
            let read_txn = db
                .begin_read()
                .map_err(|e| PluginApiError::internal(format!("redb begin_read failed: {}", e)))?;
            let table = read_txn.open_table(TOPIC_META_TABLE).map_err(|e| {
                PluginApiError::internal(format!("redb open topic_meta failed: {}", e))
            })?;
            let mut grouped: HashMap<String, Vec<TopicMeta>> = HashMap::new();
            for item in table
                .iter()
                .map_err(|e| PluginApiError::internal(format!("redb iter failed: {}", e)))?
            {
                let (k_guard, v_guard) = item
                    .map_err(|e| PluginApiError::internal(format!("redb entry failed: {}", e)))?;
                let (owner, topic_name) = k_guard.value();
                let val: TopicMetaValue = serde_json::from_slice(v_guard.value()).map_err(|e| {
                    PluginApiError::internal(format!("Corrupt topic_meta JSON: {}", e))
                })?;
                grouped
                    .entry(owner.to_string())
                    .or_default()
                    .push(TopicMeta {
                        name: topic_name.to_string(),
                        public: val.public,
                        description: val.description,
                        updated_at: val.updated_at,
                    });
            }
            Ok(grouped)
        })
        .await
    }
}
