use crate::model::{AuthState, RoomQuickItem};
use anyhow::{Context, Result, anyhow};
use qrcode::QrCode;
use qrcode::render::{svg, unicode};
use reqwest::header::{CONTENT_TYPE, COOKIE, ORIGIN, REFERER, SET_COOKIE};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::RwLock;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::watch;

const USER_AGENT: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36";

fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let Ok(hex) = std::str::from_utf8(&bytes[i + 1..i + 3])
            && let Ok(val) = u8::from_str_radix(hex, 16)
        {
            out.push(val);
            i += 3;
            continue;
        } else if bytes[i] == b'+' {
            out.push(b' ');
            i += 1;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).to_string()
}

fn value_as_u64(v: &Value) -> Option<u64> {
    v.as_u64()
        .or_else(|| v.as_i64().filter(|&n| n >= 0).map(|n| n as u64))
        .or_else(|| v.as_str().and_then(|s| s.parse::<u64>().ok()))
}

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

/// Persisted Bilibili session cookies stored in `dambo_session.json`.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct SessionData {
    #[serde(rename = "SESSDATA", alias = "sessdata")]
    pub sessdata: String,
    #[serde(rename = "bili_jct", alias = "biliJct")]
    pub bili_jct: String,
    #[serde(rename = "DedeUserID", alias = "dede_user_id")]
    pub dede_user_id: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub uname: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub face: String,
}

impl SessionData {
    pub fn load(path: &Path) -> Result<Option<Self>> {
        if !path.exists() {
            return Ok(None);
        }
        let raw = fs::read_to_string(path)
            .with_context(|| format!("Failed to read session file {:?}", path))?;
        let parsed: SessionData = serde_json::from_str(&raw)
            .with_context(|| format!("Failed to parse session JSON at {:?}", path))?;
        Ok(Some(parsed))
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            fs::create_dir_all(parent)
                .with_context(|| format!("Failed to create parent dir for {:?}", path))?;
        }
        let json = serde_json::to_string_pretty(self)?;
        fs::write(path, json).with_context(|| format!("Failed to write session to {:?}", path))
    }

    pub fn remove(path: &Path) -> Result<()> {
        if path.exists() {
            fs::remove_file(path)
                .with_context(|| format!("Failed to delete session file {:?}", path))?;
        }
        Ok(())
    }

    pub fn is_valid_format(&self) -> bool {
        !self.sessdata.trim().is_empty()
            && !self.bili_jct.trim().is_empty()
            && !self.dede_user_id.trim().is_empty()
    }

    pub fn to_cookie_header(&self) -> String {
        format!(
            "SESSDATA={}; bili_jct={}; DedeUserID={}",
            self.sessdata.trim(),
            self.bili_jct.trim(),
            self.dede_user_id.trim()
        )
    }

    /// Extract SESSDATA, bili_jct, and DedeUserID from HTTP `Set-Cookie` header values.
    pub fn from_set_cookie_headers(headers: &[String]) -> Option<Self> {
        let mut sessdata = String::new();
        let mut bili_jct = String::new();
        let mut dede_user_id = String::new();

        for h in headers {
            if let Some(kv) = h.split(';').next()
                && let Some((k, v)) = kv.split_once('=')
            {
                match k.trim() {
                    "SESSDATA" => sessdata = v.trim().to_string(),
                    "bili_jct" => bili_jct = v.trim().to_string(),
                    "DedeUserID" => dede_user_id = v.trim().to_string(),
                    _ => {}
                }
            }
        }

        let session = Self {
            sessdata,
            bili_jct,
            dede_user_id,
            uname: String::new(),
            face: String::new(),
        };
        if session.is_valid_format() {
            Some(session)
        } else {
            None
        }
    }

    /// Fallback extractor parsing `SESSDATA`, `bili_jct`, `DedeUserID` from QR poll `data.url` query params.
    pub fn from_login_url(url: &str) -> Option<Self> {
        let (_, query) = url.split_once('?')?;
        let mut sessdata = String::new();
        let mut bili_jct = String::new();
        let mut dede_user_id = String::new();

        for pair in query.split('&') {
            if let Some((k, v)) = pair.split_once('=') {
                let decoded = percent_decode(v.trim());
                match k.trim() {
                    "SESSDATA" => sessdata = decoded,
                    "bili_jct" => bili_jct = decoded,
                    "DedeUserID" => dede_user_id = decoded,
                    _ => {}
                }
            }
        }

        let session = Self {
            sessdata,
            bili_jct,
            dede_user_id,
            uname: String::new(),
            face: String::new(),
        };
        if session.is_valid_format() {
            Some(session)
        } else {
            None
        }
    }
}

