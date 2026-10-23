//! 用户与会话的小工具：邮箱规范化、组织 slug、会话的设备标签。

/// 邮箱：去首尾空白、转小写，检查形状（不做 RFC 5322 的完整解析，也不发验证邮件）。
/// 返回规范化后的邮箱，或英文原因。数据库列是 `citext`，唯一性本来就不区分大小写；这里转小写是为了让
/// 登录限流的账号键与存储一致。
pub fn normalize_email(raw: &str) -> Result<String, &'static str> {
    let e = raw.trim();
    if e.len() < 3 || e.len() > 320 {
        return Err("the email must be 3 to 320 characters");
    }
    let bad = |c: char| c.is_whitespace() || c.is_control() || "<>()[]\\,;:\"".contains(c);
    let Some((local, domain)) = e.rsplit_once('@') else {
        return Err("the email must contain an `@`");
    };
    if local.is_empty()
        || local.len() > 64
        || local.contains('@')
        || domain.len() < 3
        || !domain.contains('.')
        || domain.starts_with('.')
        || domain.ends_with('.')
        || domain.contains("..")
        || e.chars().any(bad)
    {
        return Err("the email address is not valid");
    }
    Ok(e.to_lowercase())
}

/// 组织名（1 到 100 个字符，`organizations.name` 的 CHECK）：去首尾空白，不许有控制字符。
pub fn normalize_org_name(raw: &str) -> Result<String, &'static str> {
    let n = raw.trim();
    let chars = n.chars().count();
    if !(1..=100).contains(&chars) || n.chars().any(char::is_control) {
        return Err("the organization name must be 1 to 100 characters");
    }
    Ok(n.to_owned())
}

/// 由组织名得到 slug：只留 ASCII 字母数字，其余折成 `-`，最长 40 个字符；什么都没剩下就是 `org`。
pub fn slugify(name: &str) -> String {
    let mut out = String::new();
    for c in name.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.ends_with('-') {
            out.push('-');
        }
    }
    let s = out.trim_matches('-');
    let s: String = s.chars().take(40).collect();
    let s = s.trim_matches('-').to_owned();
    if s.is_empty() { "org".to_owned() } else { s }
}

