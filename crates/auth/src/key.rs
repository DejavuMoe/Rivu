//! API key 的格式、生成与哈希（`platform-and-ops.md` §4.3、§5）。
//!
//! 格式：`rivu_` + 32 字节 CSPRNG 随机数的 base64url（无填充，43 个字符），共 48 个字符。
//! 库里只存明文的 SHA-256（32 字节）和明文前 12 个字符（`rivu_` 加 7 个随机字符，仅用于列表识别）；
//! 明文只在创建时返回一次，之后无法找回。key 本身是 256 位随机数，所以用快速哈希（SHA-256）而不是 argon2：
//! 没有口令式的低熵输入需要拉伸，鉴权在每个请求的热路径上。

use std::fmt;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use rand::RngCore;
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

/// 固定前缀。
pub const KEY_PREFIX: &str = "rivu_";
/// 随机部分的字节数。
const RANDOM_BYTES: usize = 32;
/// 随机部分 base64url（无填充）的字符数。
const ENCODED_LEN: usize = 43;
/// 完整 key 的字符数。
pub const KEY_LEN: usize = KEY_PREFIX.len() + ENCODED_LEN;
/// `key_prefix` 列保存的明文前缀长度。
pub const DISPLAY_PREFIX_LEN: usize = 12;

/// 新生成的 key。`Debug` 不会打印明文。
pub struct NewKey {
    plaintext: String,
    hash: [u8; 32],
}

impl fmt::Debug for NewKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NewKey")
            .field("prefix", &self.prefix())
            .field("plaintext", &"[redacted]")
            .finish()
    }
}

impl NewKey {
    /// 明文：只在创建时交给用户一次。
    pub fn plaintext(&self) -> &str {
        &self.plaintext
    }

    /// 明文前 12 个字符（写入 `api_keys.key_prefix`）。
    pub fn prefix(&self) -> &str {
        &self.plaintext[..DISPLAY_PREFIX_LEN]
    }

    /// SHA-256(明文)（写入 `api_keys.key_hash`）。
    pub fn hash(&self) -> [u8; 32] {
        self.hash
    }
}

/// 生成一把新 key。
pub fn generate() -> NewKey {
    let mut random = [0u8; RANDOM_BYTES];
    rand::rng().fill_bytes(&mut random);
    let plaintext = format!("{KEY_PREFIX}{}", URL_SAFE_NO_PAD.encode(random));
    debug_assert_eq!(plaintext.len(), KEY_LEN);
    let hash = hash_key(&plaintext);
    NewKey { plaintext, hash }
}

/// SHA-256(明文)。
pub fn hash_key(plaintext: &str) -> [u8; 32] {
    Sha256::digest(plaintext.as_bytes()).into()
}

/// 明文是否符合 key 的格式：`rivu_` 加 43 个 base64url 字符，且恰好解码成 32 字节。
/// 格式不对的串不必查库（挡掉随手乱填的请求，不给 PG 增加负担）。
pub fn is_well_formed(s: &str) -> bool {
    let Some(body) = s.strip_prefix(KEY_PREFIX) else {
        return false;
    };
    body.len() == ENCODED_LEN
        && URL_SAFE_NO_PAD
            .decode(body)
            .is_ok_and(|bytes| bytes.len() == RANDOM_BYTES)
}

/// 常数时间比较两个摘要。长度不同返回 `false`（长度不是秘密）。
pub fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    a.ct_eq(b).into()
}

