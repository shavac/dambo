use super::acl::{AclLevel, AclRecord, check_acl_in_read_txn, is_namespace_visible_in_open_mode};
use super::{
    ACL_RECORDS_TABLE, CONFIG_NAMESPACE, INLINE_VALUES_MAX_BYTES, KV_DATA_TABLE, PluginStore,
    TOPIC_META_TABLE, exclusive_prefix_end,
};
use crate::plugin_types::{
    DataType, MAX_BYTES_PER_STORE, MAX_KEYS_PER_STORE, PluginApiError, TypedEntry, WriteEntryInput,
    infer_data_type, is_valid_key, is_valid_namespace, parse_acl_resource, parse_default_entry,
    serialize_and_check_entry_size, validate_schema_definition, validate_value_against,
};
use redb::{Database, ReadableTable};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;

/// Storage class selector (`persist` backed by `redb`, `memory` volatile in-process).
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "lowercase")]
pub enum StoreKind {
    Persist,
    Memory,
}

impl StoreKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Persist => "persist",
            Self::Memory => "memory",
        }
    }

    pub fn parse(raw: &str) -> Result<Self, PluginApiError> {
        match raw {
            "persist" => Ok(Self::Persist),
            "memory" => Ok(Self::Memory),
            other => Err(PluginApiError::invalid_argument(format!(
                "Invalid store '{}': expected 'persist' or 'memory'",
                other
            ))),
        }
    }
}

/// Merged config entry returned for `GET /api/v1/plugins/{target}` and `Dambo.config.schema()`.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ConfigEntryView {
    #[serde(rename = "type")]
    pub data_type: DataType,
    pub value: serde_json::Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub meta: Option<crate::plugin_types::DataMeta>,
    pub is_overridden: bool,
}

/// Quota counters maintained in memory via O(1) incremental updates.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct QuotaStats {
    pub keys: usize,
    pub bytes: usize,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum MutationOp {
    Set,
    Delete,
    Mset,
    Clear,
}

/// Mutation summary produced by data write/delete/clear calls for SSE `data.changed` fanout.
#[derive(Clone, Debug, PartialEq)]
pub struct DataMutationResult {
    pub store: StoreKind,
    pub owner_namedomain: String,
    pub namespace: String,
    pub op: MutationOp,
    pub keys: Vec<String>,
    pub inline_values: Option<BTreeMap<String, serde_json::Value>>,
}

/// Parse valid entries from raw `plugin.json` `defaults`, warning and skipping malformed items.
pub fn parse_defaults_map(
    raw_defaults: &BTreeMap<String, serde_json::Value>,
    owner_namedomain: &str,
) -> BTreeMap<String, TypedEntry> {
    let mut out = BTreeMap::new();
    for (k, raw_v) in raw_defaults {
        if !is_valid_key(k) {
            log::warn!(
                "Skipping invalid default config key '{}' in '{}'",
                k,
                owner_namedomain
            );
            continue;
        }
        match parse_default_entry(raw_v) {
            Ok(entry) => {
                out.insert(k.clone(), entry);
            }
            Err(e) => {
                log::warn!(
                    "Ignoring invalid default entry '{}' for '{}': {}",
                    k,
                    owner_namedomain,
                    e
                );
            }
        }
    }
    out
}

fn compute_inline_values(
    entries: &BTreeMap<String, TypedEntry>,
) -> Option<BTreeMap<String, serde_json::Value>> {
    let mut map = BTreeMap::new();
    for (k, v) in entries {
        map.insert(k.clone(), v.value.clone());
    }
    match serde_json::to_vec(&map) {
        Ok(bytes) if bytes.len() <= INLINE_VALUES_MAX_BYTES => Some(map),
        _ => None,
    }
}

fn adjust_quota_check(
    current: QuotaStats,
    old_bytes: Option<usize>,
    new_bytes: usize,
    owner: &str,
    store: StoreKind,
) -> Result<QuotaStats, PluginApiError> {
    let next_keys = match old_bytes {
        Some(_) => current.keys,
        None => current.keys + 1,
    };
    let prev = old_bytes.unwrap_or(0);
    let next_bytes = current.bytes.saturating_sub(prev) + new_bytes;

    if next_keys > MAX_KEYS_PER_STORE {
        return Err(PluginApiError::quota_exceeded(format!(
            "Plugin '{}' exceeded max key limit ({}) in '{}' store",
            owner,
            MAX_KEYS_PER_STORE,
            store.as_str()
        )));
    }
    if next_bytes > MAX_BYTES_PER_STORE {
        return Err(PluginApiError::quota_exceeded(format!(
            "Plugin '{}' exceeded max byte limit ({} bytes) in '{}' store",
            owner,
            MAX_BYTES_PER_STORE,
            store.as_str()
        )));
    }
    Ok(QuotaStats {
        keys: next_keys,
        bytes: next_bytes,
    })
}

fn apply_delete_quota(
    quotas: &mut HashMap<String, QuotaStats>,
    owner: &str,
    removed_keys: usize,
    removed_bytes: usize,
) {
    if let Some(q) = quotas.get_mut(owner) {
        q.keys = q.keys.saturating_sub(removed_keys);
        q.bytes = q.bytes.saturating_sub(removed_bytes);
        if q.keys == 0 {
            quotas.remove(owner);
        }
    }
}

fn build_updated_entry(
    is_owner: bool,
    is_default_locked: bool,
    existing: Option<&TypedEntry>,
    input: WriteEntryInput,
    key: &str,
) -> Result<(TypedEntry, Vec<u8>), PluginApiError> {
    if input.is_define_mode() {
        if !is_owner {
            return Err(PluginApiError::schema_locked(format!(
                "External plugins cannot modify schema for key '{}'",
                key
            )));
        }
        if is_default_locked {
            return Err(PluginApiError::schema_locked(format!(
                "Schema of default config key '{}' is locked by plugin.json defaults",
                key
            )));
        }
        let target_type = match input.data_type {
            Some(dt) => dt,
            None => match existing {
                Some(ex) => ex.data_type,
                None => infer_data_type(&input.value)?,
            },
        };
        let target_meta = input.meta;
        validate_schema_definition(target_type, target_meta.as_ref(), 0)?;
        validate_value_against(target_type, target_meta.as_ref(), &input.value, 0, key)?;
        let entry = TypedEntry {
            data_type: target_type,
            value: input.value,
            meta: target_meta,
        };
        let serialized = serialize_and_check_entry_size(&entry)?;
        Ok((entry, serialized))
    } else if let Some(ex) = existing {
        validate_value_against(ex.data_type, ex.meta.as_ref(), &input.value, 0, key)?;
        let entry = TypedEntry {
            data_type: ex.data_type,
            value: input.value,
            meta: ex.meta.clone(),
        };
        let serialized = serialize_and_check_entry_size(&entry)?;
        Ok((entry, serialized))
    } else {
        let inferred = infer_data_type(&input.value)?;
        validate_value_against(inferred, None, &input.value, 0, key)?;
        let entry = TypedEntry {
            data_type: inferred,
            value: input.value,
            meta: None,
        };
        let serialized = serialize_and_check_entry_size(&entry)?;
        Ok((entry, serialized))
    }
}

