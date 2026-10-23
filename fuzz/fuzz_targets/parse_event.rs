//! `rivu_ingest::protocol::parse_event`：任意字节，覆盖 URL / hostname / pathname / referrer 规范化。
//! 不变量：不 panic、确定性、成功时的字段边界。
#![no_main]

use libfuzzer_sys::fuzz_target;
use rivu_ingest::protocol::parse_event;

fuzz_target!(|data: &[u8]| {
    let first = parse_event(data);
    let second = parse_event(data);
    assert_eq!(first, second, "parse_event 必须是确定性的");
    match first {
        Ok(ev) => {
            assert!(ev.pathname.len() <= 1024, "pathname 超过 1024 字节");
            assert!(ev.pathname.starts_with('/'), "pathname 必须以 / 开头：{:?}", ev.pathname);
            assert!(!ev.hostname.is_empty());
            assert!(ev.name.chars().count() <= 120);
            assert!(ev.props.len() <= 30);
            assert!(u64::from(ev.engagement_ms) < 2_592_000_000);
            assert!(ev.scroll_depth <= 100 || ev.scroll_depth == 255);
            if let Some(r) = &ev.referrer {
                assert!(!r.host.is_empty());
                assert!(!r.stored.contains('?') && !r.stored.contains('#'), "{:?}", r.stored);
            }
        }
        Err(errs) => {
            assert!(!errs.is_empty());
            // 400 响应体必须能序列化成 JSON 对象
            assert!(errs.to_json().is_object());
        }
    }
});
