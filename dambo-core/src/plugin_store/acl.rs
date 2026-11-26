use super::topics::TopicMetaValue;
use super::{
    ACL_DENY_COOLDOWN_MS, ACL_RECORDS_TABLE, CONFIG_NAMESPACE, PluginStore, StoreKind,
    TOPIC_META_TABLE,
};
use crate::config::SecurityMode;
use crate::plugin_types::{
    AclResourceKind, ParsedAclResource, PluginApiError, is_false, is_valid_namespace,
    parse_acl_resource,
};
use chrono::Utc;
use redb::ReadableTable;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

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
#[serde(rename_all = "snake_case")]
pub enum AclStatus {
    Pending,
    Granted,
    #[serde(alias = "autogranted", alias = "AutoGranted")]
    AutoGranted,
    Denied,
}

impl AclStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Granted => "granted",
            Self::AutoGranted => "auto_granted",
            Self::Denied => "denied",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(super) struct AclRecordValue {
    pub(super) level: AclLevel,
    pub(super) status: AclStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) requested_level: Option<AclLevel>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) note: Option<String>,
    /// Tracks whether the exact record had an explicit denial before a subsequent re-request,
    /// preventing a pending re-request from temporarily inheriting wildcard access.
    #[serde(default, skip_serializing_if = "is_false")]
    pub(super) prior_denied: bool,
    /// Tracks whether the current granted level originated from `Open`-mode auto-granting,
    /// ensuring `Strict` mode continues treating it as unauthorized even if upgraded to `Pending`.
    #[serde(default, skip_serializing_if = "is_false")]
    pub(super) auto_granted: bool,
    pub(super) updated_at: i64,
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
    pub(super) fn from_parts(
        owner: &str,
        requester: &str,
        resource: &str,
        v: AclRecordValue,
    ) -> Self {
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

pub(super) fn effective_record_level(rec: &AclRecordValue, mode: SecurityMode) -> AclLevel {
    if mode.is_strict() && (rec.status == AclStatus::AutoGranted || rec.auto_granted) {
        AclLevel::None
    } else {
        rec.level
    }
}

fn read_acl_record<R: ReadableTable<(&'static str, &'static str, &'static str), &'static [u8]>>(
    acl_table: &R,
    owner: &str,
    caller: &str,
    resource: &str,
) -> Result<Option<AclRecordValue>, PluginApiError> {
    let Some(guard) = acl_table
        .get((owner, caller, resource))
        .map_err(|e| PluginApiError::internal(format!("redb acl_records read failed: {}", e)))?
    else {
        return Ok(None);
    };
    let rec: AclRecordValue = serde_json::from_slice(guard.value()).map_err(|e| {
        PluginApiError::internal(format!("Corrupt acl_records JSON in redb: {}", e))
    })?;
    Ok(Some(rec))
}

fn is_topic_public<T: ReadableTable<(&'static str, &'static str), &'static [u8]>>(
    topic_table: &T,
    owner_namedomain: &str,
    topic_name: &str,
) -> Result<bool, PluginApiError> {
    let Some(guard) = topic_table
        .get((owner_namedomain, topic_name))
        .map_err(|e| PluginApiError::internal(format!("redb topic_meta read failed: {}", e)))?
    else {
        return Ok(false);
    };
    let meta: TopicMetaValue = serde_json::from_slice(guard.value())
        .map_err(|e| PluginApiError::internal(format!("Corrupt topic_meta JSON in redb: {}", e)))?;
    Ok(meta.public)
}

pub(super) fn check_acl_in_read_txn<
    R: ReadableTable<(&'static str, &'static str, &'static str), &'static [u8]>,
    T: ReadableTable<(&'static str, &'static str), &'static [u8]>,
>(
    acl_table: &R,
    topic_table: &T,
    caller_namedomain: &str,
    owner_namedomain: &str,
    parsed: &ParsedAclResource,
    security_mode: SecurityMode,
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
        && is_topic_public(topic_table, owner_namedomain, parsed.scope.as_str())?
    {
        return Ok(AclLevel::Subscribe);
    }

    let exact_key = parsed.canonical();
    let wild_key = parsed.wildcard();

    let mut blocked_by_exact_deny = false;
    if let Some(rec) = read_acl_record(acl_table, owner_namedomain, caller_namedomain, &exact_key)?
    {
        let eff = effective_record_level(&rec, security_mode);
        if eff != AclLevel::None {
            return Ok(if parsed.is_persist_config() {
                AclLevel::Read
            } else {
                eff
            });
        }
        if rec.status == AclStatus::Denied || rec.prior_denied {
            blocked_by_exact_deny = true;
        }
    }

    if !blocked_by_exact_deny
        && !parsed.is_wildcard()
        && let Some(rec) =
            read_acl_record(acl_table, owner_namedomain, caller_namedomain, &wild_key)?
    {
        let eff = effective_record_level(&rec, security_mode);
        if eff != AclLevel::None {
            return Ok(if parsed.is_persist_config() {
                AclLevel::Read
            } else {
                eff
            });
        }
    }

    Ok(AclLevel::None)
}

/// Returns `true` if `caller` should see `parsed` namespace when listing in `Open` mode:
/// visible unless explicitly denied (by exact rule or un-overridden wildcard deny).
pub(super) fn is_namespace_visible_in_open_mode<
    R: ReadableTable<(&'static str, &'static str, &'static str), &'static [u8]>,
>(
    acl_table: &R,
    caller_namedomain: &str,
    owner_namedomain: &str,
    parsed: &ParsedAclResource,
) -> Result<bool, PluginApiError> {
    if caller_namedomain == owner_namedomain {
        return Ok(true);
    }
    let exact_key = parsed.canonical();
    if let Some(rec) = read_acl_record(acl_table, owner_namedomain, caller_namedomain, &exact_key)?
    {
        if rec.level != AclLevel::None {
            return Ok(true);
        }
        if rec.status == AclStatus::Denied || rec.prior_denied {
            return Ok(false);
        }
        return Ok(true);
    }

    let wild_key = parsed.wildcard();
    if let Some(wild) = read_acl_record(acl_table, owner_namedomain, caller_namedomain, &wild_key)?
        && wild.level == AclLevel::None
        && (wild.status == AclStatus::Denied || wild.prior_denied)
    {
        return Ok(false);
    }

    Ok(true)
}

fn acl_denied_err(owner: &str, resource: &str, required: AclLevel) -> PluginApiError {
    PluginApiError::acl_denied(format!(
        "Permission '{}' required on '{}' for target '{}'",
        required.as_str(),
        resource,
        owner
    ))
    .with_details(serde_json::json!({
        "target_namedomain": owner,
        "resource": resource,
        "required_level": required.as_str()
    }))
}

fn build_auto_granted_value(level: AclLevel, now_ms: i64) -> AclRecordValue {
    AclRecordValue {
        level,
        status: AclStatus::AutoGranted,
        requested_level: None,
        reason: None,
        note: Some("Open 模式自动放行".to_string()),
        prior_denied: false,
        auto_granted: true,
        updated_at: now_ms,
    }
}

impl PluginStore {
    /// Enforce ACL access for `store:ns`, returning `Some(AclRecord)` when `Open` mode
    /// auto-grants a previously unrecorded cross-plugin `Read` request.
    ///
    /// Cross-plugin `Write` is NEVER auto-granted in either mode.
    pub(super) fn require_data_access_or_auto_grant_sync(
        &self,
        caller_namedomain: &str,
        owner_namedomain: &str,
        store: StoreKind,
        ns: &str,
        required: AclLevel,
    ) -> Result<Option<AclRecord>, PluginApiError> {
        if !is_valid_namespace(ns) {
            return Err(PluginApiError::invalid_argument(format!(
                "Invalid namespace '{}': must match [A-Za-z0-9_.-] (1..=128 bytes)",
                ns
            )));
        }
        if caller_namedomain == owner_namedomain {
            return Ok(None);
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

        let effective = {
            let read_txn = self
                .db
                .begin_read()
                .map_err(|e| PluginApiError::internal(format!("redb begin_read failed: {}", e)))?;
            let acl_table = read_txn.open_table(ACL_RECORDS_TABLE).map_err(|e| {
                PluginApiError::internal(format!("redb open acl_records failed: {}", e))
            })?;
            let topic_table = read_txn.open_table(TOPIC_META_TABLE).map_err(|e| {
                PluginApiError::internal(format!("redb open topic_meta failed: {}", e))
            })?;
            check_acl_in_read_txn(
                &acl_table,
                &topic_table,
                caller_namedomain,
                owner_namedomain,
                &parsed,
                self.security_mode,
            )?
        };

        if effective.satisfies(required, &parsed.kind) {
            return Ok(None);
        }

        // Write is never auto-granted, and Strict mode never auto-grants.
        if self.security_mode.is_strict() || required != AclLevel::Read {
            return Err(acl_denied_err(owner_namedomain, &resource_str, required));
        }

        // Open mode + Read: inspect exact & wildcard under a write txn and auto-grant only if no record blocks it.
        let exact_key = parsed.canonical();
        let wild_key = parsed.wildcard();
        let now_ms = Utc::now().timestamp_millis();

        let write_txn = self
            .db
            .begin_write()
            .map_err(|e| PluginApiError::internal(format!("redb begin_write failed: {}", e)))?;
        let created = {
            let mut acl_table = write_txn.open_table(ACL_RECORDS_TABLE).map_err(|e| {
                PluginApiError::internal(format!("redb open acl_records failed: {}", e))
            })?;

            if let Some(existing) =
                read_acl_record(&acl_table, owner_namedomain, caller_namedomain, &exact_key)?
            {
                if effective_record_level(&existing, self.security_mode)
                    .satisfies(AclLevel::Read, &parsed.kind)
                {
                    None
                } else {
                    // Existing Denied, prior_denied, or Pending record blocks auto-grant.
                    return Err(acl_denied_err(owner_namedomain, &resource_str, required));
                }
            } else {
                if let Some(wild) =
                    read_acl_record(&acl_table, owner_namedomain, caller_namedomain, &wild_key)?
                {
                    if wild.level == AclLevel::None
                        && (wild.status == AclStatus::Denied || wild.prior_denied)
                    {
                        return Err(acl_denied_err(owner_namedomain, &resource_str, required));
                    }
                    if effective_record_level(&wild, self.security_mode)
                        .satisfies(AclLevel::Read, &parsed.kind)
                    {
                        return Ok(None);
                    }
                }

                let val = build_auto_granted_value(AclLevel::Read, now_ms);
                let bytes = serde_json::to_vec(&val).map_err(|e| {
                    PluginApiError::internal(format!("Failed to serialize acl record: {}", e))
                })?;
                acl_table
                    .insert(
                        (owner_namedomain, caller_namedomain, exact_key.as_str()),
                        bytes.as_slice(),
                    )
                    .map_err(|e| PluginApiError::internal(format!("redb insert failed: {}", e)))?;
                Some(AclRecord::from_parts(
                    owner_namedomain,
                    caller_namedomain,
                    &exact_key,
                    val,
                ))
            }
        };

        if created.is_some() {
            write_txn
                .commit()
                .map_err(|e| PluginApiError::internal(format!("redb commit failed: {}", e)))?;
        }
        Ok(created)
    }

    /// Check effective ACL permission for `caller_namedomain` on `(owner_namedomain, resource)` (read-only).
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
        let mode = self.security_mode;
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
            let eff =
                check_acl_in_read_txn(&acl_table, &topic_table, &caller, &owner, &parsed, mode)?;
            Ok(AclCheckResult {
                is_self: false,
                effective_level: eff.as_str().to_string(),
            })
        })
        .await
    }

    /// Evaluate a batch of `(caller_namedomain, owner_namedomain, ParsedAclResource)` checks in a single
    /// `redb` read transaction (read-only, never triggers `AutoGranted`).
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
        let mode = self.security_mode;
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
                let eff =
                    check_acl_in_read_txn(&acl_table, &topic_table, caller, owner, parsed, mode)?;
                results.push(eff);
            }
            Ok(results)
        })
        .await
    }

    /// Evaluate SSE subscription ACLs (`Read` for watches, `Subscribe` for topics), auto-granting
    /// unrecorded cross-plugin `Read`/`Subscribe` requests in `Open` mode inside a single write transaction.
    pub async fn check_effective_acl_batch_for_subscriptions(
        &self,
        queries: Vec<(String, String, ParsedAclResource)>,
    ) -> Result<(Vec<AclLevel>, Vec<AclRecord>), PluginApiError> {
        if queries.is_empty() {
            return Ok((Vec::new(), Vec::new()));
        }
        if queries.iter().all(|(caller, owner, _)| caller == owner) {
            let levels = queries
                .into_iter()
                .map(|(_, _, parsed)| match parsed.kind {
                    AclResourceKind::Persist | AclResourceKind::Memory => AclLevel::Write,
                    AclResourceKind::Topic => AclLevel::Subscribe,
                })
                .collect();
            return Ok((levels, Vec::new()));
        }

        let db = self.db.clone();
        let mode = self.security_mode;
        self.run_blocking(move || {
            let mut levels = Vec::with_capacity(queries.len());
            let mut pending_auto_indices = Vec::new();
            {
                let read_txn = db.begin_read().map_err(|e| {
                    PluginApiError::internal(format!("redb begin_read failed: {}", e))
                })?;
                let acl_table = read_txn.open_table(ACL_RECORDS_TABLE).map_err(|e| {
                    PluginApiError::internal(format!("redb open acl_records failed: {}", e))
                })?;
                let topic_table = read_txn.open_table(TOPIC_META_TABLE).map_err(|e| {
                    PluginApiError::internal(format!("redb open topic_meta failed: {}", e))
                })?;

                for (idx, (caller, owner, parsed)) in queries.iter().enumerate() {
                    let eff = check_acl_in_read_txn(
                        &acl_table,
                        &topic_table,
                        caller,
                        owner,
                        parsed,
                        mode,
                    )?;
                    let required = match parsed.kind {
                        AclResourceKind::Persist | AclResourceKind::Memory => AclLevel::Read,
                        AclResourceKind::Topic => AclLevel::Subscribe,
                    };
                    if mode.is_open() && caller != owner && !eff.satisfies(required, &parsed.kind) {
                        pending_auto_indices.push(idx);
                    }
                    levels.push(eff);
                }
            }

            if pending_auto_indices.is_empty() {
                return Ok((levels, Vec::new()));
            }

            let now_ms = Utc::now().timestamp_millis();
            let write_txn = db
                .begin_write()
                .map_err(|e| PluginApiError::internal(format!("redb begin_write failed: {}", e)))?;
            let mut newly_granted = Vec::new();
            let mut inserted_keys: HashSet<(String, String, String)> = HashSet::new();

            {
                let mut acl_table = write_txn.open_table(ACL_RECORDS_TABLE).map_err(|e| {
                    PluginApiError::internal(format!("redb open acl_records failed: {}", e))
                })?;
                let topic_table = write_txn.open_table(TOPIC_META_TABLE).map_err(|e| {
                    PluginApiError::internal(format!("redb open topic_meta failed: {}", e))
                })?;

                for idx in pending_auto_indices {
                    let (caller, owner, parsed) = &queries[idx];
                    let required = match parsed.kind {
                        AclResourceKind::Persist | AclResourceKind::Memory => AclLevel::Read,
                        AclResourceKind::Topic => AclLevel::Subscribe,
                    };

                    // Re-verify inside write transaction
                    let current = check_acl_in_read_txn(
                        &acl_table,
                        &topic_table,
                        caller,
                        owner,
                        parsed,
                        mode,
                    )?;
                    if current.satisfies(required, &parsed.kind) {
                        levels[idx] = current;
                        continue;
                    }

                    let exact_key = parsed.canonical();
                    let wild_key = parsed.wildcard();

                    if read_acl_record(&acl_table, owner, caller, &exact_key)?.is_some() {
                        // Existing record (Denied / prior_denied / Pending) stays blocked.
                        continue;
                    }
                    if let Some(wild) = read_acl_record(&acl_table, owner, caller, &wild_key)?
                        && wild.level == AclLevel::None
                        && (wild.status == AclStatus::Denied || wild.prior_denied)
                    {
                        continue;
                    }

                    let val = build_auto_granted_value(required, now_ms);
                    let dedup_key = (owner.clone(), caller.clone(), exact_key.clone());
                    if inserted_keys.insert(dedup_key) {
                        let bytes = serde_json::to_vec(&val).map_err(|e| {
                            PluginApiError::internal(format!(
                                "Failed to serialize acl record: {}",
                                e
                            ))
                        })?;
                        acl_table
                            .insert(
                                (owner.as_str(), caller.as_str(), exact_key.as_str()),
                                bytes.as_slice(),
                            )
                            .map_err(|e| {
                                PluginApiError::internal(format!("redb insert failed: {}", e))
                            })?;
                        newly_granted.push(AclRecord::from_parts(owner, caller, &exact_key, val));
                    }
                    levels[idx] = required;
                }
            }

            if !newly_granted.is_empty() {
                write_txn
                    .commit()
                    .map_err(|e| PluginApiError::internal(format!("redb commit failed: {}", e)))?;
            }

            Ok((levels, newly_granted))
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
                let existing = read_acl_record(&table, &owner, &caller, &canonical_res)?;

                let (preserved_level, note, prior_denied, auto_granted) =
                    if let Some(prev) = existing {
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
                            prev.status == AclStatus::AutoGranted || prev.auto_granted,
                        )
                    } else {
                        (AclLevel::None, None, false, false)
                    };

                let val = AclRecordValue {
                    level: preserved_level,
                    status: AclStatus::Pending,
                    requested_level: Some(requested_level),
                    reason: clean_reason,
                    note,
                    prior_denied,
                    auto_granted,
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
                let existing = read_acl_record(&table, &owner, &requester, &canonical_res)?;

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
                    auto_granted: false,
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

    /// Fetch every persisted ACL record across all plugins (used by the Tauri control capsule).
    pub async fn list_all_acl(&self) -> Result<Vec<AclRecord>, PluginApiError> {
        let db = self.db.clone();
        self.run_blocking(move || {
            let read_txn = db
                .begin_read()
                .map_err(|e| PluginApiError::internal(format!("redb begin_read failed: {}", e)))?;
            let table = read_txn.open_table(ACL_RECORDS_TABLE).map_err(|e| {
                PluginApiError::internal(format!("redb open acl_records failed: {}", e))
            })?;
            let mut records = Vec::new();
            for item in table
                .iter()
                .map_err(|e| PluginApiError::internal(format!("redb iter failed: {}", e)))?
            {
                let (k_guard, v_guard) = item
                    .map_err(|e| PluginApiError::internal(format!("redb entry failed: {}", e)))?;
                let (owner, requester, resource) = k_guard.value();
                let val: AclRecordValue = serde_json::from_slice(v_guard.value())
                    .map_err(|e| PluginApiError::internal(format!("Corrupt acl record: {}", e)))?;
                records.push(AclRecord::from_parts(owner, requester, resource, val));
            }
            records.sort_by_key(|a| std::cmp::Reverse(a.updated_at));
            Ok(records)
        })
        .await
    }
}
