use crate::plugin_types::{
    AclResourceKind, DataType, MAX_BYTES_PER_STORE, MAX_KEYS_PER_STORE, ParsedAclResource,
    PluginApiError, TypedEntry, WriteEntryInput, infer_data_type, is_false, is_valid_key,
    is_valid_namespace, is_valid_topic_name, parse_acl_resource, parse_default_entry,
    serialize_and_check_entry_size, validate_schema_definition, validate_value_against,
};
use anyhow::Result;
use chrono::Utc;
use redb::{Database, ReadableTable, TableDefinition};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, RwLock};

/// Maximum serialized byte size of inline `values` attached to a `data.changed` SSE event (`16 KB`).
pub const INLINE_VALUES_MAX_BYTES: usize = 16 * 1024;

/// Cooldown window (ms) before a denied ACL request can be re-submitted.
pub const ACL_DENY_COOLDOWN_MS: i64 = 5_000;

/// Reserved namespace under `persist` merged with `plugin.json` `defaults` at read time.
pub const CONFIG_NAMESPACE: &str = "config";

const KV_DATA_TABLE: TableDefinition<(&str, &str, &str), &[u8]> = TableDefinition::new("kv_data");
const ACL_RECORDS_TABLE: TableDefinition<(&str, &str, &str), &[u8]> =
    TableDefinition::new("acl_records");
const TOPIC_META_TABLE: TableDefinition<(&str, &str), &[u8]> = TableDefinition::new("topic_meta");

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

/// Effective permission level stored in `acl_records` and returned by `check_effective_acl`.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum AclLevel {
    None,
    Read,
    Write,
    Subscribe,
}

impl AclLevel {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Read => "read",
            Self::Write => "write",
            Self::Subscribe => "subscribe",
        }
    }

    pub fn from_str_lossy(raw: &str) -> Self {
        match raw {
            "read" => Self::Read,
            "write" => Self::Write,
            "subscribe" => Self::Subscribe,
            _ => Self::None,
        }
    }

    pub fn valid_for_request(self, kind: &AclResourceKind) -> bool {
        match kind {
            AclResourceKind::Persist | AclResourceKind::Memory => {
                matches!(self, Self::Read | Self::Write)
            }
            AclResourceKind::Topic => matches!(self, Self::Subscribe),
        }
    }

    pub fn valid_for_grant(self, kind: &AclResourceKind) -> bool {
        self == Self::None || self.valid_for_request(kind)
    }

    pub fn satisfies(self, required: Self, kind: &AclResourceKind) -> bool {
        match kind {
            AclResourceKind::Persist | AclResourceKind::Memory => match required {
                Self::None => true,
                Self::Read => matches!(self, Self::Read | Self::Write),
                Self::Write => self == Self::Write,
                Self::Subscribe => false,
            },
            AclResourceKind::Topic => match required {
                Self::None => true,
                Self::Subscribe => self == Self::Subscribe,
                _ => false,
            },
        }
    }
}

/// Request lifecycle status in unified single-table `acl_records`.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum AclStatus {
    Pending,
    Granted,
    Denied,
}

impl AclStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Granted => "granted",
            Self::Denied => "denied",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
struct AclRecordValue {
    level: AclLevel,
    status: AclStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    requested_level: Option<AclLevel>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    note: Option<String>,
    /// Tracks whether the exact record had an explicit denial before a subsequent re-request,
    /// preventing a pending re-request from temporarily inheriting wildcard access.
    #[serde(default, skip_serializing_if = "is_false")]
    prior_denied: bool,
    updated_at: i64,
}

/// Public representation of an ACL row keyed by `(owner, requester, resource)`.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct AclRecord {
    pub owner: String,
    pub requester: String,
    pub resource: String,
    pub level: AclLevel,
    pub status: AclStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requested_level: Option<AclLevel>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    pub updated_at: i64,
}

impl AclRecord {
    fn from_parts(owner: &str, requester: &str, resource: &str, v: AclRecordValue) -> Self {
        Self {
            owner: owner.to_string(),
            requester: requester.to_string(),
            resource: resource.to_string(),
            level: v.level,
            status: v.status,
            requested_level: v.requested_level,
            reason: v.reason,
            note: v.note,
            updated_at: v.updated_at,
        }
    }
}

/// Outcome of `check_effective_acl`.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct AclCheckResult {
    pub is_self: bool,
    pub effective_level: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
