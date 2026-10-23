//! hostname 规范化与 `HostnamePatterns`、`validated_pattern`。
//! 不变量：不 panic；`normalize_hostname` 幂等（文档写明的 `www.www.x` 除外）；
//! `validated_pattern` 成功时结果被 `HostnamePatterns` 接受且是规范形式（再校验得到同一个值）。
#![no_main]

use libfuzzer_sys::fuzz_target;
use rivu_ingest::hostname::{HostnamePatterns, normalize_hostname, validated_pattern};

fuzz_target!(|data: &[u8]| {
    let Ok(text) = std::str::from_utf8(data) else {
        return;
    };
    // 第一行是被测 host，其余每行一个模式
    let mut lines = text.split('\n');
    let host = lines.next().unwrap_or("");
    let patterns: Vec<&str> = lines.collect();

    let once = normalize_hostname(host);
    if !once.starts_with("www.") {
        assert_eq!(normalize_hostname(&once), once, "normalize_hostname 不幂等：{host:?}");
    }

    let set = HostnamePatterns::new(patterns.iter().copied());
    let _ = set.matches(&once);
    let _ = set.matches(host);
    let _ = set.is_empty();

    for p in &patterns {
        if let Ok(v) = validated_pattern(p) {
            assert!(
                !HostnamePatterns::new([v.as_str()]).is_empty(),
                "validated_pattern({p:?}) = {v:?} 却被 HostnamePatterns 忽略"
            );
            assert_eq!(
                validated_pattern(&v).as_deref().ok(),
                Some(v.as_str()),
                "validated_pattern 结果不是规范形式：{p:?} -> {v:?}"
            );
        }
    }
});