fn resolve_config_override(
    key: &str,
    default_entry: &TypedEntry,
    stored_opt: Option<&TypedEntry>,
    owner_namedomain: &str,
) -> (TypedEntry, bool) {
    let Some(stored) = stored_opt else {
        return (default_entry.clone(), false);
    };
    if stored.data_type == default_entry.data_type
        && validate_value_against(
            default_entry.data_type,
            default_entry.meta.as_ref(),
            &stored.value,
            0,
            key,
        )
        .is_ok()
    {
        (
            TypedEntry {
                data_type: default_entry.data_type,
                value: stored.value.clone(),
                meta: default_entry.meta.clone(),
            },
            true,
        )
    } else {
        log::warn!(
            "Stale config override for '{}' key '{}' violates plugin.json defaults schema; falling back to default",
            owner_namedomain,
            key
        );
        (default_entry.clone(), false)
    }
}

fn filter_and_limit_entries(
    mut entries: BTreeMap<String, TypedEntry>,
    keys_filter: Option<&[String]>,
    limit: usize,
) -> BTreeMap<String, TypedEntry> {
    if let Some(keys) = keys_filter {
        let mut selected = BTreeMap::new();
        for k in keys {
            if selected.len() >= limit {
                break;
            }
            if let Some(v) = entries.remove(k) {
                selected.insert(k.clone(), v);
            }
        }
        return selected;
    }
    if entries.len() <= limit {
        return entries;
    }
    entries.into_iter().take(limit).collect()
}

impl PluginStore {
    /// List accessible non-empty namespaces (plus `"config"` under `persist` when `defaults` is non-empty).
    pub async fn list_namespaces(
        &self,
        caller_namedomain: &str,
        owner_namedomain: &str,
        store: StoreKind,
        owner_defaults: &BTreeMap<String, serde_json::Value>,
    ) -> Result<Vec<String>, PluginApiError> {
        let mut candidates = BTreeSet::new();
        if store == StoreKind::Persist && !owner_defaults.is_empty() {
            candidates.insert(CONFIG_NAMESPACE.to_string());
        }

        match store {
            StoreKind::Persist => {
                let db = self.db.clone();
                let owner = owner_namedomain.to_string();
                let persisted_ns = self
                    .run_blocking(move || {
                        let read_txn = db.begin_read().map_err(|e| {
                            PluginApiError::internal(format!("redb begin_read failed: {}", e))
                        })?;
                        let table = read_txn.open_table(KV_DATA_TABLE).map_err(|e| {
                            PluginApiError::internal(format!("redb open kv_data failed: {}", e))
                        })?;
                        let owner_end = exclusive_prefix_end(&owner);
                        let mut set = BTreeSet::new();
                        let range = table
                            .range((owner.as_str(), "", "")..(owner_end.as_str(), "", ""))
                            .map_err(|e| {
                                PluginApiError::internal(format!("redb range failed: {}", e))
                            })?;
                        for item in range {
                            let (k_guard, _) = item.map_err(|e| {
                                PluginApiError::internal(format!("redb iter failed: {}", e))
                            })?;
                            let (_, ns, _) = k_guard.value();
                            set.insert(ns.to_string());
                        }
                        Ok(set)
                    })
                    .await?;
                candidates.extend(persisted_ns);
            }
            StoreKind::Memory => {
                let mem = self
                    .memory
                    .read()
                    .map_err(|_| PluginApiError::internal("Memory store lock poisoned"))?;
                let owner_end = exclusive_prefix_end(owner_namedomain);
                for ((_, ns, _), _) in mem.entries.range(
                    (owner_namedomain.to_string(), String::new(), String::new())
                        ..(owner_end, String::new(), String::new()),
                ) {
                    candidates.insert(ns.clone());
                }
            }
        }

        if caller_namedomain == owner_namedomain {
            return Ok(candidates.into_iter().collect());
        }

        let db = self.db.clone();
        let caller = caller_namedomain.to_string();
        let owner = owner_namedomain.to_string();
        let mode = self.security_mode;
        let list: Vec<String> = candidates.into_iter().collect();
        self.run_blocking(move || {
            let read_txn = db
                .begin_read()
                .map_err(|e| PluginApiError::internal(format!("redb begin_read failed: {}", e)))?;
            let acl_table = read_txn.open_table(ACL_RECORDS_TABLE).map_err(|e| {
                PluginApiError::internal(format!("redb open acl_records failed: {}", e))
            })?;
            let topic_table = read_txn.open_table(TOPIC_META_TABLE).map_err(|e| {
                PluginApiError::internal(format!("redb open topic_meta failed: {}", e))
            })?;
            let mut visible = Vec::new();
            for ns in list {
                let parsed = parse_acl_resource(&format!("{}:{}", store.as_str(), ns))?;
                let allowed = if mode.is_open() {
                    is_namespace_visible_in_open_mode(&acl_table, &caller, &owner, &parsed)?
                } else {
                    let eff = check_acl_in_read_txn(
                        &acl_table,
                        &topic_table,
                        &caller,
                        &owner,
                        &parsed,
                        mode,
                    )?;
                    eff.satisfies(AclLevel::Read, &parsed.kind)
                };
                if allowed {
                    visible.push(ns);
                }
            }
            Ok(visible)
        })
        .await
    }

    /// Read merged configuration view (`defaults` overlaid by validated `persist/config` values).
    pub async fn get_merged_config(
        &self,
        caller_namedomain: &str,
        owner_namedomain: &str,
        owner_defaults: &BTreeMap<String, serde_json::Value>,
    ) -> Result<BTreeMap<String, ConfigEntryView>, PluginApiError> {
        let parsed = Arc::new(parse_defaults_map(owner_defaults, owner_namedomain));
        self.get_merged_config_cached(caller_namedomain, owner_namedomain, &parsed)
            .await
    }

