use super::*;
use crate::plugin_types::{
    DataType, MAX_VALUE_BYTES, WriteEntryInput, is_valid_key, parse_acl_resource,
    parse_default_entry,
};
use chrono::Utc;
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};
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

#[tokio::test]
async fn test_security_mode_open_auto_grant_and_strict_isolation() {
    let db_path = temp_db_path("security_modes");
    cleanup_db(&db_path);
    let parsed_defs = Arc::new(BTreeMap::new());
    let empty_defs = BTreeMap::new();

    {
        let open_store = PluginStore::open_with_mode(db_path.clone(), SecurityMode::Open).unwrap();

        // Owner writes persist and memory entries
        open_store
            .put_entry(
                "owner@1",
                "owner@1",
                StoreKind::Persist,
                "scores",
                "u1",
                WriteEntryInput {
                    data_type: None,
                    value: json!(100),
                    meta: None,
                },
                &empty_defs,
            )
            .await
            .unwrap();
        open_store
            .put_entry(
                "owner@1",
                "owner@1",
                StoreKind::Memory,
                "live",
                "k1",
                WriteEntryInput {
                    data_type: None,
                    value: json!("active"),
                    meta: None,
                },
                &empty_defs,
            )
            .await
            .unwrap();

        // 1. Cross-plugin Read in Open mode auto-grants and returns AutoGranted record
        let (entry, auto_rec) = open_store
            .get_entry_cached_with_acl(
                "reader@1",
                "owner@1",
                StoreKind::Persist,
                "scores",
                "u1",
                &parsed_defs,
            )
            .await
            .unwrap();
        assert_eq!(entry.value, json!(100));
        let rec = auto_rec.expect("expected AutoGranted record on first cross-plugin read");
        assert_eq!(rec.status, AclStatus::AutoGranted);
        assert_eq!(rec.level, AclLevel::Read);
        assert_eq!(rec.resource, "persist:scores");

        // Subsequent reads reuse existing AutoGranted without emitting duplicate record
        let (_, repeat_rec) = open_store
            .get_entry_cached_with_acl(
                "reader@1",
                "owner@1",
                StoreKind::Persist,
                "scores",
                "u1",
                &parsed_defs,
            )
            .await
            .unwrap();
        assert!(repeat_rec.is_none());

        // Cross-plugin Memory Read + Topic Subscribe batch also auto-grants
        let queries = vec![
            (
                "reader@1".to_string(),
                "owner@1".to_string(),
                parse_acl_resource("memory:live").unwrap(),
            ),
            (
                "reader@1".to_string(),
                "owner@1".to_string(),
                parse_acl_resource("topic:round").unwrap(),
            ),
        ];
        let (levels, new_grants) = open_store
            .check_effective_acl_batch_for_subscriptions(queries)
            .await
            .unwrap();
        assert_eq!(levels, vec![AclLevel::Read, AclLevel::Subscribe]);
        assert_eq!(new_grants.len(), 2);
        assert!(
            new_grants
                .iter()
                .all(|g| g.status == AclStatus::AutoGranted)
        );

        // 2. Cross-plugin Write in Open mode is NEVER auto-granted
        let write_err = open_store
            .put_entry(
                "reader@1",
                "owner@1",
                StoreKind::Persist,
                "scores",
                "u1",
                WriteEntryInput {
                    data_type: None,
                    value: json!(999),
                    meta: None,
                },
                &empty_defs,
            )
            .await
            .unwrap_err();
        assert_eq!(write_err.code, "ACL_DENIED");

        // 3. Explicit admin denial persists and blocks Open mode auto-grant
        open_store
            .set_acl_grant(
                "owner@1",
                "reader@1",
                "memory:live",
                AclLevel::None,
                Some("revoked".to_string()),
            )
            .await
            .unwrap();
        let blocked_read = open_store
            .get_entry_cached_with_acl(
                "reader@1",
                "owner@1",
                StoreKind::Memory,
                "live",
                "k1",
                &parsed_defs,
            )
            .await
            .unwrap_err();
        assert_eq!(blocked_read.code, "ACL_DENIED");

        // Explicit Grant works for Write and persists as Granted
        open_store
            .set_acl_grant(
                "owner@1",
                "writer@1",
                "persist:scores",
                AclLevel::Write,
                None,
            )
            .await
            .unwrap();

        // Inject a legacy v0.3.3 ACL record without `auto_granted` field to verify backward compatibility
        let write_txn = open_store.db.begin_write().unwrap();
        {
            let mut tbl = write_txn.open_table(ACL_RECORDS_TABLE).unwrap();
            tbl.insert(
                ("owner@1", "legacy@1", "persist:scores"),
                br#"{"level":"read","status":"granted","updated_at":12345}"#.as_slice(),
            )
            .unwrap();
        }
        write_txn.commit().unwrap();
    }

    // 4. Reopen same redb in Strict mode: AutoGranted becomes unauthorized, Granted + legacy survive
    let strict_store = PluginStore::open_with_mode(db_path.clone(), SecurityMode::Strict).unwrap();
    let auto_blocked = strict_store
        .get_entry(
            "reader@1",
            "owner@1",
            StoreKind::Persist,
            "scores",
            "u1",
            &empty_defs,
        )
        .await
        .unwrap_err();
    assert_eq!(auto_blocked.code, "ACL_DENIED");

    assert_eq!(
        strict_store
            .check_effective_acl("reader@1", "owner@1", "topic:round")
            .await
            .unwrap()
            .effective_level,
        "none"
    );
    assert_eq!(
        strict_store
            .check_effective_acl("legacy@1", "owner@1", "persist:scores")
            .await
            .unwrap()
            .effective_level,
        "read"
    );
    assert_eq!(
        strict_store
            .check_effective_acl("writer@1", "owner@1", "persist:scores")
            .await
            .unwrap()
            .effective_level,
        "write"
    );
    assert_eq!(
        strict_store
            .get_entry(
                "legacy@1",
                "owner@1",
                StoreKind::Persist,
                "scores",
                "u1",
                &empty_defs,
            )
            .await
            .unwrap()
            .value,
        json!(100)
    );

    cleanup_db(&db_path);
}
