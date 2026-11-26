use dambo_core::state::normalize_locale;
use serde::Serialize;
use std::collections::HashMap;
use std::sync::OnceLock;

pub const LOCALE_ZH_CN: &str = "zh-CN";
pub const LOCALE_JA: &str = "ja";
pub const LOCALE_EN: &str = "en";

pub const SUPPORTED_LOCALES: &[(&str, &str)] = &[
    (LOCALE_ZH_CN, "简体中文"),
    (LOCALE_JA, "日本語"),
    (LOCALE_EN, "English"),
];

const ZH_CN_JSON: &str = include_str!("../locales/zh-CN.json");
const JA_JSON: &str = include_str!("../locales/ja.json");
const EN_JSON: &str = include_str!("../locales/en.json");

fn parse_dict(raw: &str, name: &str) -> HashMap<String, String> {
    serde_json::from_str(raw)
        .unwrap_or_else(|e| panic!("Invalid embedded locale dictionary {}: {}", name, e))
}

fn dict_zh_cn() -> &'static HashMap<String, String> {
    static MAP: OnceLock<HashMap<String, String>> = OnceLock::new();
    MAP.get_or_init(|| parse_dict(ZH_CN_JSON, "zh-CN.json"))
}

fn dict_ja() -> &'static HashMap<String, String> {
    static MAP: OnceLock<HashMap<String, String>> = OnceLock::new();
    MAP.get_or_init(|| parse_dict(JA_JSON, "ja.json"))
}

fn dict_en() -> &'static HashMap<String, String> {
    static MAP: OnceLock<HashMap<String, String>> = OnceLock::new();
    MAP.get_or_init(|| parse_dict(EN_JSON, "en.json"))
}

#[derive(Clone, Debug, Serialize)]
pub struct I18nPayload {
    pub locale: String,
    pub messages: HashMap<String, String>,
}

/// Detect the operating system UI language (`zh*` -> `"zh-CN"`, `ja*` -> `"ja"`, fallback `"en"`).
pub fn detect_system_locale() -> &'static str {
    for var in ["LC_ALL", "LC_MESSAGES", "LANGUAGE", "LANG"] {
        if let Ok(val) = std::env::var(var)
            && let Some(norm) = normalize_locale(&val)
        {
            return norm;
        }
    }

    #[cfg(target_os = "macos")]
    {
        if let Ok(output) = std::process::Command::new("defaults")
            .args(["read", "-g", "AppleLanguages"])
            .output()
            && output.status.success()
        {
            let stdout = String::from_utf8_lossy(&output.stdout);
            for line in stdout.lines() {
                let cleaned = line
                    .trim()
                    .trim_matches(|c| c == '"' || c == ',' || c == '(' || c == ')');
                if let Some(norm) = normalize_locale(cleaned) {
                    return norm;
                }
            }
        }
    }

    LOCALE_EN
}

/// Resolve active locale preferring persisted `stored` override, otherwise OS detection.
pub fn resolve_locale(stored: Option<&str>) -> &'static str {
    stored
        .and_then(normalize_locale)
        .unwrap_or_else(detect_system_locale)
}

pub fn messages(locale: &str) -> &'static HashMap<String, String> {
    match normalize_locale(locale).unwrap_or(LOCALE_EN) {
        LOCALE_ZH_CN => dict_zh_cn(),
        LOCALE_JA => dict_ja(),
        _ => dict_en(),
    }
}

pub fn t(locale: &str, key: &str) -> String {
    messages(locale)
        .get(key)
        .or_else(|| dict_en().get(key))
        .cloned()
        .unwrap_or_else(|| key.to_string())
}

pub fn t_fmt(locale: &str, key: &str, args: &[(&str, &str)]) -> String {
    let mut out = t(locale, key);
    for &(name, val) in args {
        let placeholder = format!("{{{}}}", name);
        out = out.replace(&placeholder, val);
    }
    out
}