    /// Read merged configuration view using pre-parsed `defaults`.
    pub async fn get_merged_config_cached(
        &self,
        caller_namedomain: &str,
        owner_namedomain: &str,
        parsed_defaults: &Arc<BTreeMap<String, TypedEntry>>,
    ) -> Result<BTreeMap<String, ConfigEntryView>, PluginApiError> {
        let (res, _) = self
            .get_merged_config_cached_with_acl(caller_namedomain, owner_namedomain, parsed_defaults)
            .await?;
        Ok(res)
    }

    /// Read merged configuration view, also returning any `AutoGranted` ACL created in `Open` mode.
    pub(crate) async fn get_merged_config_cached_with_acl(
        &self,
        caller_namedomain: &str,
        owner_namedomain: &str,
        parsed_defaults: &Arc<BTreeMap<String, TypedEntry>>,
    ) -> Result<(BTreeMap<String, ConfigEntryView>, Option<AclRecord>), PluginApiError> {
        let db = self.db.clone();
        let caller = caller_namedomain.to_string();
        let owner = owner_namedomain.to_string();
        let parsed_defaults = Arc::clone(parsed_defaults);
        let this = self.clone_handles();
        self.run_blocking(move || {
            let auto_rec = this.require_data_access_or_auto_grant_sync(
                &caller,
                &owner,
                StoreKind::Persist,
                CONFIG_NAMESPACE,
                AclLevel::Read,
            )?;
            let stored = this.read_namespace_persist_sync(&db, &owner, CONFIG_NAMESPACE, None)?;

            let mut result = BTreeMap::new();
            for (k, def_entry) in parsed_defaults.iter() {
                let (merged, is_overridden) =
                    resolve_config_override(k, def_entry, stored.get(k), &owner);
                result.insert(
                    k.clone(),
                    ConfigEntryView {
                        data_type: merged.data_type,
                        value: merged.value,
                        meta: merged.meta,
                        is_overridden,
                    },
                );
            }
            for (k, st_entry) in stored {
                result.entry(k).or_insert_with(|| ConfigEntryView {
                    data_type: st_entry.data_type,
                    value: st_entry.value,
                    meta: st_entry.meta,
                    is_overridden: true,
                });
            }
            Ok((result, auto_rec))
        })
        .await
    }

    /// Get a single entry envelope; applies `defaults` merge on `persist/config`.
    pub async fn get_entry(
        &self,
        caller_namedomain: &str,
        owner_namedomain: &str,
        store: StoreKind,
        ns: &str,
        key: &str,
        owner_defaults: &BTreeMap<String, serde_json::Value>,
    ) -> Result<TypedEntry, PluginApiError> {
        let parsed = if store == StoreKind::Persist && ns == CONFIG_NAMESPACE {
            Arc::new(parse_defaults_map(owner_defaults, owner_namedomain))
        } else {
            Arc::new(BTreeMap::new())
        };
        self.get_entry_cached(caller_namedomain, owner_namedomain, store, ns, key, &parsed)
            .await
    }

    /// Get a single entry envelope using pre-parsed `defaults`.
    pub async fn get_entry_cached(
        &self,
        caller_namedomain: &str,
        owner_namedomain: &str,
        store: StoreKind,
        ns: &str,
        key: &str,
        parsed_defaults: &Arc<BTreeMap<String, TypedEntry>>,
    ) -> Result<TypedEntry, PluginApiError> {
        let (entry, _) = self
            .get_entry_cached_with_acl(
                caller_namedomain,
                owner_namedomain,
                store,
                ns,
                key,
                parsed_defaults,
            )
            .await?;
        Ok(entry)
    }

    /// Get a single entry envelope, also returning any `AutoGranted` ACL created in `Open` mode.
    pub(crate) async fn get_entry_cached_with_acl(
        &self,
        caller_namedomain: &str,
        owner_namedomain: &str,
        store: StoreKind,
        ns: &str,
        key: &str,
        parsed_defaults: &Arc<BTreeMap<String, TypedEntry>>,
    ) -> Result<(TypedEntry, Option<AclRecord>), PluginApiError> {
        if !is_valid_key(key) {
            return Err(PluginApiError::invalid_argument(format!(
                "Invalid key '{}'",
                key
            )));
        }

        let is_config = store == StoreKind::Persist && ns == CONFIG_NAMESPACE;
        match store {
            StoreKind::Persist => {
                let this = self.clone_handles();
                let caller = caller_namedomain.to_string();
                let owner = owner_namedomain.to_string();
                let ns_str = ns.to_string();
                let key_str = key.to_string();
                let defs_arc = is_config.then(|| Arc::clone(parsed_defaults));
                self.run_blocking(move || {
                    let auto_rec = this.require_data_access_or_auto_grant_sync(
                        &caller,
                        &owner,
                        StoreKind::Persist,
                        &ns_str,
                        AclLevel::Read,
                    )?;
                    let stored = this.read_single_persist_sync(&owner, &ns_str, &key_str)?;
                    if let Some(ref parsed_defs) = defs_arc
                        && let Some(def_entry) = parsed_defs.get(&key_str)
                    {
                        let (resolved, _) =
                            resolve_config_override(&key_str, def_entry, stored.as_ref(), &owner);
                        return Ok((resolved, auto_rec));
                    }
                    let entry = stored.ok_or_else(|| {
                        PluginApiError::key_not_found(format!(
                            "Key '{}/{}' not found in persist store for '{}'",
                            ns_str, key_str, owner
                        ))
                    })?;
                    Ok((entry, auto_rec))
                })
                .await
            }
            StoreKind::Memory => {
                if caller_namedomain == owner_namedomain {
                    if !is_valid_namespace(ns) {
                        return Err(PluginApiError::invalid_argument(format!(
                            "Invalid namespace '{}': must match [A-Za-z0-9_.-] (1..=128 bytes)",
                            ns
                        )));
                    }
                    return self
                        .read_single_memory_sync(owner_namedomain, ns, key)
                        .map(|e| (e, None));
                }
                let this = self.clone_handles();
                let caller = caller_namedomain.to_string();
                let owner = owner_namedomain.to_string();
                let ns_str = ns.to_string();
                let key_str = key.to_string();
                self.run_blocking(move || {
                    let auto_rec = this.require_data_access_or_auto_grant_sync(
                        &caller,
                        &owner,
                        StoreKind::Memory,
                        &ns_str,
                        AclLevel::Read,
                    )?;
                    let entry = this.read_single_memory_sync(&owner, &ns_str, &key_str)?;
                    Ok((entry, auto_rec))
                })
                .await
            }
        }
    }

