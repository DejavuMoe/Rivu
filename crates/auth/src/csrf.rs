//! 内部端点的 CSRF 防护（`platform-and-ops.md` §5「CSRF」）。
//!
//! 文档的规则：变更请求只接受 `Content-Type: application/json` 并校验 `Origin` 的 host 等于 `RIVU_BASE_URL`；
//! 无表单提交；同源部署不开 CORS。实现（比文档更严一点）：
//!
//! 1. 只检查会改变状态的方法（`POST`、`PUT`、`PATCH`、`DELETE`；`GET`、`HEAD`、`OPTIONS` 放行——内部的 `GET` 端点都是只读的）；
//! 2. `Content-Type` 必须是 `application/json`（参数如 `; charset=utf-8` 忽略）：跨站的 `<form>` 只能发
//!    `application/x-www-form-urlencoded`、`multipart/form-data`、`text/plain`，发 JSON 要走 `fetch`，而没有 CORS 应答
//!    时浏览器会在预检就拦下；
//! 3. 必须带 `Origin`，且与 `RIVU_BASE_URL` 的源（协议、主机、端口，不只是主机）相同；`Origin: null`、缺失、
//!    别的源一律拒绝。浏览器对所有 `POST` 与跨源请求都会发 `Origin`，所以缺失说明不是浏览器的同源请求；
//!    非浏览器客户端（运维脚本）用命令行 `rivu ...`，公开 API 用 API key；
//! 4. 浏览器声明的 `Sec-Fetch-Site: cross-site` 直接拒绝（纵深防御，不依赖它）。
//!
//! 再叠加 cookie 的 `SameSite=Lax`：跨站的 `POST` 本来就不会带 cookie。

use axum::http::{HeaderMap, Method, header};
use url::{Origin, Url};

/// CSRF 判定失败的原因（响应里只给通用文字）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CsrfError {
    /// `Content-Type` 不是 `application/json`。
    ContentType,
    /// 没有 `Origin`（或不止一个）。
    OriginMissing,
    /// `Origin` 与 `RIVU_BASE_URL` 不是同一个源。
    OriginMismatch,
    /// 浏览器声明这是跨站请求。
    CrossSite,
}

/// 期望的源（来自 `RIVU_BASE_URL`）。
#[derive(Clone, Debug)]
pub struct Csrf {
    origin: Origin,
}

impl Csrf {
    /// `base_url` 是 `RIVU_BASE_URL`（`http(s)://host[:port][/path]`，路径被忽略）。
    pub fn new(base_url: &str) -> Result<Self, String> {
        let url = Url::parse(base_url).map_err(|e| format!("`{base_url}` 不是有效的 URL：{e}"))?;
        if !matches!(url.scheme(), "http" | "https") || url.host().is_none() {
            return Err(format!("`{base_url}` 必须是带主机的 http(s) 地址"));
        }
        Ok(Csrf {
            origin: url.origin(),
        })
    }

    /// 方法是否会改变状态（需要检查）。
    pub fn is_unsafe(method: &Method) -> bool {
        !matches!(*method, Method::GET | Method::HEAD | Method::OPTIONS)
    }

    pub fn check(&self, method: &Method, headers: &HeaderMap) -> Result<(), CsrfError> {
        if !Self::is_unsafe(method) {
            return Ok(());
        }
        if headers
            .get("sec-fetch-site")
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.eq_ignore_ascii_case("cross-site"))
        {
            return Err(CsrfError::CrossSite);
        }
        let json = headers
            .get_all(header::CONTENT_TYPE)
            .iter()
            .collect::<Vec<_>>();
        let is_json = json.len() == 1
            && json[0].to_str().is_ok_and(|v| {
                v.split(';')
                    .next()
                    .is_some_and(|m| m.trim().eq_ignore_ascii_case("application/json"))
            });
        if !is_json {
            return Err(CsrfError::ContentType);
        }
        let mut origins = headers.get_all(header::ORIGIN).iter();
        let (Some(origin), None) = (origins.next(), origins.next()) else {
            return Err(CsrfError::OriginMissing);
        };
        let Some(origin) = origin.to_str().ok().and_then(|o| Url::parse(o).ok()) else {
            return Err(CsrfError::OriginMismatch);
        };
        // `Origin` 只有「协议://主机[:端口]」：带路径、查询或用户信息的不是合法的源
        if origin.path() != "/"
            || origin.query().is_some()
            || origin.fragment().is_some()
            || !origin.username().is_empty()
            || origin.password().is_some()
            || origin.origin() != self.origin
        {
            return Err(CsrfError::OriginMismatch);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use axum::http::HeaderValue;

    use super::*;

    fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut h = HeaderMap::new();
        for (k, v) in pairs {
            h.append(
                axum::http::HeaderName::from_bytes(k.as_bytes()).unwrap(),
                HeaderValue::from_str(v).unwrap(),
            );
        }
        h
    }

