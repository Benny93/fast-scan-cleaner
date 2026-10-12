/// (code, native name) of every supported UI language.
pub const LANGS: [(&str, &str); 6] = [
    ("en", "English"),
    ("de", "Deutsch"),
    ("fr", "Français"),
    ("es", "Español"),
    ("zh", "中文"),
    ("ja", "日本語"),
];

/// Human-readable size with the locale's decimal separator.
/// Decimal units (1 GB = 10^9 bytes), as Finder and most file managers show.
pub fn size(bytes: u64, locale: &str) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    if bytes < 1000 {
        return format!("{bytes} B");
    }
    let mut v = bytes as f64;
    let mut unit = 0;
    while v >= 1000.0 && unit < UNITS.len() - 1 {
        v /= 1000.0;
        unit += 1;
    }
    let s = format!("{v:.1} {}", UNITS[unit]);
    if decimal_comma(locale) { s.replace('.', ",") } else { s }
}

fn decimal_comma(locale: &str) -> bool {
    matches!(locale.get(..2), Some("de" | "fr" | "es"))
}

/// Whole number with the locale's thousands separator.
pub fn count(n: u64, locale: &str) -> String {
    let sep = match locale.get(..2) {
        Some("de" | "es") => ".",
        Some("fr") => "\u{202f}",
        _ => ",",
    };
    let digits = n.to_string();
    let mut out = String::new();
    for (i, ch) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push_str(sep);
        }
        out.push(ch);
    }
    out
}

/// Locale key and number for "how long ago".
pub fn ago(secs: u64) -> (&'static str, u64) {
    match secs {
        0..60 => ("time_now", 0),
        60..3600 => ("time_minutes", secs / 60),
        3600..86_400 => ("time_hours", secs / 3600),
        86_400..172_800 => ("time_days_one", 1),
        _ => ("time_days", secs / 86_400),
    }
}

/// Percentage with one decimal and the locale's decimal separator (no % sign).
pub fn pct(x: f64, locale: &str) -> String {
    let s = format!("{x:.1}");
    if decimal_comma(locale) { s.replace('.', ",") } else { s }
}

/// The UI language: the explicit setting if supported, else the system language, else English.
pub fn pick_locale(setting: Option<&str>, system: Option<String>) -> &'static str {
    let supported = |code: &str| LANGS.iter().map(|(c, _)| *c).find(|c| code.get(..2) == Some(*c));
    setting
        .and_then(supported)
        .or_else(|| system.as_deref().and_then(supported))
        .unwrap_or("en")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    #[test]
    fn sizes() {
        assert_eq!(size(0, "en"), "0 B");
        assert_eq!(size(999, "en"), "999 B");
        assert_eq!(size(1_000, "en"), "1.0 KB");
        assert_eq!(size(1_500_000_000, "en"), "1.5 GB");
        assert_eq!(size(1_500_000_000, "de"), "1,5 GB");
        assert_eq!(size(2_000_000_000_000, "fr"), "2,0 TB");
        assert_eq!(size(u64::MAX, "en"), "18446744.1 TB");
    }

    #[test]
    fn counts_group_thousands_per_locale() {
        assert_eq!(count(0, "en"), "0");
        assert_eq!(count(999, "en"), "999");
        assert_eq!(count(2956, "en"), "2,956");
        assert_eq!(count(1_234_567, "de"), "1.234.567");
        assert_eq!(count(2956, "es"), "2.956");
        assert_eq!(count(2956, "fr"), "2\u{202f}956");
        assert_eq!(count(2956, "ja"), "2,956");
    }

    #[test]
    fn ago_buckets() {
        assert_eq!(ago(5), ("time_now", 0));
        assert_eq!(ago(59), ("time_now", 0));
        assert_eq!(ago(60), ("time_minutes", 1));
        assert_eq!(ago(3599), ("time_minutes", 59));
        assert_eq!(ago(7200), ("time_hours", 2));
        assert_eq!(ago(86_400 + 5), ("time_days_one", 1));
        assert_eq!(ago(3 * 86400 + 5), ("time_days", 3));
    }

    #[test]
    fn percentages_use_locale_separator() {
        assert_eq!(pct(97.24, "en"), "97.2");
        assert_eq!(pct(97.24, "de"), "97,2");
        assert_eq!(pct(2.75, "fr"), "2,8");
        assert_eq!(pct(0.0, "zh"), "0.0");
    }

    #[test]
    fn locale_choice() {
        assert_eq!(pick_locale(Some("ja"), Some("de-DE".into())), "ja");
        assert_eq!(pick_locale(None, Some("de-DE".into())), "de");
        assert_eq!(pick_locale(None, Some("zh-Hans-CN".into())), "zh");
        assert_eq!(pick_locale(None, Some("pt-BR".into())), "en");
        assert_eq!(pick_locale(Some("xx"), None), "en");
        assert_eq!(pick_locale(None, None), "en");
    }

    fn keys(code: &str) -> BTreeSet<String> {
        let path = format!("{}/locales/{code}.yml", env!("CARGO_MANIFEST_DIR"));
        std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("{path}: {e}"))
            .lines()
            .filter(|l| !l.trim().is_empty() && !l.starts_with('#'))
            .map(|l| l.split(':').next().unwrap().trim().to_string())
            .collect()
    }

    #[test]
    fn every_locale_has_every_key() {
        let en = keys("en");
        for (code, _) in LANGS {
            let other = keys(code);
            let missing: Vec<_> = en.difference(&other).collect();
            let extra: Vec<_> = other.difference(&en).collect();
            assert!(missing.is_empty() && extra.is_empty(), "{code}: missing {missing:?}, extra {extra:?}");
        }
    }
}