    /// List entries in a namespace by optional `prefix`, explicit `keys` list, and optional `limit`.
    #[allow(clippy::too_many_arguments)]
    pub async fn list_entries(
        &self,
        caller_namedomain: &str,
        owner_namedomain: &str,
        store: StoreKind,
        ns: &str,
        prefix: Option<&str>,
        keys_filter: Option<&[String]>,
        limit: Option<usize>,
        owner_defaults: &BTreeMap<String, serde_json::Value>,
    ) -> Result<BTreeMap<String, TypedEntry>, PluginApiError> {
        let parsed = if store == StoreKind::Persist && ns == CONFIG_NAMESPACE {
            Arc::new(parse_defaults_map(owner_defaults, owner_namedomain))
        } else {
            Arc::new(BTreeMap::new())
        };
        self.list_entries_cached(
            caller_namedomain,
            owner_namedomain,
            store,
            ns,
            prefix,
            keys_filter,
            limit,
            &parsed,
        )
        .await
    }

    /// List entries in a namespace using pre-parsed `defaults`.
    #[allow(clippy::too_many_arguments)]
    pub async fn list_entries_cached(
        &self,
        caller_namedomain: &str,
        owner_namedomain: &str,
        store: StoreKind,
        ns: &str,
        prefix: Option<&str>,
        keys_filter: Option<&[String]>,
        limit: Option<usize>,
        parsed_defaults: &Arc<BTreeMap<String, TypedEntry>>,
    ) -> Result<BTreeMap<String, TypedEntry>, PluginApiError> {
        let (entries, _) = self
            .list_entries_cached_with_acl(
                caller_namedomain,
                owner_namedomain,
                store,
                ns,
                prefix,
                keys_filter,
                limit,
                parsed_defaults,
            )
            .await?;
        Ok(entries)
    }

