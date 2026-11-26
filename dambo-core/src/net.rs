use crate::auth::render_qr;
use crate::config::{SecurityMode, ServerConfig};
use crate::server::{connect_host, format_url_host, is_wildcard_host};
use serde::{Deserialize, Serialize};
use std::net::{IpAddr, Ipv4Addr};

/// Single discovered LAN (or explicit host) access entry with rendered SVG and terminal QR.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct LanUrlEntry {
    pub ip: String,
    pub url: String,
    pub qr_svg: String,
    #[serde(skip_serializing, default)]
    pub terminal_qr: String,
}

/// Aggregated LAN discovery and QR code payload for `/api/v1/system/lan-urls`, Tauri IPC, and CLI.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct LanUrlsPayload {
    pub local_only: bool,
    pub security_mode: SecurityMode,
    pub host: String,
    pub port: u16,
    pub path: String,
    pub local_url: String,
    pub urls: Vec<LanUrlEntry>,
}

/// Returns `true` for RFC 1918 LAN private IPv4 ranges (`192.168/16`, `10/8`, `172.16/12`),
/// explicitly excluding Docker's default bridge subnet `172.17.0.0/16`.
pub fn is_lan_private_ipv4(ip: Ipv4Addr) -> bool {
    let octets = ip.octets();
    if octets[0] == 192 && octets[1] == 168 {
        return true;
    }
    if octets[0] == 10 {
        return true;
    }
    if octets[0] == 172 && (16..=31).contains(&octets[1]) && octets[1] != 17 {
        return true;
    }
    false
}

fn lan_ipv4_priority(ip: Ipv4Addr) -> (u8, [u8; 4]) {
    let octets = ip.octets();
    let tier = match octets[0] {
        192 => 0,
        10 => 1,
        172 => 2,
        _ => 3,
    };
    (tier, octets)
}

/// Filter to RFC 1918 LAN private IPv4 addresses (excluding `172.17.0.0/16`),
/// sort by `192.168` -> `10` -> `172`, and deduplicate.
pub fn filter_and_sort_lan_ipv4s(ips: impl IntoIterator<Item = Ipv4Addr>) -> Vec<Ipv4Addr> {
    let mut filtered: Vec<Ipv4Addr> = ips
        .into_iter()
        .filter(|&ip| is_lan_private_ipv4(ip))
        .collect();
    filtered.sort_by_key(|&ip| lan_ipv4_priority(ip));
    filtered.dedup();
    filtered
}

/// Enumerate IPv4 addresses bound to UP, non-loopback network interfaces on the local host.
#[cfg(unix)]
pub fn enumerate_up_ipv4_addrs() -> Vec<Ipv4Addr> {
    struct IfAddrsGuard(*mut libc::ifaddrs);
    impl Drop for IfAddrsGuard {
        fn drop(&mut self) {
            if !self.0.is_null() {
                unsafe {
                    libc::freeifaddrs(self.0);
                }
            }
        }
    }

    let mut head: *mut libc::ifaddrs = std::ptr::null_mut();
    let rc = unsafe { libc::getifaddrs(&mut head) };
    if rc != 0 || head.is_null() {
        return Vec::new();
    }
    let _guard = IfAddrsGuard(head);

    let mut out = Vec::new();
    let mut cursor = head;
    while !cursor.is_null() {
        let ifa = unsafe { &*cursor };
        let flags = ifa.ifa_flags;
        let is_up = (flags & libc::IFF_UP as u32) != 0;
        let is_loopback = (flags & libc::IFF_LOOPBACK as u32) != 0;
        if is_up && !is_loopback && !ifa.ifa_addr.is_null() {
            let family = unsafe { i32::from((*ifa.ifa_addr).sa_family) };
            if family == libc::AF_INET {
                let sin =
                    unsafe { std::ptr::read_unaligned(ifa.ifa_addr as *const libc::sockaddr_in) };
                out.push(Ipv4Addr::from(sin.sin_addr.s_addr.to_ne_bytes()));
            }
        }
        cursor = ifa.ifa_next;
    }
    out
}

