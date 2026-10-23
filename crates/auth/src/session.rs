//! 登录会话的令牌与 cookie（`platform-and-ops.md` §5）。
//!
//! - 令牌：32 字节 CSPRNG 随机数的 base64url（无填充，43 个字符）；库里（`auth_sessions.token_hash`）只存 SHA-256，
//!   明文只在 `Set-Cookie` 里出现一次。令牌本身是 256 位随机数，所以同 API key 一样用 SHA-256 而不是 argon2；
//! - 有效期：`expires_at = last_used_at + 14 天`，距上次续期超过 10 分钟才写库（滑动续期）；
//! - cookie：`__Host-rivu_session`，`HttpOnly; Secure; SameSite=Lax; Path=/`，`Max-Age` 14 天随续期刷新。
//!   `__Host-` 前缀要求 `Secure`，所以在开发用的明文 HTTP 模式（`Secure` 关闭）下改用不带前缀的 `rivu_session`；
//! - 共享链接的访问凭证（密码通过后签发）同样是随机令牌（库里只存 SHA-256），cookie 名按链接区分
//!   （`__Host-rivu_share_<link_id>`），12 小时，只对该链接有效。

use std::fmt;
use std::time::Duration;

use axum::http::{HeaderMap, header};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use rand::RngCore;
use sha2::{Digest, Sha256};

/// 登录会话的有效期（距上次续期）。
pub const SESSION_TTL: Duration = Duration::from_secs(14 * 24 * 3600);
/// 距上次续期超过这么久才写库。
pub const RENEW_AFTER: Duration = Duration::from_secs(10 * 60);
/// 共享链接访问凭证的有效期。
pub const SHARE_GRANT_TTL: Duration = Duration::from_secs(12 * 3600);

const TOKEN_BYTES: usize = 32;
const ENCODED_LEN: usize = 43;

/// 新生成的令牌。`Debug` 不打印明文。
pub struct NewToken {
    plaintext: String,
    hash: [u8; 32],
}

impl fmt::Debug for NewToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NewToken")
            .field("plaintext", &"[redacted]")
            .finish()
    }
}

impl NewToken {
    pub fn plaintext(&self) -> &str {
        &self.plaintext
    }

    /// SHA-256(明文)，写入库。
    pub fn hash(&self) -> [u8; 32] {
        self.hash
    }
}

pub fn generate() -> NewToken {
    let mut random = [0u8; TOKEN_BYTES];
    rand::rng().fill_bytes(&mut random);
    let plaintext = URL_SAFE_NO_PAD.encode(random);
    let hash = hash_token(&plaintext);
    NewToken { plaintext, hash }
}

pub fn hash_token(plaintext: &str) -> [u8; 32] {
    Sha256::digest(plaintext.as_bytes()).into()
}

/// 令牌格式：43 个 base64url 字符，恰好解码成 32 字节。格式不对的 cookie 不必查库。
pub fn is_well_formed(s: &str) -> bool {
    s.len() == ENCODED_LEN
        && URL_SAFE_NO_PAD
            .decode(s)
            .is_ok_and(|b| b.len() == TOKEN_BYTES)
}

/// cookie 的属性：只有 `Secure` 是可配的（开发时的明文 HTTP 要关掉）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CookieConfig {
    pub secure: bool,
}

impl CookieConfig {
    /// 会话 cookie 的名字。
    pub fn session_name(&self) -> &'static str {
        if self.secure {
            "__Host-rivu_session"
        } else {
            "rivu_session"
        }
    }

    /// 共享链接访问凭证 cookie 的名字（按链接区分，同一个浏览器可以同时持有多个链接的凭证）。
    pub fn share_name(&self, link_id: i64) -> String {
        let prefix = if self.secure { "__Host-" } else { "" };
        format!("{prefix}rivu_share_{link_id}")
    }

    /// `Set-Cookie` 的值：`HttpOnly; SameSite=Lax; Path=/`，`secure` 时加 `Secure`。
    pub fn set(&self, name: &str, value: &str, max_age: Duration) -> String {
        let mut c = format!(
            "{name}={value}; Max-Age={}; Path=/; HttpOnly; SameSite=Lax",
            max_age.as_secs()
        );
        if self.secure {
            c.push_str("; Secure");
        }
        c
    }

    /// 让浏览器立即删除这个 cookie（属性必须与签发时一致，`__Host-` 才会被接受）。
    pub fn clear(&self, name: &str) -> String {
        let mut c = format!(
            "{name}=; Max-Age=0; Expires=Thu, 01 Jan 1970 00:00:00 GMT; Path=/; HttpOnly; SameSite=Lax"
        );
        if self.secure {
            c.push_str("; Secure");
        }
        c
    }
}