/// 从 `Authorization` 头取出 Bearer 令牌：`Bearer <token>`，方案名不区分大小写，
/// 方案与令牌之间一个或多个空格，令牌本身不含空白。其他写法返回 `None`。
pub fn parse_bearer(header_value: &str) -> Option<&str> {
    let (scheme, rest) = header_value.trim().split_once(' ')?;
    if !scheme.eq_ignore_ascii_case("bearer") {
        return None;
    }
    let token = rest.trim_start();
    (!token.is_empty() && !token.contains(char::is_whitespace)).then_some(token)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_keys_have_the_documented_shape() {
        let k = generate();
        assert_eq!(k.plaintext().len(), 48);
        assert!(k.plaintext().starts_with("rivu_"));
        assert!(is_well_formed(k.plaintext()));
        assert_eq!(k.prefix().len(), 12);
        assert!(k.plaintext().starts_with(k.prefix()));
        assert_eq!(k.prefix().len(), DISPLAY_PREFIX_LEN);
        // base64url 字母表：没有 + / =
        assert!(
            k.plaintext()[5..]
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        );
    }

    #[test]
    fn keys_are_random_and_hashes_match_sha256_of_the_plaintext() {
        let a = generate();
        let b = generate();
        assert_ne!(a.plaintext(), b.plaintext());
        assert_ne!(a.hash(), b.hash());
        assert_eq!(a.hash(), hash_key(a.plaintext()));
        // 已知向量：SHA-256("abc")
        let hex: String = hash_key("abc").iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(
            hex,
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn many_keys_never_collide_and_never_contain_the_hash_prefix_trivially() {
        let mut seen = std::collections::HashSet::new();
        for _ in 0..2000 {
            let k = generate();
            assert!(seen.insert(k.plaintext().to_owned()));
        }
    }

    #[test]
    fn debug_output_does_not_leak_the_plaintext() {
        let k = generate();
        let dbg = format!("{k:?}");
        assert!(!dbg.contains(k.plaintext()));
        assert!(dbg.contains("[redacted]"));
        assert!(dbg.contains(k.prefix()));
    }

    #[test]
    fn well_formedness_rejects_everything_else() {
        let good = generate().plaintext().to_owned();
        assert!(is_well_formed(&good));
        let body = &good[5..];
        for bad in [
            String::new(),
            "rivu_".into(),
            format!("rivu_{}", &body[..42]),  // 太短
            format!("rivu_{body}x"),          // 太长
            format!("RIVU_{body}"),           // 前缀大小写
            format!("rivx_{body}"),           // 前缀错
            format!("rivu_{}+", &body[..42]), // 标准 base64 字母
            format!("rivu_{}=", &body[..42]), // 填充
            format!("rivu_{} ", &body[..42]), // 空白
            body.to_owned(),                  // 缺前缀
            format!("rivu_{}", "!".repeat(43)),
            format!("rivu_{}", "é".repeat(22)),
        ] {
            assert!(!is_well_formed(&bad), "{bad:?}");
        }
        // 末位的未用比特非零：不是规范编码，拒绝
        let mut chars: Vec<char> = body.chars().collect();
        let last = *chars.last().unwrap();
        // 43 个字符编码 258 位，最后一个字符只有高 4 位有意义；把低位置 1
        let idx = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_"
            .find(last)
            .unwrap();
        let tweaked = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_"
            .chars()
            .nth(idx | 0b11)
            .unwrap();
        *chars.last_mut().unwrap() = tweaked;
        let non_canonical: String = chars.into_iter().collect();
        if non_canonical != body {
            assert!(!is_well_formed(&format!("rivu_{non_canonical}")));
        }
    }

    #[test]
    fn constant_time_comparison_behaves_like_equality() {
        let a = hash_key("one");
        let b = hash_key("one");
        let c = hash_key("two");
        assert!(ct_eq(&a, &b));
        assert!(!ct_eq(&a, &c));
        // 只差最后一个字节
        let mut d = a;
        d[31] ^= 1;
        assert!(!ct_eq(&a, &d));
        // 长度不同
        assert!(!ct_eq(&a, &a[..31]));
        assert!(!ct_eq(&a, &[]));
        assert!(ct_eq(&[], &[]));
    }

    #[test]
    fn bearer_parsing() {
        assert_eq!(parse_bearer("Bearer abc"), Some("abc"));
        assert_eq!(parse_bearer("bearer abc"), Some("abc"));
        assert_eq!(parse_bearer("BEARER   abc"), Some("abc"));
        assert_eq!(parse_bearer("  Bearer abc  "), Some("abc"));
        for bad in [
            "",
            "Bearer",
            "Bearer ",
            "Basic abc",
            "abc",
            "Bearer a b",
            "Bearerabc",
            "Token abc",
        ] {
            assert_eq!(parse_bearer(bad), None, "{bad:?}");
        }
    }
}
