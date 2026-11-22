// src/auth.rs
//! Authentication helpers for Bilibili live danmaku WebSocket client

use crate::models::AuthState;
use md5;
use reqwest::StatusCode;
use reqwest::header::HeaderMap;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fmt;
use std::time::{SystemTime, UNIX_EPOCH};

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

/// Bilibili session cookies (`SESSDATA`, `bili_jct`, `DedeUserID`) and cached profile metadata.
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

pub fn form_urlencode(pairs: &[(&str, &str)]) -> String {
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
pub fn parse_send_danmaku_response(resp: &Value) -> Result<(), SendDanmakuError> {
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

/// Validate explicit Bilibili cookies and fail with a clear error when missing.
pub fn resolve_cookies(provided_cookie: Option<&str>) -> Result<String, String> {
    if let Some(cookie) = provided_cookie
        && cookie.split(';').map(str::trim).any(|part| {
            part.strip_prefix("SESSDATA=")
                .is_some_and(|value| !value.is_empty() && value != "dummy_sessdata")
        })
    {
        log::info!("Using provided Bilibili cookies");
        return Ok(cookie.to_string());
    }
    Err("No valid explicit cookies supplied".into())
}

pub fn init_uid(headers: HeaderMap) -> (StatusCode, String) {
    let client = reqwest::blocking::Client::builder()
        .https_only(true)
        .build()
        .unwrap();

    let mut request_headers = headers;
    request_headers.insert("user-agent", USER_AGENT.parse().unwrap());

    let response = client.get(UID_INIT_URL).headers(request_headers).send();
    log::debug!("init uid response: {:?}", response);
    let stat: StatusCode;
    let body: String;
    match response {
        Ok(resp) => {
            stat = resp.status();
            body = resp.text().unwrap();
            log::info!("init uid response: {:?}", body);
        }
        Err(_) => {
            panic!("init uid failed");
        }
    }
    (stat, body)
}

/// Initializes the buvid by sending a request and extracting the 'buvid3' cookie.
///
/// Note: This function is not used for document creation.
///
/// # Panics
///
/// Panics if the request fails.
pub fn init_buvid(headers: HeaderMap) -> (StatusCode, String) {
    // Not used for document creation.
    let client = reqwest::blocking::Client::builder()
        .https_only(true)
        .build()
        .unwrap();

    let mut request_headers = headers;
    request_headers.insert("user-agent", USER_AGENT.parse().unwrap());

    let response = client.get(BUVID_INIT_URL).headers(request_headers).send();
    let stat: StatusCode;
    let mut buvid: String = "".to_string();
    match response {
        Ok(resp) => {
            stat = resp.status();
            let cookies = resp.cookies();
            for i in cookies {
                log::debug!("init buvid response cookie : {:?}", i);
                if "buvid3".eq(i.name()) {
                    buvid = i.value().to_string();
                    log::info!("init buvid response: {:?}", buvid);
                }
            }
        }
        Err(_) => {
            panic!("init buvid failed");
        }
    }
    (stat, buvid)
}

/// Initializes the room by sending a request with the given room ID.
///
/// Note: This function should NOT be used for document creation.
///
/// # Panics
///
/// Panics if the request fails.
pub fn init_room(headers: HeaderMap, temp_room_id: &str) -> (StatusCode, String) {
    let client = reqwest::blocking::Client::builder()
        .https_only(true)
        .build()
        .unwrap();

    let mut request_headers = headers;
    request_headers.insert("user-agent", USER_AGENT.parse().unwrap());

    let url = format!("{}?room_id={}", ROOM_INIT_URL, temp_room_id);
    let response = client.get(url).headers(request_headers).send();
    let stat: StatusCode;
    let body: String;
    match response {
        Ok(resp) => {
            stat = resp.status();
            body = resp.text().unwrap();
            log::info!("init room response: {:?}", body);
        }
        Err(_) => {
            panic!("init buvid failed");
        }
    }
    (stat, body)
}

pub fn init_host_server(headers: HeaderMap, room_id: u64) -> (StatusCode, String) {
    let client = reqwest::blocking::Client::builder()
        .https_only(true)
        .build()
        .unwrap();

    let mut request_headers = headers.clone();
    request_headers.insert("user-agent", USER_AGENT.parse().unwrap());

    // Get WBI keys for signing
    let wbi_keys = match get_wbi_keys(request_headers.clone()) {
        Ok(keys) => keys,
        Err(e) => {
            log::error!("Failed to get WBI keys: {:?}", e);
            panic!("Failed to get WBI keys");
        }
    };

    // Prepare parameters for signing
    let params = vec![
        ("id", room_id.to_string()),
        ("type", "0".to_string()),
        ("web_location", "444.8".to_string()),
    ];

    // Generate signed query string
    let signed_query = encode_wbi(params, wbi_keys);

    // Construct final URL
    let url = format!("{}?{}", DANMAKU_SERVER_CONF_URL, signed_query);

    // debug log the total request
    let response = client.get(url).headers(request_headers).send();
    log::debug!("init host server response: {:?}", response);
    let stat: StatusCode;
    let body: String;
    match response {
        Ok(resp) => {
            stat = resp.status();
            body = resp.text().unwrap();
            log::info!("init host server response body: {:?}", body);
        }
        Err(_) => {
            panic!("init host server failed");
        }
    }
    (stat, body)
}

// WBI signing constants and functions
const MIXIN_KEY_ENC_TAB: [usize; 64] = [
    46, 47, 18, 2, 53, 8, 23, 32, 15, 50, 10, 31, 58, 3, 45, 35, 27, 43, 5, 49, 33, 9, 42, 19, 29,
    28, 14, 39, 12, 38, 41, 13, 37, 48, 7, 16, 24, 55, 40, 61, 26, 17, 0, 1, 60, 51, 30, 4, 22, 25,
    54, 21, 56, 59, 6, 63, 57, 62, 11, 36, 20, 34, 44, 52,
];

#[derive(Deserialize)]
struct WbiImg {
    img_url: String,
    sub_url: String,
}

#[derive(Deserialize)]
struct Data {
    wbi_img: WbiImg,
}

#[derive(Deserialize)]
struct ResWbi {
    data: Data,
}

// 对 imgKey 和 subKey 进行字符顺序打乱编码
fn get_mixin_key(orig: &[u8]) -> String {
    MIXIN_KEY_ENC_TAB
        .iter()
        .take(32)
        .map(|&i| orig[i] as char)
        .collect::<String>()
}

fn get_url_encoded(s: &str) -> String {
    s.chars()
        .filter_map(|c| match c.is_ascii_alphanumeric() || "-_.~".contains(c) {
            true => Some(c.to_string()),
            false => {
                // 过滤 value 中的 "!'()*" 字符
                if "!'()*".contains(c) {
                    return None;
                }
                let encoded = c
                    .encode_utf8(&mut [0; 4])
                    .bytes()
                    .fold("".to_string(), |acc, b| acc + &format!("%{:02X}", b));
                Some(encoded)
            }
        })
        .collect::<String>()
}

// 为请求参数进行 wbi 签名
fn encode_wbi(params: Vec<(&str, String)>, (img_key, sub_key): (String, String)) -> String {
    let cur_time = match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(t) => t.as_secs(),
        Err(_) => panic!("SystemTime before UNIX EPOCH!"),
    };
    _encode_wbi(params, (img_key, sub_key), cur_time)
}

fn _encode_wbi(
    mut params: Vec<(&str, String)>,
    (img_key, sub_key): (String, String),
    timestamp: u64,
) -> String {
    let mixin_key = get_mixin_key((img_key + &sub_key).as_bytes());
    // 添加当前时间戳
    params.push(("wts", timestamp.to_string()));
    // 重新排序
    params.sort_by(|a, b| a.0.cmp(b.0));
    // 拼接参数
    let query = params
        .iter()
        .map(|(k, v)| format!("{}={}", get_url_encoded(k), get_url_encoded(v)))
        .collect::<Vec<_>>()
        .join("&");
    // 计算签名
    let web_sign = format!("{:x}", md5::compute(query.clone() + &mixin_key));
    // 返回最终的 query
    query + &format!("&w_rid={}", web_sign)
}

fn get_wbi_keys(headers: HeaderMap) -> Result<(String, String), reqwest::Error> {
    let client = reqwest::blocking::Client::builder()
        .https_only(true)
        .build()
        .unwrap();

    let mut request_headers = headers;
    request_headers.insert("user-agent", USER_AGENT.parse().unwrap());

    let response = client
        .get("https://api.bilibili.com/x/web-interface/nav")
        .headers(request_headers)
        .send()?;

    let res_wbi: ResWbi = response.json()?;
    Ok((
        take_filename(res_wbi.data.wbi_img.img_url).unwrap(),
        take_filename(res_wbi.data.wbi_img.sub_url).unwrap(),
    ))
}

fn take_filename(url: String) -> Option<String> {
    url.rsplit_once('/')
        .and_then(|(_, s)| s.rsplit_once('.'))
        .map(|(s, _)| s.to_string())
}

pub const UID_INIT_URL: &str = "https://api.bilibili.com/x/web-interface/nav";
pub const BUVID_INIT_URL: &str = "https://data.bilibili.com/v/";
pub const ROOM_INIT_URL: &str =
    "https://api.live.bilibili.com/xlive/web-room/v1/index/getInfoByRoom";
pub const DANMAKU_SERVER_CONF_URL: &str =
    "https://api.live.bilibili.com/xlive/web-room/v1/index/getDanmuInfo";
pub const USER_AGENT: &str =
    "Mozilla/5.0 (X11; Linux x86_64; rv:138.0) Gecko/20100101 Firefox/138.0";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_uid_url_constant() {
        assert!(UID_INIT_URL.contains("bilibili.com"));
    }

    #[test]
    fn test_take_filename() {
        assert_eq!(
            take_filename(
                "https://i0.hdslb.com/bfs/wbi/7cd084941338484aae1ad9425b84077c.png".to_string()
            ),
            Some("7cd084941338484aae1ad9425b84077c".to_string())
        );

        assert_eq!(
            take_filename(
                "https://i0.hdslb.com/bfs/wbi/4932caff0ff746eab6f01bf08b70ac45.png".to_string()
            ),
            Some("4932caff0ff746eab6f01bf08b70ac45".to_string())
        );

        // Test edge case with no extension
        assert_eq!(
            take_filename("https://example.com/path/file".to_string()),
            None
        );
    }

    #[test]
    fn test_encode_wbi_with_known_values() {
        let params = vec![
            ("foo", String::from("114")),
            ("bar", String::from("514")),
            ("zab", String::from("1919810")),
        ];

        let result = _encode_wbi(
            params,
            (
                "7cd084941338484aae1ad9425b84077c".to_string(),
                "4932caff0ff746eab6f01bf08b70ac45".to_string(),
            ),
            1702204169,
        );

        assert_eq!(
            result,
            "bar=514&foo=114&wts=1702204169&zab=1919810&w_rid=8f6f2b5b3d485fe1886cec6a0be8c5d4"
        );
    }

    #[test]
    fn test_encode_wbi_bilibili_danmu_params() {
        // Test with the actual Bilibili danmu parameters from the example
        let params = vec![
            ("id", String::from("24779526")),
            ("type", String::from("0")),
            ("web_location", String::from("444.8")),
        ];

        // Using the timestamp from the example URL (1748308267)
        let result = _encode_wbi(
            params,
            (
                "7cd084941338484aae1ad9425b84077c".to_string(),
                "4932caff0ff746eab6f01bf08b70ac45".to_string(),
            ),
            1748308267,
        );

        // The result should contain the correct parameters and w_rid
        assert!(result.contains("id=24779526"));
        assert!(result.contains("type=0"));
        assert!(result.contains("web_location=444.8"));
        assert!(result.contains("wts=1748308267"));
        assert!(result.contains("w_rid="));

        // Check the parameter order (should be alphabetical)
        let expected_order = "id=24779526&type=0&web_location=444.8&wts=1748308267&w_rid=";
        assert!(result.starts_with(expected_order));
    }

    #[test]
    fn test_wbi_signature_consistency() {
        // Test that the same parameters always generate the same signature
        let params1 = vec![
            ("id", String::from("24779526")),
            ("type", String::from("0")),
            ("web_location", String::from("444.8")),
        ];

        let params2 = vec![
            ("id", String::from("24779526")),
            ("type", String::from("0")),
            ("web_location", String::from("444.8")),
        ];

        let keys = (
            "7cd084941338484aae1ad9425b84077c".to_string(),
            "4932caff0ff746eab6f01bf08b70ac45".to_string(),
        );

        let timestamp = 1748308267;

        let result1 = _encode_wbi(params1, keys.clone(), timestamp);
        let result2 = _encode_wbi(params2, keys, timestamp);

        assert_eq!(result1, result2);
    }

    #[test]
    fn test_wbi_parameter_sorting() {
        // Test that parameters are properly sorted alphabetically
        let params = vec![
            ("z_param", String::from("last")),
            ("a_param", String::from("first")),
            ("m_param", String::from("middle")),
        ];

        let result = _encode_wbi(
            params,
            (
                "7cd084941338484aae1ad9425b84077c".to_string(),
                "4932caff0ff746eab6f01bf08b70ac45".to_string(),
            ),
            1748308267,
        );

        // Check that parameters appear in alphabetical order
        let parts: Vec<&str> = result.split('&').collect();
        assert!(parts[0].starts_with("a_param="));
        assert!(parts[1].starts_with("m_param="));
        assert!(parts[2].starts_with("wts="));
        assert!(parts[3].starts_with("z_param="));
        assert!(parts[4].starts_with("w_rid="));
    }

    #[test]
    fn test_correct_bilibili_url_signature() {
        // Test the exact URL from the working example:
        // "https://api.live.bilibili.com/xlive/web-room/v1/index/getDanmuInfo?id=24779526&type=0&web_location=444.8&wts=1748308267&w_rid=884cf361b8ad4e239b4a9dbbb7134679"
        // "https://api.live.bilibili.com/xlive/web-room/v1/index/getDanmuInfo?id=24779526&type=0&web_location=444.8&w_rid=d1e619744b4977f88ed67524a1f567cc&wts=1751072897"

        let params = vec![
            ("id", String::from("24779526")),
            ("type", String::from("0")),
            ("web_location", String::from("444.8")),
        ];

        let result = _encode_wbi(
            params,
            (
                "7cd084941338484aae1ad9425b84077c".to_string(),
                "4932caff0ff746eab6f01bf08b70ac45".to_string(),
            ),
            1751072897,
        );

        // Expected complete query string from working URL
        let expected = "id=24779526&type=0&web_location=444.8&wts=1751072897&w_rid=d1e619744b4977f88ed67524a1f567cc";
        assert_eq!(result, expected);

        // Extract and verify the w_rid specifically
        let w_rid = result.split("w_rid=").nth(1).unwrap();
        assert_eq!(w_rid, "d1e619744b4977f88ed67524a1f567cc");
    }

    #[test]
    fn test_second_bilibili_url_signature() {
        // Test the second URL example:
        // "https://api.live.bilibili.com/xlive/web-room/v1/index/getDanmuInfo?id=24779526&type=0&web_location=444.8&w_rid=fa20533eb27334ba6f2ec7263721319a&wts=1748311635"

        let params = vec![
            ("id", String::from("24779526")),
            ("type", String::from("0")),
            ("web_location", String::from("444.8")),
        ];

        let result = _encode_wbi(
            params,
            (
                "7cd084941338484aae1ad9425b84077c".to_string(),
                "4932caff0ff746eab6f01bf08b70ac45".to_string(),
            ),
            1748311635,
        );

        // Expected complete query string from working URL
        let expected = "id=24779526&type=0&web_location=444.8&wts=1748311635&w_rid=fa20533eb27334ba6f2ec7263721319a";
        assert_eq!(result, expected);

        // Extract and verify the w_rid specifically
        let w_rid = result.split("w_rid=").nth(1).unwrap();
        assert_eq!(w_rid, "fa20533eb27334ba6f2ec7263721319a");
    }

    #[test]
    fn test_third_bilibili_url_signature() {
        // Test the third URL example from README:
        // "https://api.live.bilibili.com/xlive/web-room/v1/index/getDanmuInfo?id=24779526&type=0&web_location=444.8&wts=1748312554&w_rid=30f250e8abd9effea1bcb88aab416507"

        let params = vec![
            ("id", String::from("24779526")),
            ("type", String::from("0")),
            ("web_location", String::from("444.8")),
        ];

        let result = _encode_wbi(
            params,
            (
                "7cd084941338484aae1ad9425b84077c".to_string(),
                "4932caff0ff746eab6f01bf08b70ac45".to_string(),
            ),
            1748312554,
        );

        // Expected complete query string from working URL
        let expected = "id=24779526&type=0&web_location=444.8&wts=1748312554&w_rid=30f250e8abd9effea1bcb88aab416507";
        assert_eq!(result, expected);

        // Extract and verify the w_rid specifically
        let w_rid = result.split("w_rid=").nth(1).unwrap();
        assert_eq!(w_rid, "30f250e8abd9effea1bcb88aab416507");
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
    fn test_from_login_url() {
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
