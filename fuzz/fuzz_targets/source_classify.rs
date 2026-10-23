//! `rivu_source::classify` 与垃圾引荐匹配：不 panic、确定性；垃圾引荐与朴素后缀匹配一致。
#![no_main]

use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;
use rivu_domain::Utm;
use rivu_source::{SourceInput, classify, spam};

#[derive(Arbitrary, Debug)]
struct Input {
    referrer_host: Option<String>,
    source: Option<String>,
    medium: Option<String>,
    campaign: Option<String>,
    content: Option<String>,
    term: Option<String>,
    click_id_param: Option<String>,
    spam_host: String,
}

/// 朴素实现：去掉一个结尾的点、转小写，等于条目或以 `.条目` 结尾。
fn naive_spam(host: &str) -> bool {
    let h = host.strip_suffix('.').unwrap_or(host).to_ascii_lowercase();
    if h.is_empty() {
        return false;
    }
    spam::entries().any(|e| {
        h == e || (h.len() > e.len() && h.ends_with(e) && h.as_bytes()[h.len() - e.len() - 1] == b'.')
    })
}

fuzz_target!(|i: Input| {
    let utm = Utm {
        source: i.source,
        medium: i.medium,
        campaign: i.campaign,
        content: i.content,
        term: i.term,
    };
    let input = SourceInput {
        referrer_host: i.referrer_host.as_deref(),
        utm: &utm,
        click_id_param: i.click_id_param.as_deref(),
    };
    let a = classify(&input);
    let b = classify(&input);
    assert_eq!(a, b, "classify 必须是确定性的");

    if i.spam_host.len() <= 512 {
        let got = spam::is_spam_referrer(&i.spam_host);
        assert_eq!(got, spam::is_spam_referrer(&i.spam_host));
        assert_eq!(got, naive_spam(&i.spam_host), "{:?}", i.spam_host);
    }
    if let Some(h) = i.referrer_host.as_deref().filter(|h| h.len() <= 512) {
        assert_eq!(spam::is_spam_referrer(h), naive_spam(h), "{h:?}");
    }
});
