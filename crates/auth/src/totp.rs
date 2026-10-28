//! 两步验证：TOTP（RFC 6238：HMAC-SHA1、6 位、30 秒一步）与恢复码（`platform-and-ops.md` §5）。
//!
//! - 密钥 20 字节随机数，存库前用 [`Vault`] 加密：`RIVU_SECRET_KEY` 经 HKDF-SHA256 派生 AES-256-GCM 密钥，
//!   密文是 12 字节 nonce 加 20 字节密文加 16 字节标签（共 48 字节）。轮换时旧密钥放 `RIVU_SECRET_KEY_FALLBACK`，只用来解密；
//! - 验证接受前后各一步（时钟误差 ±30 秒），且时间步必须大于上次用过的（同一个验证码不能用两次）；
//! - 恢复码 10 个，`xxxx-xxxx`（去掉了容易看错的字符），库里只存 SHA-256，用掉一个删一个。

use ring::aead::{AES_256_GCM, Aad, LessSafeKey, NONCE_LEN, Nonce, UnboundKey};
use ring::hkdf::{HKDF_SHA256, Salt};
use ring::hmac;
use ring::rand::{SecureRandom, SystemRandom};
use sha2::{Digest, Sha256};

/// 密钥长度（字节）。
pub const SECRET_LEN: usize = 20;
/// 加密后的长度：nonce + 密文 + 标签。
pub const SEALED_LEN: usize = NONCE_LEN + SECRET_LEN + 16;
/// 时间步（秒）。
pub const STEP_SECS: u64 = 30;
/// 恢复码的个数。
pub const RECOVERY_CODES: usize = 10;

/// 用 `RIVU_SECRET_KEY`（与轮换期的旧密钥）加解密 TOTP 密钥。
pub struct Vault {
    keys: Vec<LessSafeKey>,
}

fn derive(secret: &str) -> LessSafeKey {
    struct Len;
    impl ring::hkdf::KeyType for Len {
        fn len(&self) -> usize {
            32
        }
    }
    let prk = Salt::new(HKDF_SHA256, b"rivu").extract(secret.as_bytes());
    let mut key = [0u8; 32];
    prk.expand(&[b"totp-secret-v1"], Len)
        .and_then(|okm| okm.fill(&mut key))
        .expect("HKDF 输出 32 字节不会失败");
    LessSafeKey::new(UnboundKey::new(&AES_256_GCM, &key).expect("32 字节是 AES-256 的密钥长度"))
}

impl Vault {
    pub fn new(primary: &str, fallback: Option<&str>) -> Self {
        Vault {
            keys: std::iter::once(primary)
                .chain(fallback)
                .map(derive)
                .collect(),
        }
    }

    /// 用主密钥加密。
    pub fn seal(&self, secret: &[u8; SECRET_LEN]) -> Vec<u8> {
        let mut nonce = [0u8; NONCE_LEN];
        SystemRandom::new()
            .fill(&mut nonce)
            .expect("系统随机数不可用");
        let mut buf = secret.to_vec();
        self.keys[0]
            .seal_in_place_append_tag(Nonce::assume_unique_for_key(nonce), Aad::empty(), &mut buf)
            .expect("AES-GCM 加密不会失败");
        let mut out = nonce.to_vec();
        out.extend(buf);
        out
    }

    /// 依次用主密钥、旧密钥解密；都不行返回 `None`（密钥换了却没配旧密钥，或数据损坏）。
    pub fn open(&self, sealed: &[u8]) -> Option<[u8; SECRET_LEN]> {
        if sealed.len() != SEALED_LEN {
            return None;
        }
        let (nonce, ct) = sealed.split_at(NONCE_LEN);
        self.keys.iter().find_map(|k| {
            let mut buf = ct.to_vec();
            let nonce = Nonce::try_assume_unique_for_key(nonce).ok()?;
            let plain = k.open_in_place(nonce, Aad::empty(), &mut buf).ok()?;
            plain.try_into().ok()
        })
    }
}