    /// List entries in a namespace, also returning any `AutoGranted` ACL created in `Open` mode.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn list_entries_cached_with_acl(
        &self,
        caller_namedomain: &str,
        owner_namedomain: &str,
        store: StoreKind,
        ns: &str,
        prefix: Option<&str>,
        keys_filter: Option<&[String]>,
        limit: Option<usize>,
        parsed_defaults: &Arc<BTreeMap<String, TypedEntry>>,
    ) -> Result<(BTreeMap<String, TypedEntry>, Option<AclRecord>), PluginApiError> {
        if let Some(keys) = keys_filter {
            for k in keys {
                if !is_valid_key(k) {
                    return Err(PluginApiError::invalid_argument(format!(
                        "Invalid key '{}' in filter",
                        k
                    )));
                }
            }
        }

        let is_config = store == StoreKind::Persist && ns == CONFIG_NAMESPACE;
        let max_items = limit.unwrap_or(usize::MAX);
        match store {
            StoreKind::Persist => {
                let this = self.clone_handles();
                let caller = caller_namedomain.to_string();
                let owner = owner_namedomain.to_string();
                let ns_str = ns.to_string();
                let prefix_owned = prefix.map(ToString::to_string);
                let keys_owned = keys_filter.map(<[String]>::to_vec);
                let defs_arc = is_config.then(|| Arc::clone(parsed_defaults));

                self.run_blocking(move || {
                    let auto_rec = this.require_data_access_or_auto_grant_sync(
                        &caller,
                        &owner,
                        StoreKind::Persist,
                        &ns_str,
                        AclLevel::Read,
                    )?;
                    let mut stored = this.read_namespace_persist_sync(
                        &this.db,
                        &owner,
                        &ns_str,
                        if is_config {
                            None
                        } else {
                            prefix_owned.as_deref()
                        },
                    )?;

                    if let Some(ref parsed_defs) = defs_arc {
                        let mut merged = BTreeMap::new();
                        for (k, def_entry) in parsed_defs.iter() {
                            if let Some(pfx) = prefix_owned.as_deref()
                                && !k.starts_with(pfx)
                            {
                                continue;
                            }
                            let (resolved, _) =
                                resolve_config_override(k, def_entry, stored.get(k), &owner);
                            merged.insert(k.clone(), resolved);
                        }
                        for (k, st_entry) in stored {
                            if !parsed_defs.contains_key(&k) {
                                if let Some(pfx) = prefix_owned.as_deref()
                                    && !k.starts_with(pfx)
                                {
                                    continue;
                                }
                                merged.insert(k, st_entry);
                            }
                        }
                        stored = merged;
                    }

                    Ok((
                        filter_and_limit_entries(stored, keys_owned.as_deref(), max_items),
                        auto_rec,
                    ))
                })
                .await
            }
            StoreKind::Memory => {
                if caller_namedomain == owner_namedomain {
                    if !is_valid_namespace(ns) {
                        return Err(PluginApiError::invalid_argument(format!(
                            "Invalid namespace '{}': must match [A-Za-z0-9_.-] (1..=128 bytes)",
                            ns
                        )));
                    }
                    return self
                        .list_entries_memory_sync(
                            owner_namedomain,
                            ns,
                            prefix,
                            keys_filter,
                            max_items,
                        )
                        .map(|m| (m, None));
                }
                let this = self.clone_handles();
                let caller = caller_namedomain.to_string();
                let owner = owner_namedomain.to_string();
                let ns_str = ns.to_string();
                let prefix_owned = prefix.map(ToString::to_string);
                let keys_owned = keys_filter.map(<[String]>::to_vec);

                self.run_blocking(move || {
                    let auto_rec = this.require_data_access_or_auto_grant_sync(
                        &caller,
                        &owner,
                        StoreKind::Memory,
                        &ns_str,
                        AclLevel::Read,
                    )?;
                    let map = this.list_entries_memory_sync(
                        &owner,
                        &ns_str,
                        prefix_owned.as_deref(),
                        keys_owned.as_deref(),
                        max_items,
                    )?;
                    Ok((map, auto_rec))
                })
                .await
            }
        }
    }

    /// Write or schema-define a single KV entry.
    #[allow(clippy::too_many_arguments)]
    pub async fn put_entry(
        &self,
        caller_namedomain: &str,
        owner_namedomain: &str,
        store: StoreKind,
        ns: &str,
        key: &str,
        input: WriteEntryInput,
        owner_defaults: &BTreeMap<String, serde_json::Value>,
    ) -> Result<(TypedEntry, DataMutationResult), PluginApiError> {
        let parsed = if store == StoreKind::Persist && ns == CONFIG_NAMESPACE {
            Arc::new(parse_defaults_map(owner_defaults, owner_namedomain))
        } else {
            Arc::new(BTreeMap::new())
        };
        self.put_entry_cached(
            caller_namedomain,
            owner_namedomain,
            store,
            ns,
            key,
            input,
            &parsed,
        )
        .await
    }

    /// Write or schema-define a single KV entry using pre-parsed `defaults`.
    #[allow(clippy::too_many_arguments)]
    pub async fn put_entry_cached(
        &self,
        caller_namedomain: &str,
        owner_namedomain: &str,
        store: StoreKind,
        ns: &str,
        key: &str,
        input: WriteEntryInput,
        parsed_defaults: &Arc<BTreeMap<String, TypedEntry>>,
    ) -> Result<(TypedEntry, DataMutationResult), PluginApiError> {
        if !is_valid_key(key) {
            return Err(PluginApiError::invalid_argument(format!(
                "Invalid key '{}'",
                key
            )));
        }

        let is_owner = caller_namedomain == owner_namedomain;
        if store == StoreKind::Memory && is_owner {
            if !is_valid_namespace(ns) {
                return Err(PluginApiError::invalid_argument(format!(
                    "Invalid namespace '{}': must match [A-Za-z0-9_.-] (1..=128 bytes)",
                    ns
                )));
            }
            return self.put_entry_memory_sync(true, owner_namedomain, ns, key, input);
        }

        let is_config = store == StoreKind::Persist && ns == CONFIG_NAMESPACE;
        let caller = caller_namedomain.to_string();
        let owner = owner_namedomain.to_string();
        let ns_str = ns.to_string();
        let key_str = key.to_string();
        let defs_arc = is_config.then(|| Arc::clone(parsed_defaults));
        let this = self.clone_handles();

        self.run_blocking(move || {
            let _ = this.require_data_access_or_auto_grant_sync(
                &caller,
                &owner,
                store,
                &ns_str,
                AclLevel::Write,
            )?;
            let default_entry = defs_arc.as_ref().and_then(|d| d.get(&key_str));
            let is_default_locked = default_entry.is_some();

            match store {
                StoreKind::Persist => {
                    let write_txn = this.db.begin_write().map_err(|e| {
                        PluginApiError::internal(format!("redb begin_write failed: {}", e))
                    })?;
                    let mut quotas = this
                        .persist_quotas
                        .lock()
                        .map_err(|_| PluginApiError::internal("Persist quota lock poisoned"))?;
                    let (final_entry, next_quota) = {
                        let mut table = write_txn.open_table(KV_DATA_TABLE).map_err(|e| {
                            PluginApiError::internal(format!("redb open kv_data failed: {}", e))
                        })?;
                        let existing_raw = table
                            .get((owner.as_str(), ns_str.as_str(), key_str.as_str()))
                            .map_err(|e| {
                                PluginApiError::internal(format!("redb get failed: {}", e))
                            })?;
                        let old_bytes = existing_raw.as_ref().map(|g| g.value().len());
                        let existing_stored: Option<TypedEntry> = existing_raw
                            .map(|g| serde_json::from_slice(g.value()))
                            .transpose()
                            .map_err(|e| {
                                PluginApiError::internal(format!("Corrupt redb entry: {}", e))
                            })?;
                        let schema_basis = default_entry.or(existing_stored.as_ref());

                        let (entry, serialized) = build_updated_entry(
                            is_owner,
                            is_default_locked,
                            schema_basis,
                            input,
                            &key_str,
                        )?;
                        let current_quota = quotas.get(&owner).copied().unwrap_or_default();
                        let next_quota = adjust_quota_check(
                            current_quota,
                            old_bytes,
                            serialized.len(),
                            &owner,
                            StoreKind::Persist,
                        )?;
                        table
                            .insert(
                                (owner.as_str(), ns_str.as_str(), key_str.as_str()),
                                serialized.as_slice(),
                            )
                            .map_err(|e| {
                                PluginApiError::internal(format!("redb insert failed: {}", e))
                            })?;
                        (entry, next_quota)
                    };
                    write_txn.commit().map_err(|e| {
                        PluginApiError::internal(format!("redb commit failed: {}", e))
                    })?;
                    quotas.insert(owner.clone(), next_quota);
                    drop(quotas);

                    let mut val_map = BTreeMap::new();
                    val_map.insert(key_str.clone(), final_entry.clone());
                    let inline_values = compute_inline_values(&val_map);
                    Ok((
                        final_entry,
                        DataMutationResult {
                            store: StoreKind::Persist,
                            owner_namedomain: owner,
                            namespace: ns_str,
                            op: MutationOp::Set,
                            keys: vec![key_str],
                            inline_values,
                        },
                    ))
                }
                StoreKind::Memory => {
                    this.put_entry_memory_sync(is_owner, &owner, &ns_str, &key_str, input)
                }
            }
        })
        .await
    }

    /// Atomically validate and batch-write entries in one `redb` write transaction (or memory lock).
    pub async fn mset_entries(
        &self,
        caller_namedomain: &str,
        owner_namedomain: &str,
        store: StoreKind,
        ns: &str,
        entries: BTreeMap<String, WriteEntryInput>,
        owner_defaults: &BTreeMap<String, serde_json::Value>,
    ) -> Result<(BTreeMap<String, TypedEntry>, Option<DataMutationResult>), PluginApiError> {
        let parsed = if store == StoreKind::Persist && ns == CONFIG_NAMESPACE {
            Arc::new(parse_defaults_map(owner_defaults, owner_namedomain))
        } else {
            Arc::new(BTreeMap::new())
        };
        self.mset_entries_cached(
            caller_namedomain,
            owner_namedomain,
            store,
            ns,
            entries,
            &parsed,
        )
        .await
    }

    /// Atomically validate and batch-write entries using pre-parsed `defaults`.
    pub async fn mset_entries_cached(
        &self,
        caller_namedomain: &str,
        owner_namedomain: &str,
        store: StoreKind,
        ns: &str,
        entries: BTreeMap<String, WriteEntryInput>,
        parsed_defaults: &Arc<BTreeMap<String, TypedEntry>>,
    ) -> Result<(BTreeMap<String, TypedEntry>, Option<DataMutationResult>), PluginApiError> {
        for key in entries.keys() {
            if !is_valid_key(key) {
                return Err(PluginApiError::invalid_argument(format!(
                    "Invalid key '{}'",
                    key
                )));
            }
        }
        if entries.is_empty() {
            return Ok((BTreeMap::new(), None));
        }

        let is_owner = caller_namedomain == owner_namedomain;
        if store == StoreKind::Memory && is_owner {
            if !is_valid_namespace(ns) {
                return Err(PluginApiError::invalid_argument(format!(
                    "Invalid namespace '{}': must match [A-Za-z0-9_.-] (1..=128 bytes)",
                    ns
                )));
            }
            return self.mset_entries_memory_sync(true, owner_namedomain, ns, entries);
        }

        let is_config = store == StoreKind::Persist && ns == CONFIG_NAMESPACE;
        let caller = caller_namedomain.to_string();
        let owner = owner_namedomain.to_string();
        let ns_str = ns.to_string();
        let defs_arc = is_config.then(|| Arc::clone(parsed_defaults));
        let this = self.clone_handles();

        self.run_blocking(move || {
            let _ = this.require_data_access_or_auto_grant_sync(
                &caller,
                &owner,
                store,
                &ns_str,
                AclLevel::Write,
            )?;

            match store {
                StoreKind::Persist => {
                    let write_txn = this.db.begin_write().map_err(|e| {
                        PluginApiError::internal(format!("redb begin_write failed: {}", e))
                    })?;
                    let mut quotas = this
                        .persist_quotas
                        .lock()
                        .map_err(|_| PluginApiError::internal("Persist quota lock poisoned"))?;
                    let (committed, next_quota) = {
                        let mut table = write_txn.open_table(KV_DATA_TABLE).map_err(|e| {
                            PluginApiError::internal(format!("redb open kv_data failed: {}", e))
                        })?;
                        let mut running_quota = quotas.get(&owner).copied().unwrap_or_default();

                        let mut staged: Vec<(String, TypedEntry, Vec<u8>)> =
                            Vec::with_capacity(entries.len());
                        for (k, input) in entries {
                            let existing_raw = table
                                .get((owner.as_str(), ns_str.as_str(), k.as_str()))
                                .map_err(|e| {
                                    PluginApiError::internal(format!("redb get failed: {}", e))
                                })?;
                            let old_bytes = existing_raw.as_ref().map(|g| g.value().len());
                            let existing_stored: Option<TypedEntry> = existing_raw
                                .map(|g| serde_json::from_slice(g.value()))
                                .transpose()
                                .map_err(|e| {
                                    PluginApiError::internal(format!("Corrupt redb entry: {}", e))
                                })?;
                            let default_entry = defs_arc.as_ref().and_then(|d| d.get(&k));
                            let is_default_locked = default_entry.is_some();
                            let schema_basis = default_entry.or(existing_stored.as_ref());

                            let (entry, serialized) = build_updated_entry(
                                is_owner,
                                is_default_locked,
                                schema_basis,
                                input,
                                &k,
                            )?;
                            running_quota = adjust_quota_check(
                                running_quota,
                                old_bytes,
                                serialized.len(),
                                &owner,
                                StoreKind::Persist,
                            )?;
                            staged.push((k, entry, serialized));
                        }

                        let mut committed = BTreeMap::new();
                        for (k, entry, serialized) in staged {
                            table
                                .insert(
                                    (owner.as_str(), ns_str.as_str(), k.as_str()),
                                    serialized.as_slice(),
                                )
                                .map_err(|e| {
                                    PluginApiError::internal(format!("redb insert failed: {}", e))
                                })?;
                            committed.insert(k, entry);
                        }
                        (committed, running_quota)
                    };
                    write_txn.commit().map_err(|e| {
                        PluginApiError::internal(format!("redb commit failed: {}", e))
                    })?;
                    quotas.insert(owner.clone(), next_quota);
                    drop(quotas);

                    let keys: Vec<String> = committed.keys().cloned().collect();
                    let inline_values = compute_inline_values(&committed);
                    Ok((
                        committed,
                        Some(DataMutationResult {
                            store: StoreKind::Persist,
                            owner_namedomain: owner,
                            namespace: ns_str,
                            op: MutationOp::Mset,
                            keys,
                            inline_values,
                        }),
                    ))
                }
                StoreKind::Memory => {
                    this.mset_entries_memory_sync(is_owner, &owner, &ns_str, entries)
                }
            }
        })
        .await
    }

    /// Delete a single key. Returns `(existed, optional_mutation)`.
    pub async fn delete_entry(
        &self,
        caller_namedomain: &str,
        owner_namedomain: &str,
        store: StoreKind,
        ns: &str,
        key: &str,
    ) -> Result<(bool, Option<DataMutationResult>), PluginApiError> {
        if !is_valid_key(key) {
            return Err(PluginApiError::invalid_argument(format!(
                "Invalid key '{}'",
                key
            )));
        }

        if store == StoreKind::Memory && caller_namedomain == owner_namedomain {
            if !is_valid_namespace(ns) {
                return Err(PluginApiError::invalid_argument(format!(
                    "Invalid namespace '{}': must match [A-Za-z0-9_.-] (1..=128 bytes)",
                    ns
                )));
            }
            return self.delete_entry_memory_sync(owner_namedomain, ns, key);
        }

        let caller = caller_namedomain.to_string();
        let owner = owner_namedomain.to_string();
        let ns_str = ns.to_string();
        let key_str = key.to_string();
        let this = self.clone_handles();

        self.run_blocking(move || {
            let _ = this.require_data_access_or_auto_grant_sync(
                &caller,
                &owner,
                store,
                &ns_str,
                AclLevel::Write,
            )?;

            match store {
                StoreKind::Persist => {
                    let write_txn = this.db.begin_write().map_err(|e| {
                        PluginApiError::internal(format!("redb begin_write failed: {}", e))
                    })?;
                    let mut quotas = this
                        .persist_quotas
                        .lock()
                        .map_err(|_| PluginApiError::internal("Persist quota lock poisoned"))?;
                    let removed_bytes = {
                        let mut table = write_txn.open_table(KV_DATA_TABLE).map_err(|e| {
                            PluginApiError::internal(format!("redb open kv_data failed: {}", e))
                        })?;
                        let old = table
                            .remove((owner.as_str(), ns_str.as_str(), key_str.as_str()))
                            .map_err(|e| {
                                PluginApiError::internal(format!("redb remove failed: {}", e))
                            })?;
                        old.map(|g| g.value().len())
                    };
                    write_txn.commit().map_err(|e| {
                        PluginApiError::internal(format!("redb commit failed: {}", e))
                    })?;

                    if let Some(len) = removed_bytes {
                        apply_delete_quota(&mut quotas, &owner, 1, len);
                        drop(quotas);
                        Ok((
                            true,
                            Some(DataMutationResult {
                                store: StoreKind::Persist,
                                owner_namedomain: owner,
                                namespace: ns_str,
                                op: MutationOp::Delete,
                                keys: vec![key_str],
                                inline_values: None,
                            }),
                        ))
                    } else {
                        Ok((false, None))
                    }
                }
                StoreKind::Memory => this.delete_entry_memory_sync(&owner, &ns_str, &key_str),
            }
        })
        .await
    }

    /// Clear every entry in a namespace (on `persist/config`, restores all defaults by removing overrides).
    pub async fn clear_namespace(
        &self,
        caller_namedomain: &str,
        owner_namedomain: &str,
        store: StoreKind,
        ns: &str,
    ) -> Result<(usize, Option<DataMutationResult>), PluginApiError> {
        if store == StoreKind::Memory && caller_namedomain == owner_namedomain {
            if !is_valid_namespace(ns) {
                return Err(PluginApiError::invalid_argument(format!(
                    "Invalid namespace '{}': must match [A-Za-z0-9_.-] (1..=128 bytes)",
                    ns
                )));
            }
            return self.clear_namespace_memory_sync(owner_namedomain, ns);
        }

        let caller = caller_namedomain.to_string();
        let owner = owner_namedomain.to_string();
        let ns_str = ns.to_string();
        let this = self.clone_handles();

        self.run_blocking(move || {
            let _ = this.require_data_access_or_auto_grant_sync(
                &caller,
                &owner,
                store,
                &ns_str,
                AclLevel::Write,
            )?;

            match store {
                StoreKind::Persist => {
                    let write_txn = this.db.begin_write().map_err(|e| {
                        PluginApiError::internal(format!("redb begin_write failed: {}", e))
                    })?;
                    let mut quotas = this
                        .persist_quotas
                        .lock()
                        .map_err(|_| PluginApiError::internal("Persist quota lock poisoned"))?;
                    let mut removed_keys = Vec::new();
                    let mut removed_bytes = 0usize;
                    {
                        let mut table = write_txn.open_table(KV_DATA_TABLE).map_err(|e| {
                            PluginApiError::internal(format!("redb open kv_data failed: {}", e))
                        })?;
                        let ns_end = exclusive_prefix_end(&ns_str);
                        let keys_to_delete: Vec<String> = {
                            let range = table
                                .range(
                                    (owner.as_str(), ns_str.as_str(), "")
                                        ..(owner.as_str(), ns_end.as_str(), ""),
                                )
                                .map_err(|e| {
                                    PluginApiError::internal(format!("redb range failed: {}", e))
                                })?;
                            let mut list = Vec::new();
                            for item in range {
                                let (k_guard, _) = item.map_err(|e| {
                                    PluginApiError::internal(format!("redb iter failed: {}", e))
                                })?;
                                let (_, _, k) = k_guard.value();
                                list.push(k.to_string());
                            }
                            list
                        };
                        for k in keys_to_delete {
                            if let Some(old) = table
                                .remove((owner.as_str(), ns_str.as_str(), k.as_str()))
                                .map_err(|e| {
                                    PluginApiError::internal(format!("redb remove failed: {}", e))
                                })?
                            {
                                removed_bytes += old.value().len();
                                removed_keys.push(k);
                            }
                        }
                    }
                    write_txn.commit().map_err(|e| {
                        PluginApiError::internal(format!("redb commit failed: {}", e))
                    })?;

                    let count = removed_keys.len();
                    if count > 0 {
                        apply_delete_quota(&mut quotas, &owner, count, removed_bytes);
                        drop(quotas);
                        Ok((
                            count,
                            Some(DataMutationResult {
                                store: StoreKind::Persist,
                                owner_namedomain: owner,
                                namespace: ns_str,
                                op: MutationOp::Clear,
                                keys: removed_keys,
                                inline_values: None,
                            }),
                        ))
                    } else {
                        Ok((0, None))
                    }
                }
                StoreKind::Memory => this.clear_namespace_memory_sync(&owner, &ns_str),
            }
        })
        .await
    }

    fn read_single_memory_sync(
        &self,
        owner: &str,
        ns: &str,
        key: &str,
    ) -> Result<TypedEntry, PluginApiError> {
        let mem = self
            .memory
            .read()
            .map_err(|_| PluginApiError::internal("Memory store lock poisoned"))?;
        mem.entries
            .get(&(owner.to_string(), ns.to_string(), key.to_string()))
            .map(|(entry, _)| entry.clone())
            .ok_or_else(|| {
                PluginApiError::key_not_found(format!(
                    "Key '{}/{}' not found in memory store for '{}'",
                    ns, key, owner
                ))
            })
    }

    fn list_entries_memory_sync(
        &self,
        owner: &str,
        ns: &str,
        prefix: Option<&str>,
        keys_filter: Option<&[String]>,
        max_items: usize,
    ) -> Result<BTreeMap<String, TypedEntry>, PluginApiError> {
        let mem = self
            .memory
            .read()
            .map_err(|_| PluginApiError::internal("Memory store lock poisoned"))?;
        let mut map = BTreeMap::new();
        let ns_end = exclusive_prefix_end(ns);
        for ((_, _, k), (entry, _)) in mem.entries.range(
            (owner.to_string(), ns.to_string(), String::new())
                ..(owner.to_string(), ns_end, String::new()),
        ) {
            if let Some(pfx) = prefix
                && !k.starts_with(pfx)
            {
                continue;
            }
            map.insert(k.clone(), entry.clone());
        }
        Ok(filter_and_limit_entries(map, keys_filter, max_items))
    }

    fn put_entry_memory_sync(
        &self,
        is_owner: bool,
        owner: &str,
        ns: &str,
        key: &str,
        input: WriteEntryInput,
    ) -> Result<(TypedEntry, DataMutationResult), PluginApiError> {
        let mut mem = self
            .memory
            .write()
            .map_err(|_| PluginApiError::internal("Memory store lock poisoned"))?;
        let map_key = (owner.to_string(), ns.to_string(), key.to_string());
        let existing = mem.entries.get(&map_key).map(|(e, len)| (e, *len));
        let old_bytes = existing.map(|(_, len)| len);
        let schema_basis = existing.map(|(e, _)| e);

        let (entry, serialized) = build_updated_entry(is_owner, false, schema_basis, input, key)?;
        let current_quota = mem.quotas.get(owner).copied().unwrap_or_default();
        let next_quota = adjust_quota_check(
            current_quota,
            old_bytes,
            serialized.len(),
            owner,
            StoreKind::Memory,
        )?;
        mem.quotas.insert(owner.to_string(), next_quota);
        mem.entries
            .insert(map_key, (entry.clone(), serialized.len()));

        let mut val_map = BTreeMap::new();
        val_map.insert(key.to_string(), entry.clone());
        let inline_values = compute_inline_values(&val_map);
        Ok((
            entry,
            DataMutationResult {
                store: StoreKind::Memory,
                owner_namedomain: owner.to_string(),
                namespace: ns.to_string(),
                op: MutationOp::Set,
                keys: vec![key.to_string()],
                inline_values,
            },
        ))
    }

    fn mset_entries_memory_sync(
        &self,
        is_owner: bool,
        owner: &str,
        ns: &str,
        entries: BTreeMap<String, WriteEntryInput>,
    ) -> Result<(BTreeMap<String, TypedEntry>, Option<DataMutationResult>), PluginApiError> {
        let mut mem = self
            .memory
            .write()
            .map_err(|_| PluginApiError::internal("Memory store lock poisoned"))?;
        let mut running_quota = mem.quotas.get(owner).copied().unwrap_or_default();
        let mut staged: Vec<(String, TypedEntry, Vec<u8>)> = Vec::with_capacity(entries.len());

        for (k, input) in entries {
            let map_key = (owner.to_string(), ns.to_string(), k.clone());
            let existing = mem.entries.get(&map_key).map(|(e, len)| (e, *len));
            let old_bytes = existing.map(|(_, len)| len);
            let schema_basis = existing.map(|(e, _)| e);

            let (entry, serialized) =
                build_updated_entry(is_owner, false, schema_basis, input, &k)?;
            running_quota = adjust_quota_check(
                running_quota,
                old_bytes,
                serialized.len(),
                owner,
                StoreKind::Memory,
            )?;
            staged.push((k, entry, serialized));
        }

        mem.quotas.insert(owner.to_string(), running_quota);
        let mut committed = BTreeMap::new();
        for (k, entry, serialized) in staged {
            let map_key = (owner.to_string(), ns.to_string(), k.clone());
            mem.entries
                .insert(map_key, (entry.clone(), serialized.len()));
            committed.insert(k, entry);
        }

        let keys: Vec<String> = committed.keys().cloned().collect();
        let inline_values = compute_inline_values(&committed);
        Ok((
            committed,
            Some(DataMutationResult {
                store: StoreKind::Memory,
                owner_namedomain: owner.to_string(),
                namespace: ns.to_string(),
                op: MutationOp::Mset,
                keys,
                inline_values,
            }),
        ))
    }

    fn delete_entry_memory_sync(
        &self,
        owner: &str,
        ns: &str,
        key: &str,
    ) -> Result<(bool, Option<DataMutationResult>), PluginApiError> {
        let mut mem = self
            .memory
            .write()
            .map_err(|_| PluginApiError::internal("Memory store lock poisoned"))?;
        let map_key = (owner.to_string(), ns.to_string(), key.to_string());
        if let Some((_, len)) = mem.entries.remove(&map_key) {
            apply_delete_quota(&mut mem.quotas, owner, 1, len);
            Ok((
                true,
                Some(DataMutationResult {
                    store: StoreKind::Memory,
                    owner_namedomain: owner.to_string(),
                    namespace: ns.to_string(),
                    op: MutationOp::Delete,
                    keys: vec![key.to_string()],
                    inline_values: None,
                }),
            ))
        } else {
            Ok((false, None))
        }
    }

    fn clear_namespace_memory_sync(
        &self,
        owner: &str,
        ns: &str,
    ) -> Result<(usize, Option<DataMutationResult>), PluginApiError> {
        let mut mem = self
            .memory
            .write()
            .map_err(|_| PluginApiError::internal("Memory store lock poisoned"))?;
        let ns_end = exclusive_prefix_end(ns);
        let to_delete: Vec<(String, String, String)> = mem
            .entries
            .range(
                (owner.to_string(), ns.to_string(), String::new())
                    ..(owner.to_string(), ns_end, String::new()),
            )
            .map(|(k, _)| k.clone())
            .collect();
        let mut removed_keys = Vec::with_capacity(to_delete.len());
        let mut removed_bytes = 0usize;
        for k in to_delete {
            if let Some((_, len)) = mem.entries.remove(&k) {
                removed_bytes += len;
                removed_keys.push(k.2);
            }
        }
        let count = removed_keys.len();
        let mutation = if count > 0 {
            apply_delete_quota(&mut mem.quotas, owner, count, removed_bytes);
            Some(DataMutationResult {
                store: StoreKind::Memory,
                owner_namedomain: owner.to_string(),
                namespace: ns.to_string(),
                op: MutationOp::Clear,
                keys: removed_keys,
                inline_values: None,
            })
        } else {
            None
        };
        Ok((count, mutation))
    }

    fn read_single_persist_sync(
        &self,
        owner: &str,
        ns: &str,
        key: &str,
    ) -> Result<Option<TypedEntry>, PluginApiError> {
        let read_txn = self
            .db
            .begin_read()
            .map_err(|e| PluginApiError::internal(format!("redb begin_read failed: {}", e)))?;
        let table = read_txn
            .open_table(KV_DATA_TABLE)
            .map_err(|e| PluginApiError::internal(format!("redb open kv_data failed: {}", e)))?;
        table
            .get((owner, ns, key))
            .map_err(|e| PluginApiError::internal(format!("redb get failed: {}", e)))?
            .map(|g| {
                serde_json::from_slice::<TypedEntry>(g.value())
                    .map_err(|e| PluginApiError::internal(format!("Corrupt redb entry: {}", e)))
            })
            .transpose()
    }

    fn read_namespace_persist_sync(
        &self,
        db: &Database,
        owner: &str,
        ns: &str,
        prefix: Option<&str>,
    ) -> Result<BTreeMap<String, TypedEntry>, PluginApiError> {
        let read_txn = db
            .begin_read()
            .map_err(|e| PluginApiError::internal(format!("redb begin_read failed: {}", e)))?;
        let table = read_txn
            .open_table(KV_DATA_TABLE)
            .map_err(|e| PluginApiError::internal(format!("redb open kv_data failed: {}", e)))?;
        let ns_end = exclusive_prefix_end(ns);
        let start_key = prefix.unwrap_or("");
        let mut out = BTreeMap::new();
        for item in table
            .range((owner, ns, start_key)..(owner, ns_end.as_str(), ""))
            .map_err(|e| PluginApiError::internal(format!("redb range failed: {}", e)))?
        {
            let (k_guard, v_guard) =
                item.map_err(|e| PluginApiError::internal(format!("redb entry failed: {}", e)))?;
            let (_, _, k) = k_guard.value();
            if let Some(pfx) = prefix
                && !k.starts_with(pfx)
            {
                break;
            }
            let entry: TypedEntry = serde_json::from_slice(v_guard.value())
                .map_err(|e| PluginApiError::internal(format!("Corrupt redb entry: {}", e)))?;
            out.insert(k.to_string(), entry);
        }
        Ok(out)
    }
}