/// QR login poll FSM states returned by `passport.bilibili.com/x/passport-login/web/qrcode/poll`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum QrPollStatus {
    Waiting,
    Scanned,
    Expired,
    Success(SessionData),
    Error(String),
}

pub fn parse_qr_poll_response(
    code: i64,
    message: &str,
    set_cookies: &[String],
    fallback_url: Option<&str>,
) -> QrPollStatus {
    match code {
        0 => {
            if let Some(session) = SessionData::from_set_cookie_headers(set_cookies)
                .or_else(|| fallback_url.and_then(SessionData::from_login_url))
            {
                QrPollStatus::Success(session)
            } else {
                QrPollStatus::Error(
                    "Missing SESSDATA/bili_jct/DedeUserID in Set-Cookie and login URL".to_string(),
                )
            }
        }
        86101 => QrPollStatus::Waiting,
        86090 => QrPollStatus::Scanned,
        86038 => QrPollStatus::Expired,
        other => QrPollStatus::Error(format!("Bilibili QR poll code {}: {}", other, message)),
    }
}

pub fn parse_qr_poll_code(code: i64, message: &str, set_cookies: &[String]) -> QrPollStatus {
    parse_qr_poll_response(code, message, set_cookies, None)
}

/// Parses `https://api.bilibili.com/x/web-interface/nav` JSON into an `AuthState`.
/// Returns `None` on transient/risk-control errors (e.g. `-412`) so fallback APIs can run.
pub fn parse_nav_response(resp: &Value, fallback_mid: u64) -> Option<AuthState> {
    let code = resp["code"].as_i64().unwrap_or(-1);
    let is_login = resp["data"]["isLogin"].as_bool();
    if code == 0 && is_login == Some(true) {
        let uname = resp["data"]["uname"]
            .as_str()
            .unwrap_or("")
            .trim()
            .to_string();
        if uname.is_empty() {
            return None;
        }
        return Some(AuthState {
            is_login: true,
            uname,
            mid: value_as_u64(&resp["data"]["mid"]).unwrap_or(fallback_mid),
            face: resp["data"]["face"]
                .as_str()
                .unwrap_or("")
                .trim()
                .to_string(),
        });
    }
    if code == -101 || (code == 0 && is_login == Some(false)) {
        return Some(AuthState::default());
    }
    None
}

/// Parses `https://api.live.bilibili.com/xlive/web-ucenter/user/get_user_info` JSON into an `AuthState`.
/// Returns `None` on transient/risk-control errors so cached auth state is preserved.
pub fn parse_live_user_info_response(resp: &Value, fallback_mid: u64) -> Option<AuthState> {
    let code = resp["code"].as_i64().unwrap_or(-1);
    if code == 0 {
        let data = &resp["data"];
        let uname = data["uname"].as_str().unwrap_or("").trim().to_string();
        if uname.is_empty() {
            return None;
        }
        return Some(AuthState {
            is_login: true,
            uname,
            mid: value_as_u64(&data["uid"]).unwrap_or(fallback_mid),
            face: data["face"].as_str().unwrap_or("").trim().to_string(),
        });
    }
    if code == -101 {
        return Some(AuthState::default());
    }
    None
}

/// Maximum character count allowed per sent Bilibili live danmaku message.
pub const DANMAKU_MAX_CHARS: usize = 40;

/// Structured errors returned when sending a Bilibili live danmaku message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SendDanmakuError {
    NotLoggedIn,
    NotConnected,
    EmptyMessage,
    MessageTooLong { max: usize },
    RateLimited(String),
    AuthExpired(String),
    Rejected(String),
    Network(String),
}

