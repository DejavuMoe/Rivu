//! GeoIP 署名（`system-design.md` §4.4「署名要求」）。
//!
//! 署名跟随**实际加载的库**，而不是配置：切换来源后新库尚未下载成功时，仍显示旧库的署名。
//! 内置两份署名（DB-IP Lite、MaxMind GeoLite2）；自定义来源的署名由运维者通过
//! `RIVU_GEOIP_ATTRIBUTION_TEXT` / `RIVU_GEOIP_ATTRIBUTION_URL` 提供，两个都没设置时没有署名。

use serde::{Deserialize, Serialize};

/// 查询 API 的 `meta.geo_attribution`：`{source, text, url, license}`。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Attribution {
    /// `dbip-lite` | `maxmind-geolite2` | `custom`
    pub source: &'static str,
    pub text: String,
    pub url: String,
    pub license: Option<String>,
}

pub const SOURCE_DBIP: &str = "dbip-lite";
pub const SOURCE_MAXMIND: &str = "maxmind-geolite2";
pub const SOURCE_CUSTOM: &str = "custom";

impl Attribution {
    /// DB-IP Lite：文本与链接取自 DB-IP 下载页给出的署名 HTML
    /// `<a href='https://db-ip.com'>IP Geolocation by DB-IP</a>`；许可 CC BY 4.0。
    pub fn dbip_lite() -> Self {
        Attribution {
            source: SOURCE_DBIP,
            text: "IP Geolocation by DB-IP".into(),
            url: "https://db-ip.com".into(),
            license: Some("CC BY 4.0".into()),
        }
    }

    /// MaxMind GeoLite2：GeoLite EULA 第 3 条的示例句。EULA 称其为示例，措辞以 EULA 为准。
    pub fn maxmind_geolite2() -> Self {
        Attribution {
            source: SOURCE_MAXMIND,
            text: "This product includes GeoLite Data created by MaxMind, available from https://www.maxmind.com."
                .into(),
            url: "https://www.maxmind.com".into(),
            license: Some("GeoLite2 EULA".into()),
        }
    }

    /// 自定义来源：两个变量都没设置时为 `None`；只设置了其中一个时，缺的文本取链接、缺的链接取空串。
    /// 许可由运维者负责，这里不填。
    pub fn custom(text: Option<&str>, url: Option<&str>) -> Option<Self> {
        let text = text.map(str::trim).filter(|s| !s.is_empty());
        let url = url.map(str::trim).filter(|s| !s.is_empty());
        let shown = text.or(url)?;
        Some(Attribution {
            source: SOURCE_CUSTOM,
            text: shown.to_string(),
            url: url.unwrap_or_default().to_string(),
            license: None,
        })
    }

    /// 内置来源取内置署名；未知来源按自定义处理，署名取自 meta 里存下的值。
    pub(crate) fn from_stored(source: &str, stored: Option<&StoredAttribution>) -> Option<Self> {
        match source {
            SOURCE_DBIP => Some(Self::dbip_lite()),
            SOURCE_MAXMIND => Some(Self::maxmind_geolite2()),
            _ => stored.map(|s| Attribution {
                source: SOURCE_CUSTOM,
                text: s.text.clone(),
                url: s.url.clone(),
                license: s.license.clone(),
            }),
        }
    }
}

/// meta.json 里的署名字段。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredAttribution {
    pub text: String,
    pub url: String,
    pub license: Option<String>,
}

impl From<&Attribution> for StoredAttribution {
    fn from(a: &Attribution) -> Self {
        StoredAttribution {
            text: a.text.clone(),
            url: a.url.clone(),
            license: a.license.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_attributions_match_the_spec() {
        let d = Attribution::dbip_lite();
        assert_eq!(d.source, "dbip-lite");
        assert_eq!(d.text, "IP Geolocation by DB-IP");
        assert_eq!(d.url, "https://db-ip.com");
        assert_eq!(d.license.as_deref(), Some("CC BY 4.0"));
        let m = Attribution::maxmind_geolite2();
        assert_eq!(m.source, "maxmind-geolite2");
        assert_eq!(
            m.text,
            "This product includes GeoLite Data created by MaxMind, available from https://www.maxmind.com."
        );
        assert_eq!(m.url, "https://www.maxmind.com");
    }

    #[test]
    fn custom_attribution_needs_at_least_one_variable() {
        assert_eq!(Attribution::custom(None, None), None);
        assert_eq!(Attribution::custom(Some("  "), Some("")), None);
        let a = Attribution::custom(Some("Geo by X"), Some("https://x.test")).unwrap();
        assert_eq!(
            (a.source, a.text.as_str(), a.url.as_str()),
            ("custom", "Geo by X", "https://x.test")
        );
        assert_eq!(a.license, None);
        let only_text = Attribution::custom(Some("Geo by X"), None).unwrap();
        assert_eq!(only_text.url, "");
        let only_url = Attribution::custom(None, Some("https://x.test")).unwrap();
        assert_eq!(only_url.text, "https://x.test");
    }

    #[test]
    fn serializes_to_the_api_shape() {
        let v = serde_json::to_value(Attribution::dbip_lite()).unwrap();
        assert_eq!(
            v,
            serde_json::json!({
                "source": "dbip-lite",
                "text": "IP Geolocation by DB-IP",
                "url": "https://db-ip.com",
                "license": "CC BY 4.0"
            })
        );
    }
}
