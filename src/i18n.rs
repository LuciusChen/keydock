//! Overlay text in the session language (English, Simplified or Traditional Chinese).

pub struct Strings {
    pub pinned: &'static str,
    pub open: &'static str,
    pub none: &'static str,
    pub browse_hint: &'static str,
    /// "{label}+{key}" is substituted.
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

/// Picks strings from LANGUAGE, LC_ALL, LC_MESSAGES and LANG, in that order,
/// using the first language keydock has text for; English otherwise.
pub fn current() -> &'static Strings {
    let var = |name: &str| std::env::var(name).ok().filter(|v| !v.is_empty());
    let mut candidates: Vec<String> = Vec::new();
    // GNU gettext ignores LANGUAGE when the locale is "C".
    let locale = var("LC_ALL").or_else(|| var("LC_MESSAGES")).or_else(|| var("LANG"));
    if locale.as_deref().is_some_and(|l| l != "C" && l != "POSIX") {
        if let Some(list) = var("LANGUAGE") {
            candidates.extend(list.split(':').map(str::to_string));
        }
    }
    candidates.extend(locale);
    for c in candidates {
        let c = c.to_lowercase();
        if c.starts_with("zh") {
            let traditional = ["_tw", "_hk", "_mo", "hant"].iter().any(|t| c.contains(t));
            return if traditional { &ZH_TW } else { &ZH_CN };
        }
        if c.starts_with("en") {
            return &EN;
        }
    }
    &EN
}