/// Enumerate IPv4 addresses bound to UP, non-loopback network interfaces on Windows.
#[cfg(windows)]
pub fn enumerate_up_ipv4_addrs() -> Vec<Ipv4Addr> {
    use windows_sys::Win32::Foundation::{ERROR_BUFFER_OVERFLOW, NO_ERROR};
    use windows_sys::Win32::NetworkManagement::IpHelper::{
        GAA_FLAG_SKIP_ANYCAST, GAA_FLAG_SKIP_DNS_SERVER, GAA_FLAG_SKIP_MULTICAST,
        GetAdaptersAddresses, IF_TYPE_SOFTWARE_LOOPBACK, IP_ADAPTER_ADDRESSES_LH,
    };
    use windows_sys::Win32::NetworkManagement::Ndis::IfOperStatusUp;
    use windows_sys::Win32::Networking::WinSock::{AF_INET, SOCKADDR_IN};

    let flags = GAA_FLAG_SKIP_ANYCAST | GAA_FLAG_SKIP_MULTICAST | GAA_FLAG_SKIP_DNS_SERVER;
    let mut buf_len: u32 = 16 * 1024;
    let mut buf: Vec<u64> = Vec::new();

    let mut status = ERROR_BUFFER_OVERFLOW;
    for _ in 0..4 {
        buf.resize((buf_len as usize).div_ceil(std::mem::size_of::<u64>()), 0);
        let ptr = buf.as_mut_ptr() as *mut IP_ADAPTER_ADDRESSES_LH;
        status = unsafe {
            GetAdaptersAddresses(AF_INET as u32, flags, std::ptr::null(), ptr, &mut buf_len)
        };
        if status == NO_ERROR {
            break;
        }
        if status != ERROR_BUFFER_OVERFLOW || buf_len == 0 {
            return Vec::new();
        }
    }
    if status != NO_ERROR || buf.is_empty() {
        return Vec::new();
    }

    let mut out = Vec::new();
    let mut adapter_ptr = buf.as_ptr() as *const IP_ADAPTER_ADDRESSES_LH;
    while !adapter_ptr.is_null() {
        let adapter = unsafe { &*adapter_ptr };
        if adapter.OperStatus == IfOperStatusUp && adapter.IfType != IF_TYPE_SOFTWARE_LOOPBACK {
            let mut unicast_ptr = adapter.FirstUnicastAddress;
            while !unicast_ptr.is_null() {
                let unicast = unsafe { &*unicast_ptr };
                let sa = unicast.Address.lpSockaddr;
                if !sa.is_null() && unsafe { (*sa).sa_family } == AF_INET {
                    let sin = unsafe { std::ptr::read_unaligned(sa as *const SOCKADDR_IN) };
                    let s_addr = unsafe { sin.sin_addr.S_un.S_addr };
                    out.push(Ipv4Addr::from(s_addr.to_ne_bytes()));
                }
                unicast_ptr = unicast.Next;
            }
        }
        adapter_ptr = adapter.Next;
    }
    out
}

/// Fallback interface enumerator for unsupported platforms.
#[cfg(not(any(unix, windows)))]
pub fn enumerate_up_ipv4_addrs() -> Vec<Ipv4Addr> {
    Vec::new()
}

/// Determine LAN-reachable hosts and `local_only` status for a configured `[server].host`.
pub fn discover_lan_hosts(config_host: &str) -> (Vec<String>, bool) {
    let trimmed = config_host.trim();
    let unbracketed = trimmed.trim_start_matches('[').trim_end_matches(']');

    if unbracketed.is_empty()
        || unbracketed.eq_ignore_ascii_case("localhost")
        || unbracketed
            .parse::<IpAddr>()
            .is_ok_and(|ip| ip.is_loopback())
    {
        return (Vec::new(), true);
    }

    if is_wildcard_host(trimmed) {
        let ips = filter_and_sort_lan_ipv4s(enumerate_up_ipv4_addrs());
        if ips.is_empty() {
            return (Vec::new(), true);
        }
        return (ips.into_iter().map(|ip| ip.to_string()).collect(), false);
    }

    (vec![unbracketed.to_string()], false)
}

/// Normalize a requested relative HTTP path for LAN URL construction (defaults to `"/"`).
pub fn normalize_lan_path(raw: Option<&str>) -> String {
    let clean = raw.unwrap_or("/").split('#').next().unwrap_or("/").trim();
    if clean.is_empty() {
        return "/".to_string();
    }
    let stripped: String = clean.chars().filter(|c| !c.is_control()).collect();
    let trimmed = stripped.trim_start_matches('/');
    if trimmed.is_empty() {
        "/".to_string()
    } else {
        format!("/{}", trimmed)
    }
}

