//! 密码哈希：argon2id（`platform-and-ops.md` §5；shared link 的密码也用它）。
//!
//! # 参数
//!
//! 文档只写了「argon2id」，没有给参数。取 OWASP Password Storage Cheat Sheet 的最低推荐组合之一：
//! 内存 19 MiB（`m = 19456` KiB）、迭代 2、并行度 1、输出 32 字节、盐 16 字节（CSPRNG）。
//! 在常见服务器核心上约 20–50 ms 一次；并发度由信号量限制（默认 [`DEFAULT_CONCURRENCY`]），
//! 所以最坏的瞬时内存是 `并发度 × 19 MiB`。PHC 字符串里带着参数，以后调参不会让旧哈希失效。
//!
//! # 长度
//!
//! 12–128 个字符（按 Unicode 标量计），不强制字符类。上限同时挡住「用超长密码拖垮哈希」。
//!
//! # 不存在的账号
//!
//! [`PasswordService::verify`] 在没有存储的哈希时（账号不存在）对一个启动时生成的假哈希做同样的运算，
//! 所以「账号不存在」与「密码错误」的耗时同量级（哈希在 `spawn_blocking` 里算，不占用异步线程）。

use std::sync::Arc;

use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::{Algorithm, Argon2, Params, Version};
use rand::RngCore;
use tokio::sync::Semaphore;

/// `m`：内存，KiB。
pub const M_COST_KIB: u32 = 19_456;
/// `t`：迭代次数。
pub const T_COST: u32 = 2;
/// `p`：并行度。
pub const P_COST: u32 = 1;
/// 输出长度（字节）。
pub const OUTPUT_LEN: usize = 32;
/// 盐长度（字节）。
pub const SALT_LEN: usize = 16;
/// 同时进行的哈希运算数。
pub const DEFAULT_CONCURRENCY: usize = 4;

pub const MIN_PASSWORD_CHARS: usize = 12;
pub const MAX_PASSWORD_CHARS: usize = 128;

fn argon2() -> Argon2<'static> {
    let params = Params::new(M_COST_KIB, T_COST, P_COST, Some(OUTPUT_LEN)).expect("固定的合法参数");
    Argon2::new(Algorithm::Argon2id, Version::V0x13, params)
}

/// 密码是否满足长度要求（返回给用户的英文原因）。
pub fn validate_password(password: &str) -> Result<(), &'static str> {
    let n = password.chars().count();
    if n < MIN_PASSWORD_CHARS {
        return Err("the password must be at least 12 characters");
    }
    if n > MAX_PASSWORD_CHARS {
        return Err("the password must be at most 128 characters");
    }
    Ok(())
}

/// 同步哈希（阻塞，约几十毫秒）。异步代码用 [`PasswordService::hash`]。
pub fn hash_blocking(password: &str) -> Result<String, PasswordError> {
    let mut raw = [0u8; SALT_LEN];
    rand::rng().fill_bytes(&mut raw);
    let salt = SaltString::encode_b64(&raw).map_err(|_| PasswordError::Hash)?;
    argon2()
        .hash_password(password.as_bytes(), &salt)
        .map(|h| h.to_string())
        .map_err(|_| PasswordError::Hash)
}

/// 同步校验（阻塞）。`phc` 无法解析按「不匹配」处理。参数取自哈希字符串本身。
pub fn verify_blocking(password: &str, phc: &str) -> bool {
    match PasswordHash::new(phc) {
        Ok(parsed) => argon2()
            .verify_password(password.as_bytes(), &parsed)
            .is_ok(),
        Err(_) => false,
    }
}

/// 这个哈希是不是 argon2id（登录时遇到别的算法的哈希要报警，不能默默接受）。
pub fn is_argon2id(phc: &str) -> bool {
    PasswordHash::new(phc).is_ok_and(|h| h.algorithm.as_str() == "argon2id")
}

#[derive(Debug, PartialEq, Eq)]
pub enum PasswordError {
    /// 哈希运算失败（参数或内存分配问题）。
    Hash,
    /// 运算线程异常退出。
    Task,
}

impl std::fmt::Display for PasswordError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PasswordError::Hash => f.write_str("password hashing failed"),
            PasswordError::Task => f.write_str("password hashing task failed"),
        }
    }
}

impl std::error::Error for PasswordError {}

/// 在 `spawn_blocking` 里做哈希与校验，用信号量限制并发。进程里建一个并共享。
#[derive(Clone)]
pub struct PasswordService {
    permits: Arc<Semaphore>,
    /// 账号不存在时校验的假哈希（与真哈希同参数）。
    dummy: Arc<String>,
}

impl PasswordService {
    /// 生成假哈希（同步，几十毫秒），启动时调用一次。
    pub fn new(concurrency: usize) -> Self {
        let mut junk = [0u8; 24];
        rand::rng().fill_bytes(&mut junk);
        let dummy = hash_blocking(&format!("dummy-{junk:?}")).expect("假哈希");
        PasswordService {
            permits: Arc::new(Semaphore::new(concurrency.max(1))),
            dummy: Arc::new(dummy),
        }
    }

