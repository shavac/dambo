use crate::db;
use crate::model::{AuthState, RoomQuickItem};
use anyhow::{Context, Result, anyhow};
use blive_proto::auth::form_urlencode;
pub use blive_proto::auth::{
    DANMAKU_MAX_CHARS, QrPollStatus, SendDanmakuError, SessionData, parse_live_user_info_response,
    parse_nav_response, parse_qr_poll_code, parse_qr_poll_response, parse_send_danmaku_response,
};
use blive_proto::commands::value_as_u64;
use qrcode::QrCode;
use qrcode::render::{svg, unicode};
use redb::Database;
use reqwest::header::{CONTENT_TYPE, COOKIE, ORIGIN, REFERER, SET_COOKIE};
use serde_json::Value;
use std::fs;
use std::path::Path;
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::watch;

const USER_AGENT: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36";

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
    http: reqwest::Client,
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
            Ok(None) => {
                let already_migrated = db::read_meta(&db, db::KEY_AUTH_SESSION_MIGRATED)
                    .ok()
                    .flatten()
                    .is_some();
                if !already_migrated
                    && let Some(legacy_path) = legacy_session_path
                    && legacy_path.exists()
                {
                    match load_legacy_session_file(legacy_path) {
                        Ok(Some(s)) if s.is_valid_format() => match serde_json::to_vec(&s) {
                            Ok(bytes) => {
                                if let Err(e) = db::write_meta_batch(
                                    &db,
                                    &[
                                        (db::KEY_AUTH_SESSION, bytes.as_slice()),
                                        (db::KEY_AUTH_SESSION_MIGRATED, b"1"),
                                    ],
                                ) {
                                    log::warn!(
                                        "Failed to migrate legacy session {:?} into redb: {}",
                                        legacy_path,
                                        e
                                    );
                                } else {
                                    if let Err(e) = fs::remove_file(legacy_path) {
                                        log::warn!(
                                            "Migrated legacy session {:?} into redb, but failed to remove legacy file: {}",
                                            legacy_path,
                                            e
                                        );
                                    } else {
                                        log::info!(
                                            "Migrated legacy session {:?} into redb and removed plaintext file",
                                            legacy_path
                                        );
                                    }
                                }
                                Some(s)
                            }
                            Err(e) => {
                                log::warn!(
                                    "Failed to serialize legacy session {:?} during migration: {}",
                                    legacy_path,
                                    e
                                );
                                Some(s)
                            }
                        },
                        Ok(Some(_)) => {
                            log::warn!(
                                "Legacy session {:?} has invalid format; skipping migration",
                                legacy_path
                            );
                            None
                        }
                        Ok(None) => None,
                        Err(e) => {
                            log::warn!(
                                "Failed to load legacy session {:?}; leaving file untouched: {}",
                                legacy_path,
                                e
                            );
                            None
                        }
                    }
                } else {
                    None
                }
            }
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

        let http = reqwest::Client::builder()
            .user_agent(USER_AGENT)
            .timeout(Duration::from_secs(15))
            .build()
            .expect("Failed to construct reqwest HTTP client");

        let (state_tx, _) = watch::channel(initial_state);
        Self {
            db,
            write_lock: Mutex::new(()),
            session: RwLock::new(loaded),
            state_tx,
            http,
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
        let snapshot_to_save = {
            let guard = self.session.read().expect("AuthManager RwLock poisoned");
            if let Some(ref sess) = *guard {
                if sess.uname == trimmed_uname
                    && (trimmed_face.is_empty() || sess.face == trimmed_face)
                {
                    return;
                }
                let mut updated = sess.clone();
                updated.uname = trimmed_uname.to_string();
                if !trimmed_face.is_empty() {
                    updated.face = trimmed_face.to_string();
                }
                Some(updated)
            } else {
                None
            }
        };
        if let Some(updated) = snapshot_to_save {
            if let Err(e) = save_session_to_db(&self.db, &updated) {
                log::warn!("Failed to persist cached profile name to redb: {:#}", e);
                return;
            }
            *self.session.write().expect("AuthManager RwLock poisoned") = Some(updated);
        }
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

    /// Send a live danmaku text message to `room_id` via `https://api.live.bilibili.com/msg/send`.
    pub async fn send_danmaku(
        &self,
        room_id: u64,
        msg: &str,
    ) -> std::result::Result<(), SendDanmakuError> {
        if room_id == 0 {
            return Err(SendDanmakuError::NotConnected);
        }
        let Some(session) = self.current_session() else {
            return Err(SendDanmakuError::NotLoggedIn);
        };
        if !session.is_valid_format() || !self.current_state().is_login {
            return Err(SendDanmakuError::NotLoggedIn);
        }

        let trimmed = msg.trim();
        if trimmed.is_empty() {
            return Err(SendDanmakuError::EmptyMessage);
        }
        if trimmed.chars().count() > DANMAKU_MAX_CHARS {
            return Err(SendDanmakuError::MessageTooLong {
                max: DANMAKU_MAX_CHARS,
            });
        }

        let rnd = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0)
            .to_string();
        let room_str = room_id.to_string();
        let csrf = session.bili_jct.trim();
        let body = form_urlencode(&[
            ("bubble", "0"),
            ("msg", trimmed),
            ("color", "16777215"),
            ("mode", "1"),
            ("fontsize", "25"),
            ("rnd", &rnd),
            ("roomid", &room_str),
            ("csrf", csrf),
            ("csrf_token", csrf),
        ]);

        let resp = self
            .http
            .post("https://api.live.bilibili.com/msg/send")
            .header(COOKIE, session.to_cookie_header())
            .header(REFERER, format!("https://live.bilibili.com/{}", room_id))
            .header(ORIGIN, "https://live.bilibili.com")
            .header(CONTENT_TYPE, "application/x-www-form-urlencoded")
            .body(body)
            .send()
            .await
            .map_err(|e| SendDanmakuError::Network(e.to_string()))?;

        let json: Value = resp
            .json()
            .await
            .map_err(|e| SendDanmakuError::Network(e.to_string()))?;

        let parsed = parse_send_danmaku_response(&json);
        if matches!(parsed, Err(SendDanmakuError::AuthExpired(_))) {
            self.invalidate_session();
        }
        parsed
    }

    /// Validate stored cookies against `https://api.bilibili.com/x/web-interface/nav`,
    /// falling back to `https://api.live.bilibili.com/xlive/web-ucenter/user/get_user_info`.
    pub async fn validate_session(&self) -> Result<AuthState> {
        let Some(session) = self.current_session() else {
            self.state_tx.send_replace(AuthState::default());
            return Ok(AuthState::default());
        };
        if !session.is_valid_format() {
            self.state_tx.send_replace(AuthState::default());
            return Ok(AuthState::default());
        }

        let cookie = session.to_cookie_header();
        let fallback_mid = session.dede_user_id.parse().unwrap_or(0);

        let nav_err: String = match self
            .http
            .get("https://api.bilibili.com/x/web-interface/nav")
            .header(COOKIE, &cookie)
            .header(REFERER, "https://www.bilibili.com/")
            .send()
            .await
        {
            Ok(resp) => match resp.json::<Value>().await {
                Ok(json) => {
                    if let Some(state) = parse_nav_response(&json, fallback_mid) {
                        return Ok(self.apply_validated_auth(state));
                    }
                    format!(
                        "unexpected nav payload (code={})",
                        json["code"].as_i64().unwrap_or(-1)
                    )
                }
                Err(e) => format!("Failed to decode Bilibili nav JSON: {:#}", e),
            },
            Err(e) => format!("Failed to request Bilibili nav API: {:#}", e),
        };

        log::warn!(
            "Primary Bilibili nav API unavailable ({}), falling back to live user info API",
            nav_err
        );

        let live_res = self
            .http
            .get("https://api.live.bilibili.com/xlive/web-ucenter/user/get_user_info")
            .header(COOKIE, &cookie)
            .header(REFERER, "https://live.bilibili.com/")
            .send()
            .await;

        let live_err: String = match live_res {
            Ok(resp) => match resp.json::<Value>().await {
                Ok(json) => {
                    if let Some(state) = parse_live_user_info_response(&json, fallback_mid) {
                        return Ok(self.apply_validated_auth(state));
                    }
                    format!(
                        "unexpected live user_info payload (code={})",
                        json["code"].as_i64().unwrap_or(-1)
                    )
                }
                Err(e) => format!("Failed to decode Bilibili live user_info JSON: {:#}", e),
            },
            Err(e) => format!("Failed to request Bilibili live user_info API: {:#}", e),
        };

        Err(anyhow!("nav: {}; live_user_info: {}", nav_err, live_err))
    }

    /// Request a fresh QR login URL & key from `passport.bilibili.com`.
    pub async fn generate_qr(&self) -> Result<QrGenerateResult> {
        let resp: Value = self
            .http
            .get("https://passport.bilibili.com/x/passport-login/web/qrcode/generate")
            .header(REFERER, "https://passport.bilibili.com/login")
            .send()
            .await
            .context("Failed to request Bilibili QR generate API")?
            .json()
            .await
            .context("Failed to decode Bilibili QR generate JSON")?;

        let code = resp["code"].as_i64().unwrap_or(-1);
        if code != 0 {
            return Err(anyhow!(
                "QR generate API failed (code {}): {}",
                code,
                resp["message"].as_str().unwrap_or("unknown")
            ));
        }

        let url = resp["data"]["url"]
            .as_str()
            .filter(|s| !s.is_empty())
            .ok_or_else(|| anyhow!("QR generate response missing data.url"))?
            .to_string();
        let qrcode_key = resp["data"]["qrcode_key"]
            .as_str()
            .filter(|s| !s.is_empty())
            .ok_or_else(|| anyhow!("QR generate response missing data.qrcode_key"))?
            .to_string();

        let (svg, terminal_qr) = render_qr(&url)?;
        Ok(QrGenerateResult {
            url,
            qrcode_key,
            svg,
            terminal_qr,
        })
    }

    /// Poll QR scan confirmation status.
    pub async fn poll_qr(&self, qrcode_key: &str) -> Result<QrPollStatus> {
        let response = self
            .http
            .get("https://passport.bilibili.com/x/passport-login/web/qrcode/poll")
            .query(&[("qrcode_key", qrcode_key)])
            .header(REFERER, "https://passport.bilibili.com/login")
            .send()
            .await
            .context("Failed to request Bilibili QR poll API")?;

        let set_cookies: Vec<String> = response
            .headers()
            .get_all(SET_COOKIE)
            .iter()
            .filter_map(|hv| hv.to_str().ok().map(|s| s.to_string()))
            .collect();

        let body: Value = response
            .json()
            .await
            .context("Failed to decode Bilibili QR poll JSON")?;

        let code = body["data"]["code"].as_i64().unwrap_or(-1);
        let message = body["data"]["message"].as_str().unwrap_or("");
        let fallback_url = body["data"]["url"].as_str();

        let status = parse_qr_poll_response(code, message, &set_cookies, fallback_url);
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
        let mid = session.dede_user_id.trim();
        if mid.is_empty() {
            return Ok(None);
        }

        let resp: Value = self
            .http
            .get("https://api.live.bilibili.com/room/v1/Room/getRoomInfoOld")
            .query(&[("mid", mid)])
            .header(COOKIE, session.to_cookie_header())
            .header(REFERER, "https://live.bilibili.com/")
            .send()
            .await
            .context("Failed to query getRoomInfoOld")?
            .json()
            .await
            .context("Failed to parse getRoomInfoOld JSON")?;

        if resp["code"].as_i64() != Some(0) {
            return Ok(None);
        }

        let data = &resp["data"];
        let room_status = data["roomStatus"].as_i64().unwrap_or(0);
        let room_id = value_as_u64(&data["roomid"]).unwrap_or(0);
        if room_status == 1 && room_id > 0 {
            let title = data["title"].as_str().unwrap_or("").to_string();
            let is_live = data["liveStatus"].as_i64() == Some(1);
            let room_uname = self.current_state().uname;
            Ok(Some(RoomQuickItem {
                room_id,
                room_uname,
                title,
                is_live,
            }))
        } else {
            Ok(None)
        }
    }

    /// Fetch followed streamers that are currently live.
    pub async fn fetch_following_live(&self) -> Result<Vec<RoomQuickItem>> {
        let Some(session) = self.current_session() else {
            return Ok(Vec::new());
        };
        let cookie = session.to_cookie_header();

        let primary = self
            .http
            .get("https://api.live.bilibili.com/relation/v1/feed/feed_list")
            .query(&[("page", "1"), ("pagesize", "30")])
            .header(COOKIE, &cookie)
            .header(REFERER, "https://live.bilibili.com/")
            .send()
            .await;

        if let Ok(resp) = primary
            && let Ok(json) = resp.json::<Value>().await
            && json["code"].as_i64() == Some(0)
            && let Some(arr) = json["data"]["list"].as_array()
        {
            let items = arr
                .iter()
                .filter_map(|item| {
                    let room_id = value_as_u64(&item["roomid"])?;
                    if room_id == 0 {
                        return None;
                    }
                    Some(RoomQuickItem {
                        room_id,
                        room_uname: item["uname"].as_str().unwrap_or("").to_string(),
                        title: item["title"].as_str().unwrap_or("").to_string(),
                        is_live: true,
                    })
                })
                .collect::<Vec<_>>();
            return Ok(items);
        }

        // Fallback endpoint
        let fallback_resp: Value = self
            .http
            .get("https://api.live.bilibili.com/xlive/web-ucenter/user/following")
            .query(&[("page", "1"), ("page_size", "29")])
            .header(COOKIE, &cookie)
            .header(REFERER, "https://live.bilibili.com/")
            .send()
            .await
            .context("Failed to query Bilibili following live list")?
            .json()
            .await
            .context("Failed to parse following live JSON")?;

        if fallback_resp["code"].as_i64() != Some(0) {
            return Ok(Vec::new());
        }

        let items = fallback_resp["data"]["list"]
            .as_array()
            .map(|arr| {
                arr.iter()
                    .filter(|item| item["live_status"].as_i64() == Some(1))
                    .filter_map(|item| {
                        let room_id = value_as_u64(&item["roomid"])?;
                        if room_id == 0 {
                            return None;
                        }
                        Some(RoomQuickItem {
                            room_id,
                            room_uname: item["uname"].as_str().unwrap_or("").to_string(),
                            title: item["title"].as_str().unwrap_or("").to_string(),
                            is_live: true,
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();

        Ok(items)
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
        mgr.save_session(session.clone(), String::new(), String::new())
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