fn percent_encode_fragment(raw: &str) -> String {
    use std::fmt::Write;
    let mut out = String::with_capacity(raw.len());
    for byte in raw.as_bytes() {
        let ch = *byte as char;
        if byte.is_ascii_alphanumeric() || matches!(ch, '-' | '.' | '_' | '~') {
            out.push(ch);
        } else {
            let _ = write!(&mut out, "%{:02X}", byte);
        }
    }
    out
}

fn build_target_url(
    host: &str,
    port: u16,
    normalized_path: &str,
    fragment_token: Option<&str>,
) -> String {
    let url_host = format_url_host(host);
    match fragment_token {
        Some(tok) => format!(
            "http://{}:{}{}#t={}",
            url_host,
            port,
            normalized_path,
            percent_encode_fragment(tok)
        ),
        None => format!("http://{}:{}{}", url_host, port, normalized_path),
    }
}

/// Build the full LAN URL & QR payload using discovered local network interfaces.
pub fn build_lan_urls(
    cfg: &ServerConfig,
    path: Option<&str>,
    include_admin_token: bool,
) -> LanUrlsPayload {
    let (hosts, local_only) = discover_lan_hosts(&cfg.host);
    build_lan_urls_with_hosts(cfg, path, include_admin_token, hosts, local_only)
}

