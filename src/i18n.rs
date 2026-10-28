//! The session language: overlay text and the desktop-entry names to prefer.

pub struct Strings {
    pub pinned: &'static str,
    pub open: &'static str,
    pub none: &'static str,
    pub browse_hint: &'static str,
    /// "{key}" is replaced by the chord, e.g. "Super+E".
    pub unassigned: &'static str,
    pub assign_section: &'static str,
    pub assign_hint: &'static str,
}

const EN: Strings = Strings {
    pinned: "Pinned",
    open: "Open",
    none: "None",
    browse_hint: "Press a letter or number to switch · Esc to close",
    unassigned: "{key} has no app yet",
    assign_section: "Press a number to pin that app to this key",
    assign_hint: "Esc to cancel",
};

const ZH_CN: Strings = Strings {
    pinned: "已固定",
    open: "已打开",
    none: "（无）",
    browse_hint: "按字母或数字切换 · Esc 关闭",
    unassigned: "{key} 还没有绑定应用",
    assign_section: "按数字把它绑定到这个按键",
    assign_hint: "Esc 取消",
};

const ZH_TW: Strings = Strings {
    pinned: "已固定",
    open: "已開啟",
    none: "（無）",
    browse_hint: "按字母或數字切換 · Esc 關閉",
    unassigned: "{key} 尚未綁定應用程式",
    assign_section: "按數字把它綁定到這個按鍵",
    assign_hint: "Esc 取消",
};

/// Preferred languages such as "zh_CN", most preferred first, following
/// gettext: LANGUAGE (unless the locale is C), then LC_ALL, LC_MESSAGES, LANG.
pub fn languages() -> Vec<String> {
    let var = |name: &str| std::env::var(name).ok().filter(|v| !v.is_empty());
    let locale = var("LC_ALL")
        .or_else(|| var("LC_MESSAGES"))
        .or_else(|| var("LANG"));
    parse_languages(var("LANGUAGE").as_deref(), locale.as_deref())
}

fn parse_languages(language: Option<&str>, locale: Option<&str>) -> Vec<String> {
    let strip = |s: &str| s.split(['.', '@']).next().unwrap_or("").to_string();
    let Some(locale) = locale
        .map(strip)
        .filter(|l| l != "C" && l != "POSIX" && !l.is_empty())
    else {
        return Vec::new();
    };
    let mut langs: Vec<String> = language
        .unwrap_or("")
        .split(':')
        .map(strip)
        .filter(|l| !l.is_empty())
        .collect();
    langs.push(locale);
    langs
}

/// Overlay text for the first language keydock has; English otherwise.
pub fn strings(languages: &[String]) -> &'static Strings {
    for lang in languages {
        let lang = lang.to_lowercase();
        if lang.starts_with("zh") {
            let traditional = ["_tw", "_hk", "_mo", "hant"]
                .iter()
                .any(|t| lang.contains(t));
            return if traditional { &ZH_TW } else { &ZH_CN };
        }
        if lang.starts_with("en") {
            return &EN;
        }
    }
    &EN
}

#[cfg(test)]
mod tests {
    use super::*;

    fn langs(language: &str, locale: &str) -> Vec<String> {
        parse_languages(Some(language), Some(locale))
    }

    #[test]
    fn language_list_comes_before_locale() {
        assert_eq!(langs("fr:en_US", "zh_CN.UTF-8"), ["fr", "en_US", "zh_CN"]);
        assert_eq!(langs("", "zh_TW.UTF-8"), ["zh_TW"]);
    }

    #[test]
    fn c_locale_ignores_language() {
        assert!(langs("zh_CN", "C").is_empty());
        assert!(parse_languages(Some("zh_CN"), None).is_empty());
    }

    #[test]
    fn picks_first_supported_language() {
        assert_eq!(strings(&langs("fr:zh_CN", "en_US.UTF-8")).open, ZH_CN.open);
        assert_eq!(strings(&langs("", "zh_HK.UTF-8")).open, ZH_TW.open);
        assert_eq!(strings(&langs("", "de_DE.UTF-8")).open, EN.open);
    }
}