impl fmt::Display for SendDanmakuError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotLoggedIn => write!(f, "Not logged in"),
            Self::NotConnected => write!(f, "Not connected to a live room"),
            Self::EmptyMessage => write!(f, "Danmaku message is empty"),
            Self::MessageTooLong { max } => {
                write!(f, "Danmaku message exceeds {} characters", max)
            }
            Self::RateLimited(msg) => {
                if msg.is_empty() {
                    write!(f, "Sending too fast")
                } else {
                    write!(f, "Rate limited: {}", msg)
                }
            }
            Self::AuthExpired(msg) => {
                if msg.is_empty() {
                    write!(f, "Login session expired")
                } else {
                    write!(f, "Login session expired: {}", msg)
                }
            }
            Self::Rejected(msg) => write!(f, "{}", msg),
            Self::Network(msg) => write!(f, "{}", msg),
        }
    }
}

impl std::error::Error for SendDanmakuError {}

fn form_urlencode(pairs: &[(&str, &str)]) -> String {
    fn push_encoded(out: &mut String, raw: &str) {
        for &b in raw.as_bytes() {
            if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
                out.push(b as char);
            } else if b == b' ' {
                out.push('+');
            } else {
                out.push('%');
                out.push(
                    char::from_digit(u32::from(b >> 4), 16)
                        .unwrap_or('0')
                        .to_ascii_uppercase(),
                );
                out.push(
                    char::from_digit(u32::from(b & 0x0F), 16)
                        .unwrap_or('0')
                        .to_ascii_uppercase(),
                );
            }
        }
    }

    let mut encoded = String::new();
    for (idx, (k, v)) in pairs.iter().enumerate() {
        if idx > 0 {
            encoded.push('&');
        }
        push_encoded(&mut encoded, k);
        encoded.push('=');
        push_encoded(&mut encoded, v);
    }
    encoded
}