/// 新的随机密钥。
pub fn generate_secret() -> [u8; SECRET_LEN] {
    let mut s = [0u8; SECRET_LEN];
    SystemRandom::new().fill(&mut s).expect("系统随机数不可用");
    s
}

/// RFC 4648 base32，不带填充（验证器 App 手动输入用）。
pub fn base32(data: &[u8]) -> String {
    const A: &[u8; 32] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";
    let mut out = String::new();
    let (mut buf, mut bits) = (0u32, 0u32);
    for &b in data {
        buf = (buf << 8) | u32::from(b);
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            out.push(A[((buf >> bits) & 31) as usize] as char);
        }
    }
    if bits > 0 {
        out.push(A[((buf << (5 - bits)) & 31) as usize] as char);
    }
    out
}

/// 验证器 App 扫码用的 `otpauth://` 地址。
pub fn otpauth_uri(issuer: &str, account: &str, secret: &[u8; SECRET_LEN]) -> String {
    let enc = |s: &str| {
        url::form_urlencoded::byte_serialize(s.as_bytes())
            .collect::<String>()
            .replace('+', "%20")
    };
    format!(
        "otpauth://totp/{}:{}?secret={}&issuer={}&algorithm=SHA1&digits=6&period=30",
        enc(issuer),
        enc(account),
        base32(secret),
        enc(issuer)
    )
}

/// 某个时间步的 6 位验证码。
pub fn code_at(secret: &[u8], step: u64) -> u32 {
    let key = hmac::Key::new(hmac::HMAC_SHA1_FOR_LEGACY_USE_ONLY, secret);
    let tag = hmac::sign(&key, &step.to_be_bytes());
    let h = tag.as_ref();
    let off = (h[h.len() - 1] & 0x0f) as usize;
    let n = u32::from_be_bytes([h[off] & 0x7f, h[off + 1], h[off + 2], h[off + 3]]);
    n % 1_000_000
}

/// 校验验证码：接受当前步与前后各一步，且步数大于 `last_step`。返回匹配的步（调用方记为新的 `last_step`）。
pub fn verify(secret: &[u8], code: &str, unix_secs: u64, last_step: u64) -> Option<u64> {
    let code: String = code.chars().filter(|c| !c.is_whitespace()).collect();
    if code.len() != 6 || !code.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let want: u32 = code.parse().ok()?;
    let now = unix_secs / STEP_SECS;
    [now.saturating_sub(1), now, now + 1]
        .into_iter()
        .filter(|s| *s > last_step)
        .find(|s| code_at(secret, *s) == want)
}

/// 新的一组恢复码（展示给用户）与它们的摘要（存库）。
pub fn recovery_codes() -> (Vec<String>, Vec<[u8; 32]>) {
    // 去掉 0 o 1 l i 这些容易看错的字符
    const A: &[u8] = b"23456789abcdefghjkmnpqrstuvwxyz";
    let rng = SystemRandom::new();
    let codes: Vec<String> = (0..RECOVERY_CODES)
        .map(|_| {
            let mut raw = [0u8; 8];
            rng.fill(&mut raw).expect("系统随机数不可用");
            let s: String = raw
                .iter()
                .map(|b| A[*b as usize % A.len()] as char)
                .collect();
            format!("{}-{}", &s[..4], &s[4..])
        })
        .collect();
    let hashes = codes.iter().map(|c| recovery_hash(c)).collect();
    (codes, hashes)
}

/// 恢复码的摘要：去掉空白与连字符、转小写后取 SHA-256（用户输入时不必在意格式）。
pub fn recovery_hash(code: &str) -> [u8; 32] {
    let norm: String = code
        .chars()
        .filter(|c| !c.is_whitespace() && *c != '-')
        .flat_map(char::to_lowercase)
        .collect();
    Sha256::digest(norm.as_bytes()).into()
}