pub fn build_payload(locale: &str) -> I18nPayload {
    let norm = normalize_locale(locale).unwrap_or(LOCALE_EN);
    I18nPayload {
        locale: norm.to_string(),
        messages: messages(norm).clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    #[test]
    fn test_locale_dictionaries_have_identical_non_empty_keys() {
        let zh: BTreeSet<&String> = dict_zh_cn().keys().collect();
        let ja: BTreeSet<&String> = dict_ja().keys().collect();
        let en: BTreeSet<&String> = dict_en().keys().collect();

        assert_eq!(zh, ja, "zh-CN.json and ja.json key sets must match");
        assert_eq!(zh, en, "zh-CN.json and en.json key sets must match");
        assert!(!zh.is_empty());

        for (name, map) in [
            ("zh-CN", dict_zh_cn()),
            ("ja", dict_ja()),
            ("en", dict_en()),
        ] {
            for (k, v) in map {
                assert!(
                    !v.trim().is_empty(),
                    "Locale {} has empty translation for key '{}'",
                    name,
                    k
                );
            }
        }
    }

    #[test]
    fn test_t_and_t_fmt_substitution() {
        let msg = t_fmt(
            LOCALE_ZH_CN,
            "tray.tooltip_live",
            &[("room", "21452505(主播A)")],
        );
        assert_eq!(msg, "Dambo · 21452505(主播A) · 直播中");

        let ja_msg = t_fmt(LOCALE_JA, "plugin.trigger_label", &[("count", "3")]);
        assert_eq!(ja_msg, "プラグイン (3)");

        assert_eq!(t(LOCALE_ZH_CN, "tray.plugin_menu"), "插件");
        assert_eq!(t(LOCALE_JA, "tray.plugin_menu"), "プラグイン");
        assert_eq!(t(LOCALE_EN, "tray.plugin_menu"), "Plugins");

        assert_eq!(t(LOCALE_ZH_CN, "tray.language_menu"), "语言");
        assert_eq!(t(LOCALE_JA, "tray.language_menu"), "言語");
        assert_eq!(t(LOCALE_EN, "tray.language_menu"), "Language");

        assert_eq!(t(LOCALE_ZH_CN, "tray.plugin_close_desktop"), "关闭桌面");
        assert_eq!(
            t(LOCALE_JA, "tray.plugin_close_desktop"),
            "デスクトップを閉じる"
        );
        assert_eq!(t(LOCALE_EN, "tray.plugin_close_desktop"), "Close Desktop");

        assert_eq!(t(LOCALE_ZH_CN, "plugin.close_desktop"), "关闭桌面");
        assert_eq!(t(LOCALE_JA, "plugin.close_desktop"), "デスクトップを閉じる");
        assert_eq!(t(LOCALE_EN, "plugin.close_desktop"), "Close Desktop");

        assert_eq!(t(LOCALE_ZH_CN, "overlay.pin_title"), "置顶窗口");
        assert_eq!(t(LOCALE_JA, "overlay.pin_title"), "常に最前面に表示");
        assert_eq!(t(LOCALE_EN, "overlay.pin_title"), "Pin on top");

        assert_eq!(t(LOCALE_ZH_CN, "overlay.unpin_title"), "取消置顶");
        assert_eq!(t(LOCALE_JA, "overlay.unpin_title"), "最前面表示を解除");
        assert_eq!(t(LOCALE_EN, "overlay.unpin_title"), "Unpin from top");

        assert_eq!(t(LOCALE_ZH_CN, "panel.connect"), "进入");
        assert_eq!(t(LOCALE_JA, "panel.connect"), "入室");
        assert_eq!(t(LOCALE_EN, "panel.connect"), "Enter");

        assert_eq!(t(LOCALE_ZH_CN, "panel.disconnect"), "离开");
        assert_eq!(t(LOCALE_JA, "panel.disconnect"), "退室");
        assert_eq!(t(LOCALE_EN, "panel.disconnect"), "Leave");

        assert_eq!(t(LOCALE_ZH_CN, "panel.connecting"), "进房中");
        assert_eq!(t(LOCALE_JA, "panel.connecting"), "入室中");
        assert_eq!(t(LOCALE_EN, "panel.connecting"), "Entering");

        assert_eq!(t(LOCALE_ZH_CN, "console.trigger_label"), "控制台");
        assert_eq!(t(LOCALE_JA, "console.trigger_label"), "コンソール");
        assert_eq!(t(LOCALE_EN, "console.trigger_label"), "Console");

        assert_eq!(t(LOCALE_ZH_CN, "tray.open_admin"), "打开控制面板");
        assert_eq!(t(LOCALE_JA, "tray.open_admin"), "コントロールパネルを開く");
        assert_eq!(t(LOCALE_EN, "tray.open_admin"), "Open Control Panel");

        assert_eq!(t(LOCALE_ZH_CN, "panel.pin_title"), "置顶窗口");
        assert_eq!(t(LOCALE_JA, "panel.pin_title"), "最前面に固定");
        assert_eq!(t(LOCALE_EN, "panel.pin_title"), "Pin on top");

        assert_eq!(t(LOCALE_ZH_CN, "panel.unpin_title"), "取消置顶");
        assert_eq!(t(LOCALE_JA, "panel.unpin_title"), "最前面固定を解除");
        assert_eq!(t(LOCALE_EN, "panel.unpin_title"), "Unpin window");

        assert_eq!(t(LOCALE_ZH_CN, "more.language"), "🌐 语言");
        assert_eq!(t(LOCALE_JA, "more.language"), "🌐 言語");
        assert_eq!(t(LOCALE_EN, "more.language"), "🌐 Language");

        assert!(!dict_zh_cn().contains_key("tray.open_portal"));

        assert_eq!(
            t_fmt(LOCALE_ZH_CN, "console.pending_alert", &[("count", "2")]),
            "2 条跨插件权限待审批"
        );
        assert_eq!(
            t_fmt(LOCALE_EN, "console.pending_alert", &[("count", "2")]),
            "2 cross-plugin permission request(s) pending"
        );

        assert!(!dict_zh_cn().contains_key("plugin.hot_reload"));
        assert_eq!(t(LOCALE_EN, "missing.key"), "missing.key");
    }
}
