mod acl;
mod kv;
mod topics;

#[cfg(test)]
mod tests;

pub use acl::{AclCheckResult, AclLevel, AclRecord, AclStatus};
pub use kv::{
    ConfigEntryView, DataMutationResult, MutationOp, QuotaStats, StoreKind, parse_defaults_map,
};
pub use topics::TopicMeta;

use crate::config::SecurityMode;
use crate::plugin_types::{PluginApiError, TypedEntry};
use anyhow::Result;
use redb::{Database, ReadableTable, TableDefinition};
use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, RwLock};

/// Maximum serialized byte size of inline `values` attached to a `data.changed` SSE event (`16 KB`).
pub const INLINE_VALUES_MAX_BYTES: usize = 16 * 1024;

/// Cooldown window (ms) before a denied ACL request can be re-submitted.
pub const ACL_DENY_COOLDOWN_MS: i64 = 5_000;

/// Reserved namespace under `persist` merged with `plugin.json` `defaults` at read time.
pub const CONFIG_NAMESPACE: &str = "config";

pub(super) const KV_DATA_TABLE: TableDefinition<(&str, &str, &str), &[u8]> =
    TableDefinition::new("kv_data");
pub(super) const ACL_RECORDS_TABLE: TableDefinition<(&str, &str, &str), &[u8]> =
    TableDefinition::new("acl_records");
pub(super) const TOPIC_META_TABLE: TableDefinition<(&str, &str), &[u8]> =
    TableDefinition::new("topic_meta");

#[derive(Default)]
pub(super) struct MemoryState {
    pub(super) entries: BTreeMap<(String, String, String), (TypedEntry, usize)>,
    pub(super) quotas: HashMap<String, QuotaStats>,
}

/// Unified storage and ACL engine managing persistent `redb` tables and volatile memory KV state.
pub struct PluginStore {
    pub(super) db: Arc<Database>,
    pub(super) persist_quotas: Arc<Mutex<HashMap<String, QuotaStats>>>,
    pub(super) memory: Arc<RwLock<MemoryState>>,
    pub(super) security_mode: SecurityMode,
}

pub(super) fn exclusive_prefix_end(prefix: &str) -> String {
    let mut end = String::with_capacity(prefix.len() + 1);
    end.push_str(prefix);
    end.push('\0');
    end
}

impl PluginStore {
    /// Open or create a `redb` database at `db_path` in `Strict` mode (fail-closed default for standalone callers).
    pub fn open(db_path: PathBuf) -> Result<Self> {
        Self::open_with_mode(db_path, SecurityMode::Strict)
    }

    /// Open or create a `redb` database at `db_path` with an explicit [`SecurityMode`].
    pub fn open_with_mode(db_path: PathBuf, security_mode: SecurityMode) -> Result<Self> {
        let db = crate::db::open_database(&db_path)?;
        Self::from_database_with_mode(db, security_mode)
    }

    /// Wrap an existing shared `redb::Database` in `Strict` mode (fail-closed default for standalone callers).
    pub fn from_database(db: Arc<Database>) -> Result<Self> {
        Self::from_database_with_mode(db, SecurityMode::Strict)
    }

    /// Wrap an existing shared `redb::Database` with an explicit [`SecurityMode`],
    /// initialize plugin tables, and build initial O(1) quota counters.
    pub fn from_database_with_mode(db: Arc<Database>, security_mode: SecurityMode) -> Result<Self> {
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
            security_mode,
        })
    }

    /// Active security mode governing cross-plugin Read/Subscribe auto-grant behavior.
    pub fn security_mode(&self) -> SecurityMode {
        self.security_mode
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

    pub(super) async fn run_blocking<F, T>(&self, f: F) -> Result<T, PluginApiError>
    where
        F: FnOnce() -> Result<T, PluginApiError> + Send + 'static,
        T: Send + 'static,
    {
        tokio::task::spawn_blocking(f)
            .await
            .map_err(|e| PluginApiError::internal(format!("Blocking storage task failed: {}", e)))?
    }

    pub(super) fn clone_handles(&self) -> Self {
        Self {
            db: self.db.clone(),
            persist_quotas: self.persist_quotas.clone(),
            memory: self.memory.clone(),
            security_mode: self.security_mode,
        }
    }
}