/// 取名为 `name` 的 cookie 的值（多个 `Cookie` 头与 `;` 分隔都处理；重名取第一个）。
pub fn cookie_value<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .find_map(|pair| {
            let (k, v) = pair.trim().split_once('=')?;
            (k == name).then_some(v)
        })
}

#[cfg(test)]
mod tests {
    use axum::http::HeaderValue;

    use super::*;

    #[test]
    fn tokens_are_32_random_bytes_in_base64url_and_only_the_hash_is_derived() {
        let t = generate();
        assert_eq!(t.plaintext().len(), 43);
        assert!(is_well_formed(t.plaintext()));
        assert!(
            t.plaintext()
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        );
        assert_eq!(t.hash(), hash_token(t.plaintext()));
        assert_ne!(t.hash().as_slice(), t.plaintext().as_bytes());
        assert_ne!(generate().plaintext(), t.plaintext());
        // 摘要是 SHA-256("abc")
        assert_eq!(
            hash_token("abc")
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<String>(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        let shown = format!("{t:?}");
        assert!(!shown.contains(t.plaintext()), "{shown}");
    }

    #[test]
    fn malformed_tokens_are_recognised() {
        for bad in ["", "abc", &"a".repeat(42), &"a".repeat(44), &"!".repeat(43)] {
            assert!(!is_well_formed(bad), "{bad}");
        }
    }

    #[test]
    fn secure_mode_uses_the_host_prefix_and_every_documented_attribute() {
        let c = CookieConfig { secure: true };
        assert_eq!(c.session_name(), "__Host-rivu_session");
        let v = c.set(c.session_name(), "TOKEN", SESSION_TTL);
        assert_eq!(
            v,
            "__Host-rivu_session=TOKEN; Max-Age=1209600; Path=/; HttpOnly; SameSite=Lax; Secure"
        );
        // __Host- 要求 Secure、Path=/ 且没有 Domain
        assert!(v.contains("; Secure") && v.contains("Path=/") && !v.contains("Domain"));
        let share = c.set(&c.share_name(17), "T", SHARE_GRANT_TTL);
        assert!(
            share.starts_with("__Host-rivu_share_17=T; Max-Age=43200;"),
            "{share}"
        );
        assert!(share.ends_with("; Secure"));
    }

    #[test]
    fn plain_http_dev_mode_drops_secure_and_the_host_prefix_but_keeps_the_rest() {
        let c = CookieConfig { secure: false };
        assert_eq!(c.session_name(), "rivu_session");
        let v = c.set(c.session_name(), "TOKEN", SESSION_TTL);
        assert_eq!(
            v,
            "rivu_session=TOKEN; Max-Age=1209600; Path=/; HttpOnly; SameSite=Lax"
        );
        assert_eq!(c.share_name(3), "rivu_share_3");
    }

    #[test]
    fn clearing_matches_the_issuing_attributes() {
        for secure in [true, false] {
            let c = CookieConfig { secure };
            let v = c.clear(c.session_name());
            assert!(v.starts_with(&format!("{}=;", c.session_name())));
            assert!(
                v.contains("Max-Age=0") && v.contains("HttpOnly") && v.contains("SameSite=Lax")
            );
            assert_eq!(v.contains("Secure"), secure);
        }
    }

    #[test]
    fn cookie_parsing_handles_multiple_pairs_headers_and_lookalikes() {
        let mut h = HeaderMap::new();
        h.append(
            header::COOKIE,
            HeaderValue::from_static("a=1; rivu_session=ONE; b=2"),
        );
        h.append(header::COOKIE, HeaderValue::from_static("rivu_session=TWO"));
        assert_eq!(cookie_value(&h, "rivu_session"), Some("ONE"));
        assert_eq!(cookie_value(&h, "a"), Some("1"));
        assert_eq!(cookie_value(&h, "session"), None, "不做后缀匹配");
        assert_eq!(cookie_value(&h, "rivu_sessio"), None);
        assert_eq!(cookie_value(&HeaderMap::new(), "a"), None);
    }
}