/// Parses `https://api.live.bilibili.com/msg/send` JSON into `Ok(())` or a typed `SendDanmakuError`.
pub fn parse_send_danmaku_response(resp: &Value) -> std::result::Result<(), SendDanmakuError> {
    let code = resp["code"].as_i64().unwrap_or(-1);
    let msg_field = resp["msg"].as_str().unwrap_or("").trim();
    let message_field = resp["message"].as_str().unwrap_or("").trim();
    let detail = if !message_field.is_empty() {
        message_field
    } else {
        msg_field
    };

    match code {
        0 => {
            if msg_field.is_empty() {
                return Ok(());
            }
            match msg_field {
                "f" | "k" | "fire" => Err(SendDanmakuError::Rejected(format!(
                    "filtered ({})",
                    msg_field
                ))),
                "msg in 1s" | "max limit exceeded" => {
                    Err(SendDanmakuError::RateLimited(msg_field.to_string()))
                }
                other => Err(SendDanmakuError::Rejected(other.to_string())),
            }
        }
        -101 | -111 => Err(SendDanmakuError::AuthExpired(if detail.is_empty() {
            format!("code {}", code)
        } else {
            detail.to_string()
        })),
        10030 | 10031 => Err(SendDanmakuError::RateLimited(detail.to_string())),
        other => Err(SendDanmakuError::Rejected(if detail.is_empty() {
            format!("code {}", other)
        } else {
            detail.to_string()
        })),
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct QrGenerateResult {
    pub url: String,
    pub qrcode_key: String,
    pub svg: String,
    pub terminal_qr: String,
}

/// Manages Bilibili authentication state, session persistence, and QR login lifecycle.
pub struct AuthManager {
    session_path: PathBuf,
    session: RwLock<Option<SessionData>>,
    state_tx: watch::Sender<AuthState>,
    http: reqwest::Client,
}

impl AuthManager {
    pub fn new<P: AsRef<Path>>(session_path: P) -> Self {
        let path = session_path.as_ref().to_path_buf();
        let loaded = match SessionData::load(&path) {
            Ok(Some(s)) if s.is_valid_format() => Some(s),
            Ok(_) => None,
            Err(e) => {
                log::warn!("Failed to load existing session {:?}: {}", path, e);
                None
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
            session_path: path,
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
        session.save(&self.session_path)?;
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
        let snapshot_to_save = {
            let mut guard = self.session.write().expect("AuthManager RwLock poisoned");
            if let Some(ref mut sess) = *guard {
                if sess.uname == trimmed_uname
                    && (trimmed_face.is_empty() || sess.face == trimmed_face)
                {
                    return;
                }
                sess.uname = trimmed_uname.to_string();
                if !trimmed_face.is_empty() {
                    sess.face = trimmed_face.to_string();
                }
                Some(sess.clone())
            } else {
                None
            }
        };
        if let Some(sess) = snapshot_to_save
            && let Err(e) = sess.save(&self.session_path)
        {
            log::warn!(
                "Failed to persist cached profile name to {:?}: {:#}",
                self.session_path,
                e
            );
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
        SessionData::remove(&self.session_path)?;
        *self.session.write().expect("AuthManager RwLock poisoned") = None;
        self.state_tx.send_replace(AuthState::default());
        log::info!("🔓 Bilibili session cleared");
        Ok(())
    }

    /// Clear in-memory and on-disk session credentials when an authenticated API reports expiry.
    pub fn invalidate_session(&self) {
        if let Err(e) = SessionData::remove(&self.session_path) {
            log::warn!(
                "Failed to remove expired session file {:?}: {}",
                self.session_path,
                e
            );
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
        let path =
            std::env::temp_dir().join(format!("dambo_sess_{}_{}.json", std::process::id(), id));
        let _ = fs::remove_file(&path);

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

        session.save(&path).unwrap();
        let loaded = SessionData::load(&path).unwrap().unwrap();
        assert_eq!(loaded, session);

        let mgr = AuthManager::new(&path);
        assert!(mgr.current_state().is_login);
        assert_eq!(mgr.current_state().mid, 3829104);
        assert_eq!(mgr.current_state().uname, "UID:3829104");

        // Legacy JSON without `uname`/`face` deserializes cleanly; cached uname restores directly
        let legacy_json = r#"{"SESSDATA":"s_legacy","bili_jct":"j_legacy","DedeUserID":"778899"}"#;
        fs::write(&path, legacy_json).unwrap();
        let legacy_mgr = AuthManager::new(&path);
        assert_eq!(legacy_mgr.current_state().uname, "UID:778899");

        legacy_mgr.persist_profile_cache("测试主播", "https://example.com/face.jpg");
        let cached_mgr = AuthManager::new(&path);
        assert!(cached_mgr.current_state().is_login);
        assert_eq!(cached_mgr.current_state().uname, "测试主播");
        assert_eq!(
            cached_mgr.current_state().face,
            "https://example.com/face.jpg"
        );

        cached_mgr.logout().unwrap();
        assert!(!cached_mgr.current_state().is_login);
        assert!(!path.exists());
    }

    #[test]
    fn test_parse_nav_and_live_user_info_responses() {
        let nav_ok = serde_json::json!({
            "code": 0,
            "data": {
                "isLogin": true,
                "uname": "主播A",
                "mid": 123456,
                "face": "https://i0.hdslb.com/a.jpg"
            }
        });
        let parsed = parse_nav_response(&nav_ok, 999).unwrap();
        assert!(parsed.is_login);
        assert_eq!(parsed.uname, "主播A");
        assert_eq!(parsed.mid, 123456);
        assert_eq!(parsed.face, "https://i0.hdslb.com/a.jpg");

        let nav_logged_out = serde_json::json!({"code": 0, "data": {"isLogin": false}});
        assert!(!parse_nav_response(&nav_logged_out, 999).unwrap().is_login);
        let nav_expired = serde_json::json!({"code": -101});
        assert!(!parse_nav_response(&nav_expired, 999).unwrap().is_login);
        let nav_risk = serde_json::json!({"code": -412, "message": "risk"});
        assert!(parse_nav_response(&nav_risk, 999).is_none());

        let live_ok = serde_json::json!({
            "code": 0,
            "data": {
                "uid": 123456,
                "uname": "主播B",
                "face": "https://i0.hdslb.com/b.jpg"
            }
        });
        let live_parsed = parse_live_user_info_response(&live_ok, 999).unwrap();
        assert!(live_parsed.is_login);
        assert_eq!(live_parsed.uname, "主播B");
        assert_eq!(live_parsed.mid, 123456);
        assert_eq!(live_parsed.face, "https://i0.hdslb.com/b.jpg");

        let live_expired = serde_json::json!({"code": -101});
        assert!(
            !parse_live_user_info_response(&live_expired, 999)
                .unwrap()
                .is_login
        );
        let live_risk = serde_json::json!({"code": -412});
        assert!(parse_live_user_info_response(&live_risk, 999).is_none());
    }

    #[test]
    fn test_from_set_cookie_headers_and_qr_poll_fsm() {
        let cookies = vec![
            "SESSDATA=s123; Path=/; Domain=.bilibili.com; HttpOnly".to_string(),
            "bili_jct=j456; Path=/; Domain=.bilibili.com".to_string(),
            "DedeUserID=998877; Path=/".to_string(),
        ];

        assert_eq!(
            parse_qr_poll_code(86101, "未扫码", &[]),
            QrPollStatus::Waiting
        );
        assert_eq!(
            parse_qr_poll_code(86090, "已扫码", &[]),
            QrPollStatus::Scanned
        );
        assert_eq!(
            parse_qr_poll_code(86038, "已过期", &[]),
            QrPollStatus::Expired
        );

        match parse_qr_poll_code(0, "", &cookies) {
            QrPollStatus::Success(s) => {
                assert_eq!(s.sessdata, "s123");
                assert_eq!(s.bili_jct, "j456");
                assert_eq!(s.dede_user_id, "998877");
            }
            other => panic!("Unexpected poll status: {:?}", other),
        }

        assert!(matches!(
            parse_qr_poll_code(0, "", &["SESSDATA=only_one; Path=/".to_string()]),
            QrPollStatus::Error(_)
        ));
    }

    #[test]
    fn test_render_qr_and_from_login_url() {
        let url =
            "https://passport.bilibili.com/h5-app/passport/login/scan?navhide=1&qrcode_key=abc123";
        let (svg_xml, term_qr) = render_qr(url).unwrap();
        assert!(svg_xml.contains("<svg"));
        assert!(svg_xml.contains("</svg>"));
        assert!(!term_qr.trim().is_empty());

        let login_url = "https://passport.biligame.com/x/passport-login/web/crossDomain?DedeUserID=123456&DedeUserID__ckMd5=aaa&Expires=123&SESSDATA=val1%2Cval2%2A&bili_jct=csrf999";
        let parsed = SessionData::from_login_url(login_url).unwrap();
        assert_eq!(parsed.dede_user_id, "123456");
        assert_eq!(parsed.sessdata, "val1,val2*");
        assert_eq!(parsed.bili_jct, "csrf999");

        match parse_qr_poll_response(0, "", &[], Some(login_url)) {
            QrPollStatus::Success(s) => assert_eq!(s.dede_user_id, "123456"),
            other => panic!("Expected fallback login URL success, got {:?}", other),
        }
    }

    #[test]
    fn test_form_urlencode_utf8_and_special_chars() {
        let encoded = form_urlencode(&[("msg", "hello 弹幕+100%&!"), ("csrf", "abc_123-XYZ.~")]);
        assert_eq!(
            encoded,
            "msg=hello+%E5%BC%B9%E5%B9%95%2B100%25%26%21&csrf=abc_123-XYZ.~"
        );
    }

    #[test]
    fn test_parse_send_danmaku_response_variants() {
        assert_eq!(
            parse_send_danmaku_response(&serde_json::json!({"code": 0, "msg": "", "message": ""})),
            Ok(())
        );
        assert_eq!(
            parse_send_danmaku_response(&serde_json::json!({"code": 0, "msg": "f"})),
            Err(SendDanmakuError::Rejected("filtered (f)".to_string()))
        );
        assert_eq!(
            parse_send_danmaku_response(
                &serde_json::json!({"code": -101, "message": "账号未登录"})
            ),
            Err(SendDanmakuError::AuthExpired("账号未登录".to_string()))
        );
        assert_eq!(
            parse_send_danmaku_response(
                &serde_json::json!({"code": -111, "message": "csrf 校验失败"})
            ),
            Err(SendDanmakuError::AuthExpired("csrf 校验失败".to_string()))
        );
        assert_eq!(
            parse_send_danmaku_response(
                &serde_json::json!({"code": 10030, "message": "发送过于频繁"})
            ),
            Err(SendDanmakuError::RateLimited("发送过于频繁".to_string()))
        );
        assert_eq!(
            parse_send_danmaku_response(
                &serde_json::json!({"code": 1003212, "message": "超出限制"})
            ),
            Err(SendDanmakuError::Rejected("超出限制".to_string()))
        );
    }
}
