use crate::model::{AuthState, RoomQuickItem};
use anyhow::{anyhow, Context, Result};
use qrcode::render::{svg, unicode};
use qrcode::QrCode;
use reqwest::header::{COOKIE, REFERER, SET_COOKIE};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::RwLock;
use std::time::Duration;
use tokio::sync::watch;

const USER_AGENT: &str =
    "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36";

fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(hex) = std::str::from_utf8(&bytes[i + 1..i + 3]) {
                if let Ok(val) = u8::from_str_radix(hex, 16) {
                    out.push(val);
                    i += 3;
                    continue;
                }
            }
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
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                fs::create_dir_all(parent)
                    .with_context(|| format!("Failed to create parent dir for {:?}", path))?;
            }
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
            if let Some(kv) = h.split(';').next() {
                if let Some((k, v)) = kv.split_once('=') {
                    match k.trim() {
                        "SESSDATA" => sessdata = v.trim().to_string(),
                        "bili_jct" => bili_jct = v.trim().to_string(),
                        "DedeUserID" => dede_user_id = v.trim().to_string(),
                        _ => {}
                    }
                }
            }
        }

        let session = Self {
            sessdata,
            bili_jct,
            dede_user_id,
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
                uname: format!("UID:{}", s.dede_user_id),
                mid: s.dede_user_id.parse().unwrap_or(0),
                face: String::new(),
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
        self.session.read().expect("AuthManager RwLock poisoned").clone()
    }

    pub fn save_session(&self, session: SessionData, uname: String, face: String) -> Result<()> {
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

    pub fn logout(&self) -> Result<()> {
        SessionData::remove(&self.session_path)?;
        *self.session.write().expect("AuthManager RwLock poisoned") = None;
        self.state_tx.send_replace(AuthState::default());
        log::info!("🔓 Bilibili session cleared");
        Ok(())
    }

    /// Validate stored cookies against `https://api.bilibili.com/x/web-interface/nav`.
    pub async fn validate_session(&self) -> Result<AuthState> {
        let Some(session) = self.current_session() else {
            self.state_tx.send_replace(AuthState::default());
            return Ok(AuthState::default());
        };
        if !session.is_valid_format() {
            self.state_tx.send_replace(AuthState::default());
            return Ok(AuthState::default());
        }

        let resp: Value = self
            .http
            .get("https://api.bilibili.com/x/web-interface/nav")
            .header(COOKIE, session.to_cookie_header())
            .header(REFERER, "https://www.bilibili.com/")
            .send()
            .await
            .context("Failed to request Bilibili nav API")?
            .json()
            .await
            .context("Failed to decode Bilibili nav JSON")?;

        let code = resp["code"].as_i64().unwrap_or(-1);
        let is_login = resp["data"]["isLogin"].as_bool().unwrap_or(false);
        if code == 0 && is_login {
            let state = AuthState {
                is_login: true,
                uname: resp["data"]["uname"].as_str().unwrap_or("").to_string(),
                mid: value_as_u64(&resp["data"]["mid"])
                    .unwrap_or_else(|| session.dede_user_id.parse().unwrap_or(0)),
                face: resp["data"]["face"].as_str().unwrap_or("").to_string(),
            };
            self.state_tx.send_replace(state.clone());
            log::info!("✅ Bilibili session validated: @{} ({})", state.uname, state.mid);
            Ok(state)
        } else {
            log::warn!("⚠️ Bilibili stored session expired or invalid (code={})", code);
            self.state_tx.send_replace(AuthState::default());
            Ok(AuthState::default())
        }
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
            self.save_session(session.clone(), format!("UID:{}", session.dede_user_id), String::new())?;
            if let Err(e) = self.validate_session().await {
                log::warn!("Post-login nav validation failed transiently: {}", e);
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

        if let Ok(resp) = primary {
            if let Ok(json) = resp.json::<Value>().await {
                if json["code"].as_i64() == Some(0) {
                    if let Some(arr) = json["data"]["list"].as_array() {
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
                }
            }
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
        let path = std::env::temp_dir().join(format!("dambo_sess_{}_{}.json", std::process::id(), id));
        let _ = fs::remove_file(&path);

        let session = SessionData {
            sessdata: "abc%2Cdef".to_string(),
            bili_jct: "csrf123".to_string(),
            dede_user_id: "3829104".to_string(),
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

        mgr.logout().unwrap();
        assert!(!mgr.current_state().is_login);
        assert!(!path.exists());
    }

    #[test]
    fn test_from_set_cookie_headers_and_qr_poll_fsm() {
        let cookies = vec![
            "SESSDATA=s123; Path=/; Domain=.bilibili.com; HttpOnly".to_string(),
            "bili_jct=j456; Path=/; Domain=.bilibili.com".to_string(),
            "DedeUserID=998877; Path=/".to_string(),
        ];

        assert_eq!(parse_qr_poll_code(86101, "未扫码", &[]), QrPollStatus::Waiting);
        assert_eq!(parse_qr_poll_code(86090, "已扫码", &[]), QrPollStatus::Scanned);
        assert_eq!(parse_qr_poll_code(86038, "已过期", &[]), QrPollStatus::Expired);

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
        let url = "https://passport.bilibili.com/h5-app/passport/login/scan?navhide=1&qrcode_key=abc123";
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
}