/// Deterministic builder accepting pre-resolved hosts for unit testing and custom discovery.
pub fn build_lan_urls_with_hosts(
    cfg: &ServerConfig,
    path: Option<&str>,
    include_admin_token: bool,
    hosts: Vec<String>,
    local_only: bool,
) -> LanUrlsPayload {
    let normalized_path = normalize_lan_path(path);
    let fragment_token = if include_admin_token && cfg.security_mode.is_strict() {
        cfg.trimmed_admin_token()
    } else {
        None
    };

    let local_host = connect_host(&cfg.host);
    let local_url = build_target_url(local_host, cfg.port, &normalized_path, fragment_token);

    let mut urls = Vec::with_capacity(hosts.len());
    for ip in hosts {
        let url = build_target_url(&ip, cfg.port, &normalized_path, fragment_token);
        let (qr_svg, terminal_qr) = match render_qr(&url) {
            Ok((svg, term)) => (svg, term),
            Err(e) => {
                log::warn!("Failed to render LAN QR code for '{}': {}", url, e);
                (String::new(), String::new())
            }
        };
        urls.push(LanUrlEntry {
            ip,
            url,
            qr_svg,
            terminal_qr,
        });
    }

    LanUrlsPayload {
        local_only,
        security_mode: cfg.security_mode,
        host: cfg.host.clone(),
        port: cfg.port,
        path: normalized_path,
        local_url,
        urls,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_private_ipv4_filtering_and_priority_sorting() {
        let inputs = vec![
            Ipv4Addr::new(8, 8, 8, 8),       // public -> excluded
            Ipv4Addr::new(127, 0, 0, 1),     // loopback -> excluded
            Ipv4Addr::new(169, 254, 10, 5),  // link-local -> excluded
            Ipv4Addr::new(100, 64, 1, 2),    // CGNAT -> excluded
            Ipv4Addr::new(172, 17, 0, 1),    // Docker default bridge -> excluded
            Ipv4Addr::new(172, 17, 255, 2),  // Docker default bridge -> excluded
            Ipv4Addr::new(172, 15, 0, 1),    // outside 172.16/12 -> excluded
            Ipv4Addr::new(172, 32, 0, 1),    // outside 172.16/12 -> excluded
            Ipv4Addr::new(172, 20, 10, 4),   // 172.16/12 (non-17) -> priority 2
            Ipv4Addr::new(10, 0, 2, 15),     // 10/8 -> priority 1
            Ipv4Addr::new(192, 168, 50, 10), // 192.168/16 -> priority 0
            Ipv4Addr::new(192, 168, 1, 88),  // 192.168/16 -> priority 0 (sorts before .50)
            Ipv4Addr::new(192, 168, 1, 88),  // duplicate -> deduped
            Ipv4Addr::new(172, 16, 0, 9),    // 172.16/12 -> priority 2 (before .20)
            Ipv4Addr::new(172, 31, 255, 10), // 172.16/12 upper bound -> priority 2
        ];

        let sorted = filter_and_sort_lan_ipv4s(inputs);
        assert_eq!(
            sorted,
            vec![
                Ipv4Addr::new(192, 168, 1, 88),
                Ipv4Addr::new(192, 168, 50, 10),
                Ipv4Addr::new(10, 0, 2, 15),
                Ipv4Addr::new(172, 16, 0, 9),
                Ipv4Addr::new(172, 20, 10, 4),
                Ipv4Addr::new(172, 31, 255, 10),
            ]
        );
    }

    #[test]
    fn test_discover_lan_hosts_dispatch_modes() {
        assert_eq!(discover_lan_hosts("127.0.0.1"), (Vec::new(), true));
        assert_eq!(discover_lan_hosts("localhost"), (Vec::new(), true));
        assert_eq!(discover_lan_hosts("::1"), (Vec::new(), true));
        assert_eq!(discover_lan_hosts("[::1]"), (Vec::new(), true));
        assert_eq!(
            discover_lan_hosts("192.168.31.100"),
            (vec!["192.168.31.100".to_string()], false)
        );
        assert_eq!(
            discover_lan_hosts("[fe80::1]"),
            (vec!["fe80::1".to_string()], false)
        );
    }

    #[test]
    fn test_normalize_lan_path_and_build_urls_with_modes() {
        assert_eq!(normalize_lan_path(None), "/");
        assert_eq!(normalize_lan_path(Some("")), "/");
        assert_eq!(normalize_lan_path(Some("///list/")), "/list/");
        assert_eq!(
            normalize_lan_path(Some("game24/m?foo=1#old")),
            "/game24/m?foo=1"
        );

        let mut open_cfg = ServerConfig {
            host: "0.0.0.0".to_string(),
            port: 3838,
            security_mode: SecurityMode::Open,
            admin_token: Some("tok#123".to_string()),
            ..Default::default()
        };

        let open_payload = build_lan_urls_with_hosts(
            &open_cfg,
            Some("/game24/m"),
            true,
            vec!["192.168.1.20".to_string(), "10.0.0.5".to_string()],
            false,
        );
        assert!(!open_payload.local_only);
        assert_eq!(open_payload.security_mode, SecurityMode::Open);
        assert_eq!(open_payload.path, "/game24/m");
        assert_eq!(open_payload.local_url, "http://localhost:3838/game24/m");
        assert_eq!(open_payload.urls.len(), 2);
        assert_eq!(
            open_payload.urls[0].url,
            "http://192.168.1.20:3838/game24/m"
        );
        assert!(open_payload.urls[0].qr_svg.contains("<svg"));
        assert!(!open_payload.urls[0].terminal_qr.is_empty());

        // Strict mode appends `#t=<admin_token>` when `include_admin_token` is true
        open_cfg.security_mode = SecurityMode::Strict;
        let strict_with_tok = build_lan_urls_with_hosts(
            &open_cfg,
            None,
            true,
            vec!["192.168.1.20".to_string()],
            false,
        );
        assert_eq!(
            strict_with_tok.local_url,
            "http://localhost:3838/#t=tok%23123"
        );
        assert_eq!(
            strict_with_tok.urls[0].url,
            "http://192.168.1.20:3838/#t=tok%23123"
        );
        assert!(strict_with_tok.urls[0].qr_svg.contains("<svg"));

        // Strict mode without token permission omits `#t=`
        let strict_no_tok = build_lan_urls_with_hosts(
            &open_cfg,
            None,
            false,
            vec!["192.168.1.20".to_string()],
            false,
        );
        assert_eq!(strict_no_tok.urls[0].url, "http://192.168.1.20:3838/");

        // Loopback host returns local_only = true and empty urls
        let loopback_cfg = ServerConfig {
            host: "127.0.0.1".to_string(),
            port: 3939,
            ..Default::default()
        };
        let loopback_payload = build_lan_urls(&loopback_cfg, None, true);
        assert!(loopback_payload.local_only);
        assert!(loopback_payload.urls.is_empty());
        assert_eq!(loopback_payload.local_url, "http://127.0.0.1:3939/");
    }
}