/// 输入看起来像恢复码（8 个字母数字，可带连字符），而不是 6 位验证码。
pub fn looks_like_recovery_code(input: &str) -> bool {
    let n: String = input
        .chars()
        .filter(|c| !c.is_whitespace() && *c != '-')
        .collect();
    n.len() == 8 && n.chars().all(|c| c.is_ascii_alphanumeric())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RFC 6238 附录 B 的 SHA-1 向量（密钥 "12345678901234567890"），取 8 位里的后 6 位。
    #[test]
    fn rfc6238_vectors() {
        let secret = b"12345678901234567890";
        for (t, want8) in [
            (59u64, 94287082u32),
            (1111111109, 7081804),
            (1111111111, 14050471),
            (1234567890, 89005924),
            (2000000000, 69279037),
            (20000000000, 65353130),
        ] {
            assert_eq!(code_at(secret, t / STEP_SECS), want8 % 1_000_000, "t={t}");
        }
    }

    #[test]
    fn verify_window_and_replay() {
        let secret = b"12345678901234567890";
        let t = 1_111_111_111;
        let step = t / STEP_SECS;
        let code = format!("{:06}", code_at(secret, step));
        assert_eq!(verify(secret, &code, t, 0), Some(step));
        assert_eq!(
            verify(secret, &format!("{} {}", &code[..3], &code[3..]), t, 0),
            Some(step)
        );
        // 前后一步都接受，再远不接受
        assert_eq!(verify(secret, &code, t + 30, 0), Some(step));
        assert_eq!(verify(secret, &code, t - 30, 0), Some(step));
        assert_eq!(verify(secret, &code, t + 90, 0), None);
        // 用过的步不能再用
        assert_eq!(verify(secret, &code, t, step), None);
        assert_eq!(verify(secret, "12345", t, 0), None);
        assert_eq!(verify(secret, "abcdef", t, 0), None);
    }

    #[test]
    fn vault_round_trip_and_rotation() {
        let s = generate_secret();
        let old = Vault::new(&"a".repeat(32), None);
        let sealed = old.seal(&s);
        assert_eq!(sealed.len(), SEALED_LEN);
        assert_eq!(old.open(&sealed), Some(s));
        // 换了主密钥：没有旧密钥解不开，配上旧密钥能解
        assert_eq!(Vault::new(&"b".repeat(32), None).open(&sealed), None);
        let rotating = Vault::new(&"b".repeat(32), Some(&"a".repeat(32)));
        assert_eq!(rotating.open(&sealed), Some(s));
        // 新加密用主密钥
        assert_eq!(
            Vault::new(&"b".repeat(32), None).open(&rotating.seal(&s)),
            Some(s)
        );
        let mut bad = sealed.clone();
        bad[20] ^= 1;
        assert_eq!(old.open(&bad), None);
    }

    #[test]
    fn base32_and_uri() {
        assert_eq!(base32(b"foobar"), "MZXW6YTBOI");
        assert_eq!(
            base32(b"12345678901234567890"),
            "GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ"
        );
        let uri = otpauth_uri("Rivu", "lin@example.com", b"12345678901234567890");
        assert_eq!(
            uri,
            "otpauth://totp/Rivu:lin%40example.com?secret=GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ&issuer=Rivu&algorithm=SHA1&digits=6&period=30"
        );
    }

    #[test]
    fn recovery_codes_are_normalized() {
        let (codes, hashes) = recovery_codes();
        assert_eq!(codes.len(), RECOVERY_CODES);
        assert!(
            codes
                .iter()
                .all(|c| c.len() == 9 && c.as_bytes()[4] == b'-')
        );
        assert!(looks_like_recovery_code(&codes[0]));
        assert!(!looks_like_recovery_code("123456"));
        assert_eq!(
            recovery_hash(&codes[0].to_uppercase().replace('-', " ")),
            hashes[0]
        );
    }
}