    fn csrf() -> Csrf {
        Csrf::new("https://stats.example.com").unwrap()
    }

    const JSON: (&str, &str) = ("content-type", "application/json");

    #[test]
    fn safe_methods_are_not_checked() {
        for m in [Method::GET, Method::HEAD, Method::OPTIONS] {
            assert_eq!(csrf().check(&m, &HeaderMap::new()), Ok(()), "{m}");
        }
    }

    #[test]
    fn unsafe_methods_need_json_and_a_matching_origin() {
        let ok = headers(&[JSON, ("origin", "https://stats.example.com")]);
        for m in [Method::POST, Method::PUT, Method::PATCH, Method::DELETE] {
            assert_eq!(csrf().check(&m, &ok), Ok(()), "{m}");
        }
        // Content-Type 带参数、大小写不同都可以
        let ok = headers(&[
            ("content-type", "Application/JSON; charset=utf-8"),
            ("origin", "https://stats.example.com"),
        ]);
        assert_eq!(csrf().check(&Method::POST, &ok), Ok(()));
    }

    #[test]
    fn form_content_types_are_rejected_even_with_the_right_origin() {
        for ct in [
            "application/x-www-form-urlencoded",
            "multipart/form-data; boundary=x",
            "text/plain",
            "application/jsonp",
            "application/json2",
            "",
        ] {
            let h = headers(&[
                ("content-type", ct),
                ("origin", "https://stats.example.com"),
            ]);
            assert_eq!(
                csrf().check(&Method::POST, &h),
                Err(CsrfError::ContentType),
                "{ct}"
            );
        }
        let h = headers(&[("origin", "https://stats.example.com")]);
        assert_eq!(csrf().check(&Method::POST, &h), Err(CsrfError::ContentType));
        // 两个 Content-Type 头：有歧义，拒绝
        let h = headers(&[
            JSON,
            ("content-type", "text/plain"),
            ("origin", "https://stats.example.com"),
        ]);
        assert_eq!(csrf().check(&Method::POST, &h), Err(CsrfError::ContentType));
    }

    #[test]
    fn missing_null_and_foreign_origins_are_rejected() {
        assert_eq!(
            csrf().check(&Method::POST, &headers(&[JSON])),
            Err(CsrfError::OriginMissing)
        );
        for o in [
            "null",
            "",
            "https://evil.example.net",
            "https://stats.example.com.evil.net",
            "https://evil.net/https://stats.example.com",
            "http://stats.example.com",
            "https://stats.example.com:8443",
            "https://sub.stats.example.com",
            "https://stats.example.com/path",
            "https://user@stats.example.com",
        ] {
            assert_eq!(
                csrf().check(&Method::POST, &headers(&[JSON, ("origin", o)])),
                Err(CsrfError::OriginMismatch),
                "{o:?}"
            );
        }
        // 两个 Origin 头
        let h = headers(&[
            JSON,
            ("origin", "https://stats.example.com"),
            ("origin", "https://stats.example.com"),
        ]);
        assert_eq!(
            csrf().check(&Method::POST, &h),
            Err(CsrfError::OriginMissing)
        );
    }

    #[test]
    fn origin_comparison_normalises_default_ports_and_case_and_ignores_the_base_path() {
        let c = Csrf::new("https://Stats.Example.com:443/dashboard/").unwrap();
        for o in ["https://stats.example.com", "https://STATS.example.com:443"] {
            assert_eq!(
                c.check(&Method::POST, &headers(&[JSON, ("origin", o)])),
                Ok(()),
                "{o}"
            );
        }
        let dev = Csrf::new("http://127.0.0.1:8080").unwrap();
        assert_eq!(
            dev.check(
                &Method::POST,
                &headers(&[JSON, ("origin", "http://127.0.0.1:8080")])
            ),
            Ok(())
        );
        assert_eq!(
            dev.check(
                &Method::POST,
                &headers(&[JSON, ("origin", "http://localhost:8080")])
            ),
            Err(CsrfError::OriginMismatch)
        );
    }

    #[test]
    fn browser_declared_cross_site_requests_are_rejected_first() {
        let h = headers(&[
            JSON,
            ("origin", "https://stats.example.com"),
            ("sec-fetch-site", "cross-site"),
        ]);
        assert_eq!(csrf().check(&Method::POST, &h), Err(CsrfError::CrossSite));
        let h = headers(&[
            JSON,
            ("origin", "https://stats.example.com"),
            ("sec-fetch-site", "same-origin"),
        ]);
        assert_eq!(csrf().check(&Method::POST, &h), Ok(()));
    }

    #[test]
    fn base_url_must_be_an_http_url_with_a_host() {
        assert!(Csrf::new("stats.example.com").is_err());
        assert!(Csrf::new("ftp://stats.example.com").is_err());
        assert!(Csrf::new("https://").is_err());
        assert!(Csrf::new("http://localhost:8080").is_ok());
    }
}
