use crate::db;
use crate::model::{AuthState, RoomQuickItem};
use anyhow::{Context, Result, anyhow};
use blive_proto::api::BiliApiClient;
pub use blive_proto::auth::{DANMAKU_MAX_CHARS, QrPollStatus, SendDanmakuError, SessionData};
use qrcode::QrCode;
use qrcode::render::{svg, unicode};
use redb::Database;
use std::fs;
use std::path::Path;
use std::sync::{Arc, Mutex, RwLock};
use tokio::sync::watch;

/// Render a URL string into `(svg_xml, terminal_unicode_qr)`.
pub fn render_qr(url: &str) -> Result<(String, String)> {
    let code = QrCode::new(url.as_bytes()).context("Failed to encode QR code")?;
    let svg_xml = code
        .render::<svg::Color>()
        .min_dimensions(180, 180)
        .dark_color(svg::Color("#000000"))
        .light_color(svg::Color("#ffffff"))
        .build();
    let terminal_qr = code
        .render::<unicode::Dense1x2>()
        .dark_color(unicode::Dense1x2::Light)
        .light_color(unicode::Dense1x2::Dark)
        .quiet_zone(true)
        .build();
    Ok((svg_xml, terminal_qr))
}

fn load_legacy_session_file(path: &Path) -> Result<Option<SessionData>> {
    if !path.exists() {
        return Ok(None);
    }
    let raw = fs::read_to_string(path)
        .with_context(|| format!("Failed to read legacy session file {:?}", path))?;
    let parsed: SessionData = serde_json::from_str(&raw)
        .with_context(|| format!("Failed to parse legacy session JSON at {:?}", path))?;
    Ok(Some(parsed))
}

fn load_session_from_db(db: &Database) -> Result<Option<SessionData>> {
    let Some(bytes) = db::read_meta(db, db::KEY_AUTH_SESSION)? else {
        return Ok(None);
    };
    let parsed: SessionData = serde_json::from_slice(&bytes)
        .context("Failed to deserialize auth_session JSON from redb")?;
    Ok(Some(parsed))
}

fn save_session_to_db(db: &Database, session: &SessionData) -> Result<()> {
    let bytes = serde_json::to_vec(session).context("Failed to serialize SessionData")?;
    db::write_meta(db, db::KEY_AUTH_SESSION, &bytes)
        .context("Failed to persist auth_session to redb")
}

fn remove_session_from_db(db: &Database) -> Result<()> {
    db::delete_meta(db, db::KEY_AUTH_SESSION).context("Failed to remove auth_session from redb")
}