    pub async fn hash(&self, password: String) -> Result<String, PasswordError> {
        let _permit = self
            .permits
            .acquire()
            .await
            .map_err(|_| PasswordError::Task)?;
        tokio::task::spawn_blocking(move || hash_blocking(&password))
            .await
            .map_err(|_| PasswordError::Task)?
    }

    /// 校验。`stored` 为 `None`（账号不存在）时对假哈希做同样的运算并返回 `false`。
    pub async fn verify(&self, password: String, stored: Option<&str>) -> bool {
        let phc = stored.map_or_else(|| Arc::clone(&self.dummy), |s| Arc::new(s.to_owned()));
        let is_real = stored.is_some();
        let Ok(_permit) = self.permits.acquire().await else {
            return false;
        };
        let ok = tokio::task::spawn_blocking(move || verify_blocking(&password, &phc))
            .await
            .unwrap_or(false);
        ok && is_real
    }
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::*;

    #[test]
    fn hashes_are_argon2id_with_the_documented_parameters_and_random_salts() {
        let h1 = hash_blocking("correct horse battery").unwrap();
        let h2 = hash_blocking("correct horse battery").unwrap();
        assert!(h1.starts_with("$argon2id$v=19$m=19456,t=2,p=1$"), "{h1}");
        assert_ne!(h1, h2, "每次的盐都不同");
        assert!(is_argon2id(&h1));
        let parsed = PasswordHash::new(&h1).unwrap();
        assert_eq!(parsed.hash.unwrap().len(), OUTPUT_LEN);
        assert_eq!(parsed.salt.unwrap().len() * 3 / 4, SALT_LEN);
    }

    #[test]
    fn verify_accepts_the_right_password_only() {
        let h = hash_blocking("correct horse battery").unwrap();
        assert!(verify_blocking("correct horse battery", &h));
        assert!(!verify_blocking("correct horse batterz", &h));
        assert!(!verify_blocking("", &h));
        assert!(!verify_blocking(
            "correct horse battery",
            "not a phc string"
        ));
        assert!(!verify_blocking("correct horse battery", ""));
    }

    #[test]
    fn verify_follows_the_parameters_stored_in_the_hash() {
        // 旧参数（m=8 KiB, t=1）生成的哈希仍可校验：以后调参不会让旧哈希失效
        let params = Params::new(8, 1, 1, Some(32)).unwrap();
        let weak = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);
        let salt = SaltString::encode_b64(&[7u8; 16]).unwrap();
        let phc = weak
            .hash_password(b"an older password", &salt)
            .unwrap()
            .to_string();
        assert!(phc.contains("m=8,t=1,p=1"));
        assert!(verify_blocking("an older password", &phc));
        assert!(!verify_blocking("another password", &phc));
    }

    #[test]
    fn other_algorithms_are_not_argon2id() {
        assert!(!is_argon2id(
            "$argon2i$v=19$m=8,t=1,p=1$c2FsdHNhbHRzYWx0$aGFzaGhhc2hoYXNoaGFzaGhhc2hoYXNoaGFzaGhhc2g"
        ));
        assert!(!is_argon2id("garbage"));
    }

    #[test]
    fn length_rules_count_characters_not_bytes() {
        assert!(validate_password("a".repeat(11).as_str()).is_err());
        assert!(validate_password("a".repeat(12).as_str()).is_ok());
        assert!(validate_password("a".repeat(128).as_str()).is_ok());
        assert!(validate_password("a".repeat(129).as_str()).is_err());
        // 12 个汉字是 36 字节，但只有 12 个字符
        assert!(validate_password(&"密".repeat(12)).is_ok());
        assert!(validate_password(&"密".repeat(11)).is_err());
        // 没有字符类要求
        assert!(validate_password("            ").is_ok());
    }

    #[tokio::test]
    async fn service_verifies_and_treats_a_missing_account_as_a_mismatch() {
        let svc = PasswordService::new(2);
        let h = svc.hash("correct horse battery".into()).await.unwrap();
        assert!(svc.verify("correct horse battery".into(), Some(&h)).await);
        assert!(!svc.verify("wrong password!!".into(), Some(&h)).await);
        // 账号不存在：永远 false，即使给的恰好是假哈希对应的字符串也不会通过
        assert!(!svc.verify("correct horse battery".into(), None).await);
        assert!(!svc.verify(String::new(), None).await);
    }

    #[tokio::test]
    async fn a_missing_account_costs_about_as_much_as_a_wrong_password() {
        let svc = PasswordService::new(2);
        let h = svc.hash("correct horse battery".into()).await.unwrap();
        let time = async |stored: Option<&str>| {
            let t = Instant::now();
            for _ in 0..4 {
                svc.verify("wrong password!!".into(), stored).await;
            }
            t.elapsed().as_secs_f64()
        };
        let (wrong, missing) = (time(Some(&h)).await, time(None).await);
        let ratio = wrong / missing;
        assert!(
            (0.5..2.0).contains(&ratio),
            "wrong={wrong:.3}s missing={missing:.3}s"
        );
    }
}