struct TopicMetaValue {
    public: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    updated_at: i64,
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

#[derive(Default)]
struct MemoryState {
    entries: BTreeMap<(String, String, String), (TypedEntry, usize)>,
    quotas: HashMap<String, QuotaStats>,
}

/// Unified storage and ACL engine managing persistent `redb` tables and volatile memory KV state.
pub struct PluginStore {
    db: Arc<Database>,
    persist_quotas: Arc<Mutex<HashMap<String, QuotaStats>>>,
    memory: Arc<RwLock<MemoryState>>,
}

fn exclusive_prefix_end(prefix: &str) -> String {
    let mut end = String::with_capacity(prefix.len() + 1);
    end.push_str(prefix);
    end.push('\0');
    end
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

fn apply_delete_quota(quotas: &mut HashMap<String, QuotaStats>, owner: &str, removed_bytes: usize) {
    if let Some(q) = quotas.get_mut(owner) {
        q.keys = q.keys.saturating_sub(1);
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

fn check_acl_in_read_txn<
    R: ReadableTable<(&'static str, &'static str, &'static str), &'static [u8]>,
    T: ReadableTable<(&'static str, &'static str), &'static [u8]>,
>(
    acl_table: &R,
    topic_table: &T,
    caller_namedomain: &str,
    owner_namedomain: &str,
    parsed: &ParsedAclResource,
) -> Result<AclLevel, PluginApiError> {
    if caller_namedomain == owner_namedomain {
        return Ok(match parsed.kind {
            AclResourceKind::Persist | AclResourceKind::Memory => AclLevel::Write,
            AclResourceKind::Topic => AclLevel::Subscribe,
        });
    }

    // Topic public check short-circuits ACL when exact topic is declared `public: true`
    if parsed.kind == AclResourceKind::Topic
        && !parsed.is_wildcard()
        && let Some(guard) = topic_table
            .get((owner_namedomain, parsed.scope.as_str()))
            .map_err(|e| PluginApiError::internal(format!("redb topic_meta read failed: {}", e)))?
    {
        let meta: TopicMetaValue = serde_json::from_slice(guard.value()).map_err(|e| {
            PluginApiError::internal(format!("Corrupt topic_meta JSON in redb: {}", e))
        })?;
        if meta.public {
            return Ok(AclLevel::Subscribe);
        }
    }

    let exact_key = parsed.canonical();
    let wild_key = parsed.wildcard();

    let mut blocked_by_exact_deny = false;
    if let Some(guard) = acl_table
        .get((owner_namedomain, caller_namedomain, exact_key.as_str()))
        .map_err(|e| PluginApiError::internal(format!("redb acl_records read failed: {}", e)))?
    {
        let rec: AclRecordValue = serde_json::from_slice(guard.value()).map_err(|e| {
            PluginApiError::internal(format!("Corrupt acl_records JSON in redb: {}", e))
        })?;
        if rec.level != AclLevel::None {
            return Ok(if parsed.is_persist_config() {
                AclLevel::Read
            } else {
                rec.level
            });
        }
        if rec.status == AclStatus::Denied || rec.prior_denied {
            blocked_by_exact_deny = true;
        }
    }

    if !blocked_by_exact_deny
        && !parsed.is_wildcard()
        && let Some(guard) = acl_table
            .get((owner_namedomain, caller_namedomain, wild_key.as_str()))
            .map_err(|e| {
                PluginApiError::internal(format!("redb acl_records wildcard read failed: {}", e))
            })?
    {
        let rec: AclRecordValue = serde_json::from_slice(guard.value()).map_err(|e| {
            PluginApiError::internal(format!("Corrupt acl_records JSON in redb: {}", e))
        })?;
        if rec.level != AclLevel::None {
            return Ok(if parsed.is_persist_config() {
                AclLevel::Read
            } else {
                rec.level
            });
        }
    }

    Ok(AclLevel::None)
}

impl PluginStore {
    /// Open or create a `redb` database at `db_path` and initialize plugin tables and O(1) quota counters.
    pub fn open(db_path: PathBuf) -> Result<Self> {
        let db = crate::db::open_database(&db_path)?;
        Self::from_database(db)
    }

    /// Wrap an existing shared `redb::Database`, initialize plugin tables, and build initial O(1) quota counters.
    pub fn from_database(db: Arc<Database>) -> Result<Self> {
        let write_txn = db.begin_write()?;
        {
            let _ = write_txn.open_table(KV_DATA_TABLE)?;
            let _ = write_txn.open_table(ACL_RECORDS_TABLE)?;
            let _ = write_txn.open_table(TOPIC_META_TABLE)?;
        }
        write_txn.commit()?;

        let mut initial_quotas: HashMap<String, QuotaStats> = HashMap::new();
        {
            let read_txn = db.begin_read()?;
            let table = read_txn.open_table(KV_DATA_TABLE)?;
            for item in table.iter()? {
                let (k_guard, v_guard) = item?;
                let (owner, _, _) = k_guard.value();
                let stats = initial_quotas.entry(owner.to_string()).or_default();
                stats.keys += 1;
                stats.bytes += v_guard.value().len();
            }
        }

        Ok(Self {
            db,
            persist_quotas: Arc::new(Mutex::new(initial_quotas)),
            memory: Arc::new(RwLock::new(MemoryState::default())),
        })
    }

    /// Expose current in-memory quota stats for diagnostics and testing.
    #[cfg(test)]
    pub fn quota_stats(&self, owner_namedomain: &str, store: StoreKind) -> QuotaStats {
        match store {
            StoreKind::Persist => self
                .persist_quotas
                .lock()
                .ok()
                .and_then(|q| q.get(owner_namedomain).copied())
                .unwrap_or_default(),
            StoreKind::Memory => self
                .memory
                .read()
                .ok()
                .and_then(|m| m.quotas.get(owner_namedomain).copied())
                .unwrap_or_default(),
        }
    }

    fn require_data_access_sync(
        &self,
        caller_namedomain: &str,
        owner_namedomain: &str,
        store: StoreKind,
        ns: &str,
        required: AclLevel,
    ) -> Result<(), PluginApiError> {
        if !is_valid_namespace(ns) {
            return Err(PluginApiError::invalid_argument(format!(
                "Invalid namespace '{}': must match [A-Za-z0-9_.-] (1..=128 bytes)",
                ns
            )));
        }
        if caller_namedomain == owner_namedomain {
            return Ok(());
        }
        if store == StoreKind::Persist && ns == CONFIG_NAMESPACE && required == AclLevel::Write {
            return Err(PluginApiError::acl_denied(format!(
                "Reserved namespace 'persist:config' of '{}' can only be modified by owner plugin",
                owner_namedomain
            ))
            .with_details(serde_json::json!({
                "target_namedomain": owner_namedomain,
                "resource": "persist:config",
                "required_level": "write"
            })));
        }

        let resource_str = format!("{}:{}", store.as_str(), ns);
        let parsed = parse_acl_resource(&resource_str)?;
        let read_txn = self
            .db
            .begin_read()
            .map_err(|e| PluginApiError::internal(format!("redb begin_read failed: {}", e)))?;
        let acl_table = read_txn.open_table(ACL_RECORDS_TABLE).map_err(|e| {
            PluginApiError::internal(format!("redb open acl_records failed: {}", e))
        })?;
        let topic_table = read_txn
            .open_table(TOPIC_META_TABLE)
            .map_err(|e| PluginApiError::internal(format!("redb open topic_meta failed: {}", e)))?;
        let effective = check_acl_in_read_txn(
            &acl_table,
            &topic_table,
            caller_namedomain,
            owner_namedomain,
            &parsed,
        )?;
        if !effective.satisfies(required, &parsed.kind) {
            return Err(PluginApiError::acl_denied(format!(
                "Permission '{}' required on '{}' for target '{}'",
                required.as_str(),
                resource_str,
                owner_namedomain
            ))
            .with_details(serde_json::json!({
                "target_namedomain": owner_namedomain,
                "resource": resource_str,
                "required_level": required.as_str()
            })));
        }
        Ok(())
    }

    async fn run_blocking<F, T>(&self, f: F) -> Result<T, PluginApiError>
    where
        F: FnOnce() -> Result<T, PluginApiError> + Send + 'static,
        T: Send + 'static,
    {
        tokio::task::spawn_blocking(f)
            .await
            .map_err(|e| PluginApiError::internal(format!("Blocking storage task failed: {}", e)))?
    }

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
                let eff =
                    check_acl_in_read_txn(&acl_table, &topic_table, &caller, &owner, &parsed)?;
                if eff.satisfies(AclLevel::Read, &parsed.kind) {
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
        let db = self.db.clone();
        let caller = caller_namedomain.to_string();
        let owner = owner_namedomain.to_string();
        let parsed_defaults = Arc::clone(parsed_defaults);
        let this = self.clone_handles();
        self.run_blocking(move || {
            this.require_data_access_sync(
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
            Ok(result)
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
                    this.require_data_access_sync(
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
                        return Ok(resolved);
                    }
                    stored.ok_or_else(|| {
                        PluginApiError::key_not_found(format!(
                            "Key '{}/{}' not found in persist store for '{}'",
                            ns_str, key_str, owner
                        ))
                    })
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
                    return self.read_single_memory_sync(owner_namedomain, ns, key);
                }
                let this = self.clone_handles();
                let caller = caller_namedomain.to_string();
                let owner = owner_namedomain.to_string();
                let ns_str = ns.to_string();
                let key_str = key.to_string();
                self.run_blocking(move || {
                    this.require_data_access_sync(
                        &caller,
                        &owner,
                        StoreKind::Memory,
                        &ns_str,
                        AclLevel::Read,
                    )?;
                    this.read_single_memory_sync(&owner, &ns_str, &key_str)
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
                let prefix_owned = prefix.map(|s| s.to_string());
                let keys_owned = keys_filter.map(|k| k.to_vec());
                let defs_arc = is_config.then(|| Arc::clone(parsed_defaults));

                self.run_blocking(move || {
                    this.require_data_access_sync(
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

                    Ok(filter_and_limit_entries(
                        stored,
                        keys_owned.as_deref(),
                        max_items,
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
                    return self.list_entries_memory_sync(
                        owner_namedomain,
                        ns,
                        prefix,
                        keys_filter,
                        max_items,
                    );
                }
                let this = self.clone_handles();
                let caller = caller_namedomain.to_string();
                let owner = owner_namedomain.to_string();
                let ns_str = ns.to_string();
                let prefix_owned = prefix.map(|s| s.to_string());
                let keys_owned = keys_filter.map(|k| k.to_vec());

                self.run_blocking(move || {
                    this.require_data_access_sync(
                        &caller,
                        &owner,
                        StoreKind::Memory,
                        &ns_str,
                        AclLevel::Read,
                    )?;
                    this.list_entries_memory_sync(
                        &owner,
                        &ns_str,
                        prefix_owned.as_deref(),
                        keys_owned.as_deref(),
                        max_items,
                    )
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
            this.require_data_access_sync(&caller, &owner, store, &ns_str, AclLevel::Write)?;
            let default_entry = defs_arc.as_ref().and_then(|d| d.get(&key_str));
            let is_default_locked = default_entry.is_some();

            match store {
                StoreKind::Persist => {
                    let write_txn = this.db.begin_write().map_err(|e| {
                        PluginApiError::internal(format!("redb begin_write failed: {}", e))
                    })?;
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
                        let current_quota = this
                            .persist_quotas
                            .lock()
                            .map_err(|_| PluginApiError::internal("Persist quota lock poisoned"))?
                            .get(&owner)
                            .copied()
                            .unwrap_or_default();
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
                    if let Ok(mut quotas) = this.persist_quotas.lock() {
                        quotas.insert(owner.clone(), next_quota);
                    }

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
            this.require_data_access_sync(&caller, &owner, store, &ns_str, AclLevel::Write)?;

            match store {
                StoreKind::Persist => {
                    let write_txn = this.db.begin_write().map_err(|e| {
                        PluginApiError::internal(format!("redb begin_write failed: {}", e))
                    })?;
                    let (committed, next_quota) = {
                        let mut table = write_txn.open_table(KV_DATA_TABLE).map_err(|e| {
                            PluginApiError::internal(format!("redb open kv_data failed: {}", e))
                        })?;
                        let mut running_quota = this
                            .persist_quotas
                            .lock()
                            .map_err(|_| PluginApiError::internal("Persist quota lock poisoned"))?
                            .get(&owner)
                            .copied()
                            .unwrap_or_default();

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
                    if let Ok(mut quotas) = this.persist_quotas.lock() {
                        quotas.insert(owner.clone(), next_quota);
                    }

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
            this.require_data_access_sync(&caller, &owner, store, &ns_str, AclLevel::Write)?;

            match store {
                StoreKind::Persist => {
                    let write_txn = this.db.begin_write().map_err(|e| {
                        PluginApiError::internal(format!("redb begin_write failed: {}", e))
                    })?;
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
                        if let Ok(mut quotas) = this.persist_quotas.lock() {
                            apply_delete_quota(&mut quotas, &owner, len);
                        }
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
            this.require_data_access_sync(&caller, &owner, store, &ns_str, AclLevel::Write)?;

            match store {
                StoreKind::Persist => {
                    let write_txn = this.db.begin_write().map_err(|e| {
                        PluginApiError::internal(format!("redb begin_write failed: {}", e))
                    })?;
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
                        if let Ok(mut quotas) = this.persist_quotas.lock()
                            && let Some(q) = quotas.get_mut(&owner)
                        {
                            q.keys = q.keys.saturating_sub(count);
                            q.bytes = q.bytes.saturating_sub(removed_bytes);
                            if q.keys == 0 {
                                quotas.remove(&owner);
                            }
                        }
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

    /// Check effective ACL permission for `caller_namedomain` on `(owner_namedomain, resource)`.
    pub async fn check_effective_acl(
        &self,
        caller_namedomain: &str,
        owner_namedomain: &str,
        resource: &str,
    ) -> Result<AclCheckResult, PluginApiError> {
        let parsed = parse_acl_resource(resource)?;
        if caller_namedomain == owner_namedomain {
            return Ok(AclCheckResult {
                is_self: true,
                effective_level: "self".to_string(),
            });
        }
        let db = self.db.clone();
        let caller = caller_namedomain.to_string();
        let owner = owner_namedomain.to_string();
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
            let eff = check_acl_in_read_txn(&acl_table, &topic_table, &caller, &owner, &parsed)?;
            Ok(AclCheckResult {
                is_self: false,
                effective_level: eff.as_str().to_string(),
            })
        })
        .await
    }

    /// Evaluate a batch of `(caller_namedomain, owner_namedomain, ParsedAclResource)` checks in a single
    /// `redb` read transaction (or in-memory with zero `redb` access when all checks are self-owned).
    pub async fn check_effective_acl_batch(
        &self,
        queries: Vec<(String, String, ParsedAclResource)>,
    ) -> Result<Vec<AclLevel>, PluginApiError> {
        if queries.is_empty() {
            return Ok(Vec::new());
        }
        if queries.iter().all(|(caller, owner, _)| caller == owner) {
            return Ok(queries
                .into_iter()
                .map(|(_, _, parsed)| match parsed.kind {
                    AclResourceKind::Persist | AclResourceKind::Memory => AclLevel::Write,
                    AclResourceKind::Topic => AclLevel::Subscribe,
                })
                .collect());
        }
        let db = self.db.clone();
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
            let mut results = Vec::with_capacity(queries.len());
            for (caller, owner, parsed) in &queries {
                let eff = check_acl_in_read_txn(&acl_table, &topic_table, caller, owner, parsed)?;
                results.push(eff);
            }
            Ok(results)
        })
        .await
    }

    /// Submit a new or upgrade permission request. Preserves lower existing level while pending.
    pub async fn request_acl(
        &self,
        caller_namedomain: &str,
        owner_namedomain: &str,
        resource: &str,
        requested_level: AclLevel,
        reason: Option<String>,
    ) -> Result<AclRecord, PluginApiError> {
        self.request_acl_at(
            caller_namedomain,
            owner_namedomain,
            resource,
            requested_level,
            reason,
            Utc::now().timestamp_millis(),
        )
        .await
    }

    /// Timestamp-injectable implementation of `request_acl` for deterministic cooldown testing.
    pub async fn request_acl_at(
        &self,
        caller_namedomain: &str,
        owner_namedomain: &str,
        resource: &str,
        requested_level: AclLevel,
        reason: Option<String>,
        now_ms: i64,
    ) -> Result<AclRecord, PluginApiError> {
        if caller_namedomain == owner_namedomain {
            return Err(PluginApiError::invalid_argument(
                "Plugins already possess full access to their own resources",
            ));
        }
        let parsed = parse_acl_resource(resource)?;
        if !requested_level.valid_for_request(&parsed.kind) {
            return Err(PluginApiError::invalid_argument(format!(
                "Requested level '{}' is not valid for resource '{}'",
                requested_level.as_str(),
                parsed.canonical()
            )));
        }
        if parsed.is_persist_config() && requested_level == AclLevel::Write {
            return Err(PluginApiError::acl_denied(
                "Cannot request 'write' on reserved namespace 'persist:config'",
            ));
        }

        let canonical_res = parsed.canonical();
        let db = self.db.clone();
        let caller = caller_namedomain.to_string();
        let owner = owner_namedomain.to_string();
        let clean_reason = reason.and_then(|s| {
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
            let next_val = {
                let mut table = write_txn.open_table(ACL_RECORDS_TABLE).map_err(|e| {
                    PluginApiError::internal(format!("redb open acl_records failed: {}", e))
                })?;
                let existing: Option<AclRecordValue> = table
                    .get((owner.as_str(), caller.as_str(), canonical_res.as_str()))
                    .map_err(|e| PluginApiError::internal(format!("redb get failed: {}", e)))?
                    .map(|g| serde_json::from_slice(g.value()))
                    .transpose()
                    .map_err(|e| PluginApiError::internal(format!("Corrupt acl record: {}", e)))?;

                let (preserved_level, note, prior_denied) = if let Some(prev) = existing {
                    if prev.status == AclStatus::Denied
                        && now_ms.saturating_sub(prev.updated_at) < ACL_DENY_COOLDOWN_MS
                    {
                        let retry_ms =
                            ACL_DENY_COOLDOWN_MS - now_ms.saturating_sub(prev.updated_at);
                        return Err(PluginApiError::too_many_requests(format!(
                            "Request for '{}' was recently denied; retry in {} ms",
                            canonical_res, retry_ms
                        ))
                        .with_details(serde_json::json!({ "retry_after_ms": retry_ms })));
                    }
                    (
                        prev.level,
                        prev.note,
                        prev.status == AclStatus::Denied || prev.prior_denied,
                    )
                } else {
                    (AclLevel::None, None, false)
                };

                let val = AclRecordValue {
                    level: preserved_level,
                    status: AclStatus::Pending,
                    requested_level: Some(requested_level),
                    reason: clean_reason,
                    note,
                    prior_denied,
                    updated_at: now_ms,
                };
                let bytes = serde_json::to_vec(&val).map_err(|e| {
                    PluginApiError::internal(format!("Failed to serialize acl record: {}", e))
                })?;
                table
                    .insert(
                        (owner.as_str(), caller.as_str(), canonical_res.as_str()),
                        bytes.as_slice(),
                    )
                    .map_err(|e| PluginApiError::internal(format!("redb insert failed: {}", e)))?;
                val
            };
            write_txn
                .commit()
                .map_err(|e| PluginApiError::internal(format!("redb commit failed: {}", e)))?;
            Ok(AclRecord::from_parts(
                &owner,
                &caller,
                &canonical_res,
                next_val,
            ))
        })
        .await
    }

    /// Approve, modify, or deny/revoke an ACL entry (`level == None` sets `status = Denied`).
    pub async fn set_acl_grant(
        &self,
        owner_namedomain: &str,
        requester_namedomain: &str,
        resource: &str,
        level: AclLevel,
        note: Option<String>,
    ) -> Result<AclRecord, PluginApiError> {
        let parsed = parse_acl_resource(resource)?;
        if !level.valid_for_grant(&parsed.kind) {
            return Err(PluginApiError::invalid_argument(format!(
                "Grant level '{}' is not valid for resource '{}'",
                level.as_str(),
                parsed.canonical()
            )));
        }
        if parsed.is_persist_config() && level == AclLevel::Write {
            return Err(PluginApiError::acl_denied(
                "Cannot grant 'write' on reserved namespace 'persist:config'",
            ));
        }

        let canonical_res = parsed.canonical();
        let now_ms = Utc::now().timestamp_millis();
        let db = self.db.clone();
        let owner = owner_namedomain.to_string();
        let requester = requester_namedomain.to_string();
        let clean_note = note.and_then(|s| {
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
            let next_val = {
                let mut table = write_txn.open_table(ACL_RECORDS_TABLE).map_err(|e| {
                    PluginApiError::internal(format!("redb open acl_records failed: {}", e))
                })?;
                let existing: Option<AclRecordValue> = table
                    .get((owner.as_str(), requester.as_str(), canonical_res.as_str()))
                    .map_err(|e| PluginApiError::internal(format!("redb get failed: {}", e)))?
                    .map(|g| serde_json::from_slice(g.value()))
                    .transpose()
                    .map_err(|e| PluginApiError::internal(format!("Corrupt acl record: {}", e)))?;

                let status = if level == AclLevel::None {
                    AclStatus::Denied
                } else {
                    AclStatus::Granted
                };
                let val = AclRecordValue {
                    level,
                    status,
                    requested_level: None,
                    reason: existing.as_ref().and_then(|e| e.reason.clone()),
                    note: clean_note,
                    prior_denied: level == AclLevel::None,
                    updated_at: now_ms,
                };
                let bytes = serde_json::to_vec(&val).map_err(|e| {
                    PluginApiError::internal(format!("Failed to serialize acl record: {}", e))
                })?;
                table
                    .insert(
                        (owner.as_str(), requester.as_str(), canonical_res.as_str()),
                        bytes.as_slice(),
                    )
                    .map_err(|e| PluginApiError::internal(format!("redb insert failed: {}", e)))?;
                val
            };
            write_txn
                .commit()
                .map_err(|e| PluginApiError::internal(format!("redb commit failed: {}", e)))?;
            Ok(AclRecord::from_parts(
                &owner,
                &requester,
                &canonical_res,
                next_val,
            ))
        })
        .await
    }

    /// Permanently remove an ACL record from `acl_records`.
    pub async fn delete_acl_record(
        &self,
        owner_namedomain: &str,
        requester_namedomain: &str,
        resource: &str,
    ) -> Result<Option<AclRecord>, PluginApiError> {
        let parsed = parse_acl_resource(resource)?;
        let canonical_res = parsed.canonical();
        let db = self.db.clone();
        let owner = owner_namedomain.to_string();
        let requester = requester_namedomain.to_string();

        self.run_blocking(move || {
            let write_txn = db
                .begin_write()
                .map_err(|e| PluginApiError::internal(format!("redb begin_write failed: {}", e)))?;
            let removed = {
                let mut table = write_txn.open_table(ACL_RECORDS_TABLE).map_err(|e| {
                    PluginApiError::internal(format!("redb open acl_records failed: {}", e))
                })?;
                let prev = table
                    .remove((owner.as_str(), requester.as_str(), canonical_res.as_str()))
                    .map_err(|e| PluginApiError::internal(format!("redb remove failed: {}", e)))?;
                prev.map(|g| {
                    let v: AclRecordValue = serde_json::from_slice(g.value()).map_err(|e| {
                        PluginApiError::internal(format!("Corrupt acl record: {}", e))
                    })?;
                    Ok::<_, PluginApiError>(AclRecord::from_parts(
                        &owner,
                        &requester,
                        &canonical_res,
                        v,
                    ))
                })
                .transpose()?
            };
            write_txn
                .commit()
                .map_err(|e| PluginApiError::internal(format!("redb commit failed: {}", e)))?;
            Ok(removed)
        })
        .await
    }

    /// Fetch `(incoming, outgoing)` ACL records for `caller_namedomain` in a single transaction.
    pub async fn list_acl(
        &self,
        caller_namedomain: &str,
    ) -> Result<(Vec<AclRecord>, Vec<AclRecord>), PluginApiError> {
        let db = self.db.clone();
        let caller = caller_namedomain.to_string();
        self.run_blocking(move || {
            let read_txn = db
                .begin_read()
                .map_err(|e| PluginApiError::internal(format!("redb begin_read failed: {}", e)))?;
            let table = read_txn.open_table(ACL_RECORDS_TABLE).map_err(|e| {
                PluginApiError::internal(format!("redb open acl_records failed: {}", e))
            })?;
            let mut incoming = Vec::new();
            let mut outgoing = Vec::new();
            for item in table
                .iter()
                .map_err(|e| PluginApiError::internal(format!("redb iter failed: {}", e)))?
            {
                let (k_guard, v_guard) = item
                    .map_err(|e| PluginApiError::internal(format!("redb entry failed: {}", e)))?;
                let (owner, requester, resource) = k_guard.value();
                if owner != caller && requester != caller {
                    continue;
                }
                let val: AclRecordValue = serde_json::from_slice(v_guard.value())
                    .map_err(|e| PluginApiError::internal(format!("Corrupt acl record: {}", e)))?;
                let rec = AclRecord::from_parts(owner, requester, resource, val);
                if owner == caller {
                    incoming.push(rec.clone());
                }
                if requester == caller {
                    outgoing.push(rec);
                }
            }
            Ok((incoming, outgoing))
        })
        .await
    }

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
            apply_delete_quota(&mut mem.quotas, owner, len);
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
        for k in to_delete {
            if let Some((_, len)) = mem.entries.remove(&k) {
                apply_delete_quota(&mut mem.quotas, owner, len);
                removed_keys.push(k.2);
            }
        }
        let count = removed_keys.len();
        let mutation = if count > 0 {
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

    fn clone_handles(&self) -> Self {
        Self {
            db: self.db.clone(),
            persist_quotas: self.persist_quotas.clone(),
            memory: self.memory.clone(),
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugin_types::MAX_VALUE_BYTES;
    use serde_json::json;
    use std::fs;
    use std::sync::atomic::{AtomicU64, Ordering};

    static STORE_TEST_ID: AtomicU64 = AtomicU64::new(1);

    fn temp_db_path(tag: &str) -> PathBuf {
        let id = STORE_TEST_ID.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "dambo_store_test_{}_{}_{}/plugins.redb",
            tag,
            std::process::id(),
            id
        ))
    }

    fn cleanup_db(path: &std::path::Path) {
        if let Some(dir) = path.parent() {
            let _ = fs::remove_dir_all(dir);
        }
    }

    #[tokio::test]
    async fn test_persist_and_memory_crud_mset_and_redb_reload() {
        let db_path = temp_db_path("crud");
        cleanup_db(&db_path);
        let empty_defs = BTreeMap::new();

        {
            let store = PluginStore::open(db_path.clone()).unwrap();

            // Persist write (auto-inferred) + explicit enum define + cross-restart reload
            let (entry, mut_res) = store
                .put_entry(
                    "game24@1",
                    "game24@1",
                    StoreKind::Persist,
                    "scores",
                    "uid_42",
                    WriteEntryInput {
                        data_type: None,
                        value: json!(120),
                        meta: None,
                    },
                    &empty_defs,
                )
                .await
                .unwrap();
            assert_eq!(entry.data_type, DataType::Number);
            assert_eq!(mut_res.op, MutationOp::Set);
            assert_eq!(
                mut_res.inline_values.as_ref().unwrap().get("uid_42"),
                Some(&json!(120))
            );

            // Re-writing same key with wrong type fails
            let err = store
                .put_entry(
                    "game24@1",
                    "game24@1",
                    StoreKind::Persist,
                    "scores",
                    "uid_42",
                    WriteEntryInput {
                        data_type: None,
                        value: json!("not-a-number"),
                        meta: None,
                    },
                    &empty_defs,
                )
                .await
                .unwrap_err();
            assert_eq!(err.code, "TYPE_MISMATCH");

            // Memory store CRUD + mset rollback on invalid entry
            let mut batch = BTreeMap::new();
            batch.insert(
                "k1".to_string(),
                WriteEntryInput {
                    data_type: None,
                    value: json!(true),
                    meta: None,
                },
            );
            batch.insert(
                "k2".to_string(),
                WriteEntryInput {
                    data_type: None,
                    value: json!(null), // invalid top-level null triggers atomic rollback
                    meta: None,
                },
            );
            let mset_err = store
                .mset_entries(
                    "game24@1",
                    "game24@1",
                    StoreKind::Memory,
                    "round",
                    batch,
                    &empty_defs,
                )
                .await
                .unwrap_err();
            assert_eq!(mset_err.code, "INVALID_DATA_TYPE");
            assert!(
                store
                    .list_entries(
                        "game24@1",
                        "game24@1",
                        StoreKind::Memory,
                        "round",
                        None,
                        None,
                        None,
                        &empty_defs
                    )
                    .await
                    .unwrap()
                    .is_empty()
            );

            // Valid memory mset + list(keys/limit) + clear_namespace
            let mut ok_batch = BTreeMap::new();
            ok_batch.insert(
                "a_1".to_string(),
                WriteEntryInput {
                    data_type: None,
                    value: json!("one"),
                    meta: None,
                },
            );
            ok_batch.insert(
                "a_2".to_string(),
                WriteEntryInput {
                    data_type: None,
                    value: json!("two"),
                    meta: None,
                },
            );
            store
                .mset_entries(
                    "game24@1",
                    "game24@1",
                    StoreKind::Memory,
                    "round",
                    ok_batch,
                    &empty_defs,
                )
                .await
                .unwrap();

            let filtered = store
                .list_entries(
                    "game24@1",
                    "game24@1",
                    StoreKind::Memory,
                    "round",
                    Some("a_"),
                    Some(&["a_2".to_string()]),
                    Some(1),
                    &empty_defs,
                )
                .await
                .unwrap();
            assert_eq!(filtered.len(), 1);
            assert_eq!(filtered["a_2"].value, json!("two"));
        }

        // Reopen DB: persist survives, memory cleared, O(1) quota restored on reload
        let reopened = PluginStore::open(db_path.clone()).unwrap();
        let persisted = reopened
            .get_entry(
                "game24@1",
                "game24@1",
                StoreKind::Persist,
                "scores",
                "uid_42",
                &empty_defs,
            )
            .await
            .unwrap();
        assert_eq!(persisted.value, json!(120));
        assert_eq!(reopened.quota_stats("game24@1", StoreKind::Persist).keys, 1);
        assert_eq!(reopened.quota_stats("game24@1", StoreKind::Memory).keys, 0);

        let (deleted, mut_del) = reopened
            .delete_entry(
                "game24@1",
                "game24@1",
                StoreKind::Persist,
                "scores",
                "uid_42",
            )
            .await
            .unwrap();
        assert!(deleted);
        assert_eq!(mut_del.unwrap().op, MutationOp::Delete);
        assert_eq!(reopened.quota_stats("game24@1", StoreKind::Persist).keys, 0);

        cleanup_db(&db_path);
    }

    #[tokio::test]
    async fn test_defaults_merge_stale_fallback_and_config_write_protection() {
        let db_path = temp_db_path("config");
        cleanup_db(&db_path);
        let store = PluginStore::open(db_path.clone()).unwrap();

        let mut defs_v1 = BTreeMap::new();
        defs_v1.insert(
            "timeout".to_string(),
            json!({
                "type": "number",
                "value": 300,
                "meta": { "min": 10, "max": 3600, "format": "integer" }
            }),
        );
        defs_v1.insert("banner".to_string(), json!("Welcome"));

        // Override timeout validly
        store
            .put_entry(
                "game24@1",
                "game24@1",
                StoreKind::Persist,
                CONFIG_NAMESPACE,
                "timeout",
                WriteEntryInput {
                    data_type: None,
                    value: json!(600),
                    meta: None,
                },
                &defs_v1,
            )
            .await
            .unwrap();

        // Attempting to change schema of a defaults-locked key returns SCHEMA_LOCKED even for owner
        let lock_err = store
            .put_entry(
                "game24@1",
                "game24@1",
                StoreKind::Persist,
                CONFIG_NAMESPACE,
                "timeout",
                WriteEntryInput {
                    data_type: Some(DataType::String),
                    value: json!("600"),
                    meta: None,
                },
                &defs_v1,
            )
            .await
            .unwrap_err();
        assert_eq!(lock_err.code, "SCHEMA_LOCKED");

        // Tightening defaults.timeout max to 500 causes old override (600) to fall back to default (200)
        let mut defs_v2 = defs_v1.clone();
        defs_v2.insert(
            "timeout".to_string(),
            json!({
                "type": "number",
                "value": 200,
                "meta": { "min": 10, "max": 500, "format": "integer" }
            }),
        );
        let merged = store
            .get_merged_config("game24@1", "game24@1", &defs_v2)
            .await
            .unwrap();
        assert_eq!(merged["timeout"].value, json!(200));
        assert!(!merged["timeout"].is_overridden);
        assert_eq!(merged["banner"].value, json!("Welcome"));

        // External plugin with `persist:*` `write` STILL cannot write `persist:config` (max `read`)
        store
            .set_acl_grant("game24@1", "other@1", "persist:*", AclLevel::Write, None)
            .await
            .unwrap();

        let ext_cfg = store
            .get_merged_config("other@1", "game24@1", &defs_v2)
            .await
            .unwrap();
        assert_eq!(ext_cfg["banner"].value, json!("Welcome"));

        let ext_write_err = store
            .put_entry(
                "other@1",
                "game24@1",
                StoreKind::Persist,
                CONFIG_NAMESPACE,
                "banner",
                WriteEntryInput {
                    data_type: None,
                    value: json!("Hijacked"),
                    meta: None,
                },
                &defs_v2,
            )
            .await
            .unwrap_err();
        assert_eq!(ext_write_err.code, "ACL_DENIED");

        let check_cfg = store
            .check_effective_acl("other@1", "game24@1", "persist:config")
            .await
            .unwrap();
        assert_eq!(check_cfg.effective_level, "read");

        cleanup_db(&db_path);
    }

    #[tokio::test]
    async fn test_acl_single_table_upgrade_cooldown_wildcard_and_topics() {
        let db_path = temp_db_path("acl");
        cleanup_db(&db_path);
        let store = PluginStore::open(db_path.clone()).unwrap();
        let empty_defs = BTreeMap::new();

        // 1. Grant wildcard `persist:*` = Read
        store
            .set_acl_grant("points@1", "board@1", "persist:*", AclLevel::Read, None)
            .await
            .unwrap();
        assert_eq!(
            store
                .check_effective_acl("board@1", "points@1", "persist:scores")
                .await
                .unwrap()
                .effective_level,
            "read"
        );

        // 2. Request upgrade on `persist:scores` -> Write: MUST keep inherited Read active while Pending
        let req = store
            .request_acl_at(
                "board@1",
                "points@1",
                "persist:scores",
                AclLevel::Write,
                Some("记录连胜".to_string()),
                10_000,
            )
            .await
            .unwrap();
        assert_eq!(req.status, AclStatus::Pending);
        assert_eq!(req.requested_level, Some(AclLevel::Write));
        assert_eq!(
            store
                .check_effective_acl("board@1", "points@1", "persist:scores")
                .await
                .unwrap()
                .effective_level,
            "read"
        );

        // 3. Approve -> Read becomes Write; then deny (`level: None`) -> exact override blocks wildcard
        store
            .set_acl_grant(
                "points@1",
                "board@1",
                "persist:scores",
                AclLevel::Write,
                Some("OK".to_string()),
            )
            .await
            .unwrap();
        assert_eq!(
            store
                .check_effective_acl("board@1", "points@1", "persist:scores")
                .await
                .unwrap()
                .effective_level,
            "write"
        );

        let deny_err_req = store
            .request_acl_at(
                "board@1",
                "points@1",
                "memory:state",
                AclLevel::Read,
                None,
                10_000,
            )
            .await
            .unwrap();
        assert_eq!(deny_err_req.status, AclStatus::Pending);

        store
            .set_acl_grant(
                "points@1",
                "board@1",
                "memory:state",
                AclLevel::None,
                Some("No".to_string()),
            )
            .await
            .unwrap();

        let ts_now = Utc::now().timestamp_millis();
        let cooldown_err = store
            .request_acl_at(
                "board@1",
                "points@1",
                "memory:state",
                AclLevel::Read,
                None,
                ts_now + 1_000,
            )
            .await
            .unwrap_err();
        assert_eq!(cooldown_err.code, "TOO_MANY_REQUESTS");

        // After 5s cooldown passes, re-request succeeds cleanly
        let re_req = store
            .request_acl_at(
                "board@1",
                "points@1",
                "memory:state",
                AclLevel::Read,
                Some("重试".to_string()),
                ts_now + ACL_DENY_COOLDOWN_MS + 100,
            )
            .await
            .unwrap();
        assert_eq!(re_req.status, AclStatus::Pending);

        // Explicit deny on persist:scores blocks wildcard even across re-requests
        store
            .set_acl_grant(
                "points@1",
                "board@1",
                "persist:scores",
                AclLevel::None,
                None,
            )
            .await
            .unwrap();
        assert_eq!(
            store
                .check_effective_acl("board@1", "points@1", "persist:scores")
                .await
                .unwrap()
                .effective_level,
            "none"
        );

        // 4. `list_acl` returns incoming + outgoing in one call
        let (inc, out) = store.list_acl("points@1").await.unwrap();
        assert!(!inc.is_empty());
        assert!(out.is_empty());

        // 5. External schema modification locked even with `write`
        store
            .set_acl_grant(
                "points@1",
                "board@1",
                "persist:scores",
                AclLevel::Write,
                None,
            )
            .await
            .unwrap();
        store
            .put_entry(
                "points@1",
                "points@1",
                StoreKind::Persist,
                "scores",
                "k",
                WriteEntryInput {
                    data_type: None,
                    value: json!(1),
                    meta: None,
                },
                &empty_defs,
            )
            .await
            .unwrap();
        let schema_err = store
            .put_entry(
                "board@1",
                "points@1",
                StoreKind::Persist,
                "scores",
                "k",
                WriteEntryInput {
                    data_type: Some(DataType::String),
                    value: json!("1"),
                    meta: None,
                },
                &empty_defs,
            )
            .await
            .unwrap_err();
        assert_eq!(schema_err.code, "SCHEMA_LOCKED");

        // 6. Persisted topics: public skips ACL, demotion reports true
        let (t1, demoted1) = store
            .configure_topic("points@1", "round", true, Some("Public round".to_string()))
            .await
            .unwrap();
        assert!(t1.public);
        assert!(!demoted1);
        assert_eq!(
            store
                .check_effective_acl("board@1", "points@1", "topic:round")
                .await
                .unwrap()
                .effective_level,
            "subscribe"
        );

        let (t2, demoted2) = store
            .ensure_topic_for_publish("points@1", "round", Some(false), None)
            .await
            .unwrap();
        assert!(!t2.public);
        assert!(demoted2);
        assert_eq!(
            store
                .check_effective_acl("board@1", "points@1", "topic:round")
                .await
                .unwrap()
                .effective_level,
            "none"
        );

        cleanup_db(&db_path);
    }

    #[tokio::test]
    async fn test_quota_limits_and_single_value_cap() {
        let db_path = temp_db_path("quota");
        cleanup_db(&db_path);
        let store = PluginStore::open(db_path.clone()).unwrap();
        let empty_defs = BTreeMap::new();

        // Single value exceeding 256 KB fails cleanly
        let huge = "x".repeat(MAX_VALUE_BYTES);
        let err = store
            .put_entry(
                "p@0",
                "p@0",
                StoreKind::Memory,
                "big",
                "k",
                WriteEntryInput {
                    data_type: None,
                    value: json!(huge),
                    meta: None,
                },
                &empty_defs,
            )
            .await
            .unwrap_err();
        assert_eq!(err.code, "QUOTA_EXCEEDED");

        // Near-max chunks accumulate until total > 16 MB fails with QUOTA_EXCEEDED
        // (81 * ~200 KB < 16 MiB < 82 * ~200 KB)
        let chunk = "a".repeat(200 * 1024);
        for i in 0..81 {
            store
                .put_entry(
                    "p@0",
                    "p@0",
                    StoreKind::Memory,
                    "big",
                    &format!("k_{}", i),
                    WriteEntryInput {
                        data_type: None,
                        value: json!(chunk),
                        meta: None,
                    },
                    &empty_defs,
                )
                .await
                .unwrap();
        }
        let overflow_err = store
            .put_entry(
                "p@0",
                "p@0",
                StoreKind::Memory,
                "big",
                "k_overflow",
                WriteEntryInput {
                    data_type: None,
                    value: json!(chunk),
                    meta: None,
                },
                &empty_defs,
            )
            .await
            .unwrap_err();
        assert_eq!(overflow_err.code, "QUOTA_EXCEEDED");

        // Clearing namespace reclaims quota immediately
        let (cleared, mut_res) = store
            .clear_namespace("p@0", "p@0", StoreKind::Memory, "big")
            .await
            .unwrap();
        assert_eq!(cleared, 81);
        assert_eq!(mut_res.unwrap().op, MutationOp::Clear);
        assert_eq!(store.quota_stats("p@0", StoreKind::Memory).keys, 0);
        assert_eq!(store.quota_stats("p@0", StoreKind::Memory).bytes, 0);

        cleanup_db(&db_path);
    }

    #[test]
    fn test_builtin_plugin_manifests_and_defaults_are_valid() {
        let html_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../static/html");
        let plugins = crate::plugin::scan_plugins(&html_dir);
        let names: BTreeSet<&str> = plugins.iter().map(|p| p.name.as_str()).collect();
        for required in ["default", "list", "sc-ticker", "game24"] {
            assert!(
                names.contains(required),
                "Expected built-in plugin '{}' in {:?}, got {:?}",
                required,
                html_dir,
                names
            );
        }

        for p in &plugins {
            assert!(matches!(
                p.name.as_str(),
                "default" | "list" | "sc-ticker" | "game24"
            ));
            assert!(
                !p.plugin_id.is_empty(),
                "Plugin '{}' should declare plugin_id",
                p.name
            );
            assert_ne!(
                p.version, "0",
                "Built-in plugin '{}' should declare non-zero version",
                p.name
            );
            assert!(
                p.title.is_some(),
                "Built-in plugin '{}' should declare name/title",
                p.name
            );
            assert!(
                !p.defaults.is_empty(),
                "Built-in plugin '{}' should expose v2 defaults",
                p.name
            );
            for (k, raw_v) in &p.defaults {
                let parsed = parse_default_entry(raw_v).unwrap_or_else(|e| {
                    panic!(
                        "Built-in plugin '{}' default '{}' failed validation: {}",
                        p.name, k, e
                    )
                });
                assert!(is_valid_key(k), "Invalid key '{}' in '{}'", k, p.name);
                assert_ne!(parsed.value, serde_json::Value::Null);
            }
        }
    }
}