fn try_migrate_legacy_session(db: &Database, legacy_path: &Path) -> Option<SessionData> {
    db::migrate_legacy_json(
        db,
        legacy_path,
        db::KEY_AUTH_SESSION,
        db::KEY_AUTH_SESSION_MIGRATED,
        "session.json",
        |path| match load_legacy_session_file(path) {
            Ok(Some(s)) if s.is_valid_format() => Some(s),
            Ok(Some(_)) => {
                log::warn!(
                    "Legacy session {:?} has invalid format; skipping migration",
                    path
                );
                None
            }
            Ok(None) => None,
            Err(e) => {
                log::warn!(
                    "Failed to load legacy session {:?}; leaving file untouched: {}",
                    path,
                    e
                );
                None
            }
        },
    )
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct QrGenerateResult {
    pub url: String,
    pub qrcode_key: String,
    pub svg: String,
    pub terminal_qr: String,
}

/// Manages Bilibili authentication state, session persistence, and QR login lifecycle.
pub struct AuthManager {
    db: Arc<Database>,
    write_lock: Mutex<()>,
    session: RwLock<Option<SessionData>>,
    state_tx: watch::Sender<AuthState>,
    api: BiliApiClient,
}

impl AuthManager {
    /// Open a dedicated `redb` database at `db_path` (with optional legacy `session.json` migration).
    pub fn open<P: AsRef<Path>>(db_path: P, legacy_session_path: Option<&Path>) -> Result<Self> {
        let db = db::open_database(db_path.as_ref())?;
        Ok(Self::from_db(db, legacy_session_path))
    }

    /// Initialize [`AuthManager`] backed by a shared `redb::Database`, performing a one-time
    /// migration from `legacy_session_path` if `redb` has no session yet and has not migrated before.
    pub fn from_db(db: Arc<Database>, legacy_session_path: Option<&Path>) -> Self {
        let loaded = match load_session_from_db(&db) {
            Ok(Some(s)) if s.is_valid_format() => Some(s),
            Ok(Some(_)) => {
                log::warn!("Stored auth_session in redb has invalid format; ignoring");
                None
            }
            Err(e) => {
                log::warn!("Failed to load auth_session from redb: {}", e);
                None
            }
            Ok(None) => legacy_session_path.and_then(|p| try_migrate_legacy_session(&db, p)),
        };

        let initial_state = match &loaded {
            Some(s) => AuthState {
                is_login: true,
                uname: if s.uname.trim().is_empty() {
                    format!("UID:{}", s.dede_user_id)
                } else {
                    s.uname.trim().to_string()
                },
                mid: s.dede_user_id.parse().unwrap_or(0),
                face: s.face.clone(),
            },
            None => AuthState::default(),
        };

        let (state_tx, _) = watch::channel(initial_state);
        Self {
            db,
            write_lock: Mutex::new(()),
            session: RwLock::new(loaded),
            state_tx,
            api: BiliApiClient::new(),
        }
    }

    pub fn subscribe(&self) -> watch::Receiver<AuthState> {
        self.state_tx.subscribe()
    }

    pub fn current_state(&self) -> AuthState {
        self.state_tx.borrow().clone()
    }

    pub fn current_session(&self) -> Option<SessionData> {
        self.session
            .read()
            .expect("AuthManager RwLock poisoned")
            .clone()
    }

    pub fn save_session(
        &self,
        mut session: SessionData,
        uname: String,
        face: String,
    ) -> Result<()> {
        let trimmed_uname = uname.trim();
        if !trimmed_uname.is_empty() && !trimmed_uname.starts_with("UID:") {
            session.uname = trimmed_uname.to_string();
        }
        let trimmed_face = face.trim();
        if !trimmed_face.is_empty() {
            session.face = trimmed_face.to_string();
        }
        let _guard = self
            .write_lock
            .lock()
            .expect("AuthManager write_lock poisoned");
        save_session_to_db(&self.db, &session)?;
        let mid = session.dede_user_id.parse().unwrap_or(0);
        *self.session.write().expect("AuthManager RwLock poisoned") = Some(session);
        self.state_tx.send_replace(AuthState {
            is_login: true,
            uname,
            mid,
            face,
        });
        Ok(())
    }

    fn persist_profile_cache(&self, uname: &str, face: &str) {
        let trimmed_uname = uname.trim();
        let trimmed_face = face.trim();
        if trimmed_uname.is_empty() || trimmed_uname.starts_with("UID:") {
            return;
        }
        let _guard = self
            .write_lock
            .lock()
            .expect("AuthManager write_lock poisoned");
        let updated = {
            let guard = self.session.read().expect("AuthManager RwLock poisoned");
            let Some(ref sess) = *guard else {
                return;
            };
            if sess.uname == trimmed_uname && (trimmed_face.is_empty() || sess.face == trimmed_face)
            {
                return;
            }
            let mut updated = sess.clone();
            updated.uname = trimmed_uname.to_string();
            if !trimmed_face.is_empty() {
                updated.face = trimmed_face.to_string();
            }
            updated
        };
        if let Err(e) = save_session_to_db(&self.db, &updated) {
            log::warn!("Failed to persist cached profile name to redb: {:#}", e);
            return;
        }
        *self.session.write().expect("AuthManager RwLock poisoned") = Some(updated);
    }

    fn apply_validated_auth(&self, state: AuthState) -> AuthState {
        if state.is_login {
            self.persist_profile_cache(&state.uname, &state.face);
            self.state_tx.send_replace(state.clone());
            log::info!(
                "✅ Bilibili session validated: @{} ({})",
                state.uname,
                state.mid
            );
            state
        } else {
            log::warn!("⚠️ Bilibili stored session expired or invalid");
            self.state_tx.send_replace(AuthState::default());
            AuthState::default()
        }
    }

    pub fn logout(&self) -> Result<()> {
        let _guard = self
            .write_lock
            .lock()
            .expect("AuthManager write_lock poisoned");
        remove_session_from_db(&self.db)?;
        *self.session.write().expect("AuthManager RwLock poisoned") = None;
        self.state_tx.send_replace(AuthState::default());
        log::info!("🔓 Bilibili session cleared");
        Ok(())
    }

    /// Clear in-memory and on-disk session credentials when an authenticated API reports expiry.
    pub fn invalidate_session(&self) {
        let _guard = self
            .write_lock
            .lock()
            .expect("AuthManager write_lock poisoned");
        if let Err(e) = remove_session_from_db(&self.db) {
            log::warn!("Failed to remove expired auth_session from redb: {}", e);
        }
        *self.session.write().expect("AuthManager RwLock poisoned") = None;
        self.state_tx.send_replace(AuthState::default());
        log::warn!("⚠️ Cleared expired Bilibili session");
    }

    /// Send a live danmaku text message to `room_id` via `blive-proto`.
    pub async fn send_danmaku(
        &self,
        room_id: u64,
        msg: &str,
    ) -> std::result::Result<(), SendDanmakuError> {
        if room_id == 0 {
            return Err(SendDanmakuError::NotConnected);
        }
        let Some(session) = self
            .current_session()
            .filter(|s| s.is_valid_format() && self.current_state().is_login)
        else {
            return Err(SendDanmakuError::NotLoggedIn);
        };

        let res = self.api.send_danmaku(&session, room_id, msg).await;
        if matches!(res, Err(SendDanmakuError::AuthExpired(_))) {
            self.invalidate_session();
        }
        res
    }

    /// Validate stored cookies via `blive-proto` (`nav` with live user-info fallback).
    pub async fn validate_session(&self) -> Result<AuthState> {
        let Some(session) = self.current_session().filter(SessionData::is_valid_format) else {
            let state = AuthState::default();
            self.state_tx.send_replace(state.clone());
            return Ok(state);
        };

        let state = self
            .api
            .validate_session(&session)
            .await
            .map_err(|e| anyhow!(e))?;
        Ok(self.apply_validated_auth(state))
    }

    /// Request a fresh QR login URL & key from Bilibili and render SVG + terminal QR codes.
    pub async fn generate_qr(&self) -> Result<QrGenerateResult> {
        let qr = self.api.generate_qr().await.map_err(|e| anyhow!(e))?;
        let (svg, terminal_qr) = render_qr(&qr.url)?;
        Ok(QrGenerateResult {
            url: qr.url,
            qrcode_key: qr.qrcode_key,
            svg,
            terminal_qr,
        })
    }

    /// Poll QR scan confirmation status and persist the session on success.
    pub async fn poll_qr(&self, qrcode_key: &str) -> Result<QrPollStatus> {
        let status = self.api.poll_qr(qrcode_key).await.map_err(|e| anyhow!(e))?;
        if let QrPollStatus::Success(ref session) = status {
            let mid = session.dede_user_id.parse().unwrap_or(0);
            self.save_session(
                session.clone(),
                format!("UID:{}", session.dede_user_id),
                String::new(),
            )?;
            if let Err(e) = self.validate_session().await {
                log::warn!("Post-login nav validation failed transiently: {:#}", e);
                self.state_tx.send_replace(AuthState {
                    is_login: true,
                    uname: format!("UID:{}", mid),
                    mid,
                    face: String::new(),
                });
            }
        }
        Ok(status)
    }

    /// Fetch the logged-in user's own live room via `DedeUserID`.
    pub async fn fetch_my_room(&self) -> Result<Option<RoomQuickItem>> {
        let Some(session) = self.current_session() else {
            return Ok(None);
        };
        let uname = self.current_state().uname;
        self.api
            .fetch_my_room(&session, &uname)
            .await
            .map_err(|e| anyhow!(e))
    }

    /// Fetch followed streamers that are currently live.
    pub async fn fetch_following_live(&self) -> Result<Vec<RoomQuickItem>> {
        let Some(session) = self.current_session() else {
            return Ok(Vec::new());
        };
        self.api
            .fetch_following_live(&session)
            .await
            .map_err(|e| anyhow!(e))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static AUTH_TEST_ID: AtomicU64 = AtomicU64::new(1);

    #[test]
    fn test_session_save_load_and_cookie_header() {
        let id = AUTH_TEST_ID.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("dambo_sess_{}_{}", std::process::id(), id));
        let _ = fs::remove_dir_all(&dir);
        let db_path = dir.join("dambo.redb");
        let legacy_path = dir.join("session.json");

        let session = SessionData {
            sessdata: "abc%2Cdef".to_string(),
            bili_jct: "csrf123".to_string(),
            dede_user_id: "3829104".to_string(),
            uname: String::new(),
            face: String::new(),
        };
        assert!(session.is_valid_format());
        assert_eq!(
            session.to_cookie_header(),
            "SESSDATA=abc%2Cdef; bili_jct=csrf123; DedeUserID=3829104"
        );

        let mgr = AuthManager::open(&db_path, None).unwrap();
        assert!(!mgr.current_state().is_login);
        mgr.save_session(session, String::new(), String::new())
            .unwrap();
        assert!(mgr.current_state().is_login);
        assert_eq!(mgr.current_state().mid, 3829104);
        drop(mgr);

        // Reopening with empty stored `uname` falls back to "UID:<mid>"
        let mgr = AuthManager::open(&db_path, None).unwrap();
        assert!(mgr.current_state().is_login);
        assert_eq!(mgr.current_state().mid, 3829104);
        assert_eq!(mgr.current_state().uname, "UID:3829104");

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&db_path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }

        mgr.persist_profile_cache("测试主播", "https://example.com/face.jpg");
        drop(mgr);

        let reopened = AuthManager::open(&db_path, None).unwrap();
        assert!(reopened.current_state().is_login);
        assert_eq!(reopened.current_state().uname, "测试主播");
        assert_eq!(
            reopened.current_state().face,
            "https://example.com/face.jpg"
        );
        assert_eq!(reopened.current_session().unwrap().sessdata, "abc%2Cdef");

        reopened.logout().unwrap();
        assert!(!reopened.current_state().is_login);
        drop(reopened);

        let after_logout = AuthManager::open(&db_path, None).unwrap();
        assert!(!after_logout.current_state().is_login);
        assert!(after_logout.current_session().is_none());
        drop(after_logout);

        // Legacy session.json without `uname`/`face` migrates into redb once and removes plaintext file
        let legacy_db_path = dir.join("legacy_dambo.redb");
        let legacy_json = r#"{"SESSDATA":"s_legacy","bili_jct":"j_legacy","DedeUserID":"778899"}"#;
        fs::write(&legacy_path, legacy_json).unwrap();
        let legacy_mgr = AuthManager::open(&legacy_db_path, Some(&legacy_path)).unwrap();
        assert!(legacy_mgr.current_state().is_login);
        assert_eq!(legacy_mgr.current_state().uname, "UID:778899");
        assert!(!legacy_path.exists());
        legacy_mgr.logout().unwrap();
        drop(legacy_mgr);

        let after_legacy_logout = AuthManager::open(&legacy_db_path, Some(&legacy_path)).unwrap();
        assert!(!after_legacy_logout.current_state().is_login);
        drop(after_legacy_logout);

        // Corrupt legacy session.json falls back cleanly without deleting the corrupt file
        let corrupt_db_path = dir.join("corrupt_dambo.redb");
        let corrupt_path = dir.join("corrupt_session.json");
        fs::write(&corrupt_path, "{ not valid session json").unwrap();
        let corrupt_mgr = AuthManager::open(&corrupt_db_path, Some(&corrupt_path)).unwrap();
        assert!(!corrupt_mgr.current_state().is_login);
        assert!(corrupt_path.exists());
        drop(corrupt_mgr);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_render_qr() {
        let url =
            "https://passport.bilibili.com/h5-app/passport/login/scan?navhide=1&qrcode_key=abc123";
        let (svg_xml, term_qr) = render_qr(url).unwrap();
        assert!(svg_xml.contains("<svg"));
        assert!(svg_xml.contains("</svg>"));
        assert!(!term_qr.trim().is_empty());
    }
}