/// 设备标签（会话列表里显示）：客户端给的优先，限 100 个字符并去掉控制字符；没给就从 `User-Agent`
/// 粗略归纳成「浏览器 on 系统」。永远不存完整的 UA。
pub fn device_label(client_supplied: Option<&str>, user_agent: Option<&str>) -> Option<String> {
    if let Some(l) = client_supplied {
        let cleaned: String = l.chars().filter(|c| !c.is_control()).take(100).collect();
        let cleaned = cleaned.trim();
        if !cleaned.is_empty() {
            return Some(cleaned.to_owned());
        }
    }
    let ua = user_agent?;
    // 顺序有讲究：Edge、Opera 的 UA 里也有 Chrome，Chrome 的 UA 里也有 Safari
    let browser = [
        ("Edg/", "Edge"),
        ("EdgA/", "Edge"),
        ("EdgiOS/", "Edge"),
        ("OPR/", "Opera"),
        ("Firefox/", "Firefox"),
        ("FxiOS/", "Firefox"),
        ("Chrome/", "Chrome"),
        ("CriOS/", "Chrome"),
        ("Safari/", "Safari"),
    ]
    .iter()
    .find(|(needle, _)| ua.contains(needle))
    .map(|(_, name)| *name);
    let os = [
        ("iPhone", "iOS"),
        ("iPad", "iPadOS"),
        ("Android", "Android"),
        ("Windows", "Windows"),
        ("Mac OS X", "macOS"),
        ("CrOS", "ChromeOS"),
        ("Linux", "Linux"),
    ]
    .iter()
    .find(|(needle, _)| ua.contains(needle))
    .map(|(_, name)| *name);
    match (browser, os) {
        (Some(b), Some(o)) => Some(format!("{b} on {o}")),
        (Some(b), None) => Some(b.to_owned()),
        (None, Some(o)) => Some(o.to_owned()),
        (None, None) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn emails_are_trimmed_lowercased_and_shape_checked() {
        assert_eq!(
            normalize_email("  Alice@Example.COM ").unwrap(),
            "alice@example.com"
        );
        assert_eq!(
            normalize_email("a+tag@sub.example.org").unwrap(),
            "a+tag@sub.example.org"
        );
        for bad in [
            "",
            "a@b",
            "@example.com",
            "alice@",
            "alice",
            "a b@example.com",
            "a@@example.com",
            "alice@.example.com",
            "alice@example.com.",
            "alice@exa..mple.com",
            "al<ice@example.com",
            "ali\nce@example.com",
            &format!("{}@example.com", "a".repeat(65)),
            &format!("a@{}.com", "b".repeat(320)),
        ] {
            assert!(normalize_email(bad).is_err(), "{bad:?}");
        }
        // 首尾的空白（含换行）被去掉
        assert_eq!(
            normalize_email("alice@example.com\n").unwrap(),
            "alice@example.com"
        );
    }

    #[test]
    fn org_names_follow_the_check_constraint() {
        assert_eq!(normalize_org_name("  Acme Inc ").unwrap(), "Acme Inc");
        assert!(normalize_org_name("").is_err());
        assert!(normalize_org_name("   ").is_err());
        assert!(normalize_org_name(&"x".repeat(101)).is_err());
        assert!(normalize_org_name(&"x".repeat(100)).is_ok());
        assert!(normalize_org_name("a\u{0}b").is_err());
        assert!(normalize_org_name(&"组".repeat(100)).is_ok());
    }

    #[test]
    fn slugs_are_ascii_lowercase_with_single_dashes() {
        assert_eq!(slugify("Acme Inc."), "acme-inc");
        assert_eq!(slugify("  --My  Org--  "), "my-org");
        assert_eq!(slugify("组织"), "org");
        assert_eq!(slugify(""), "org");
        assert_eq!(slugify("a_b/c"), "a-b-c");
        assert_eq!(slugify(&"x".repeat(100)).len(), 40);
        assert!(!slugify(&format!("{}-y", "x".repeat(39))).ends_with('-'));
    }

    #[test]
    fn device_labels_prefer_the_client_value_and_never_store_a_full_user_agent() {
        assert_eq!(
            device_label(Some("  My laptop \u{7} "), Some("ignored")).as_deref(),
            Some("My laptop")
        );
        assert_eq!(
            device_label(Some(&"x".repeat(300)), None).unwrap().len(),
            100
        );
        let chrome_win = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/126.0.0.0 Safari/537.36";
        assert_eq!(
            device_label(None, Some(chrome_win)).as_deref(),
            Some("Chrome on Windows")
        );
        assert_eq!(
            device_label(Some("   "), Some(chrome_win)).as_deref(),
            Some("Chrome on Windows")
        );
        let edge = format!("{chrome_win} Edg/126.0.0.0");
        assert_eq!(
            device_label(None, Some(&edge)).as_deref(),
            Some("Edge on Windows")
        );
        let safari_ios = "Mozilla/5.0 (iPhone; CPU iPhone OS 17_2_1 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.2 Mobile/15E148 Safari/604.1";
        assert_eq!(
            device_label(None, Some(safari_ios)).as_deref(),
            Some("Safari on iOS")
        );
        assert_eq!(
            device_label(
                None,
                Some("Mozilla/5.0 (X11; Linux x86_64; rv:126.0) Gecko/20100101 Firefox/126.0")
            )
            .as_deref(),
            Some("Firefox on Linux")
        );
        assert_eq!(device_label(None, Some("curl/8.0")), None);
        assert_eq!(device_label(None, None), None);
    }
}
