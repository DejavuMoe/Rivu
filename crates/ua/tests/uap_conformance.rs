//! uap-core 自带测试集的一致性测试（外部数据源，固定到与 `regexes.yaml` 同一 commit）。
//!
//! 数据来源与哈希见 `tests/data/PROVENANCE.md`。三个文件逐条比对 `UaParser::parse_raw`
//! （即 uap-core 的原始结果，不含 Rivu 自己的归一化），报告每个文件的通过率。
//! 未通过的用例必须在下面的允许失败清单里，且每条都写原因；清单里多余的条目（其实已经通过）也会失败，
//! 这样清单不会悄悄变大或变旧。

use std::collections::BTreeSet;

use rivu_ua::UaParser;
use serde::Deserialize;

const DATA: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data/");

#[derive(Deserialize)]
struct Cases<T> {
    test_cases: Vec<T>,
}

#[derive(Deserialize)]
struct UaCase {
    user_agent_string: String,
    family: String,
    major: Option<String>,
    minor: Option<String>,
    patch: Option<String>,
}

#[derive(Deserialize)]
struct OsCase {
    user_agent_string: String,
    family: String,
    major: Option<String>,
    minor: Option<String>,
    patch: Option<String>,
    patch_minor: Option<String>,
}

#[derive(Deserialize)]
struct DeviceCase {
    user_agent_string: String,
    family: String,
    brand: Option<String>,
    model: Option<String>,
}

fn load<T: for<'de> Deserialize<'de>>(file: &str) -> Vec<T> {
    let text = std::fs::read_to_string(format!("{DATA}{file}")).unwrap();
    serde_saphyr::from_str::<Cases<T>>(&text)
        .unwrap()
        .test_cases
}

/// uap-core 的约定：版本段缺失可以写 null 也可以写空串，两者等价。
fn same(expected: &Option<String>, actual: &Option<String>) -> bool {
    let norm = |o: &Option<String>| o.clone().filter(|s| !s.is_empty());
    norm(expected) == norm(actual)
}

/// 允许失败清单：`(文件, UA 字符串)`，第三项是原因。
///
/// 空表是理想状态；每增加一条都要写明为什么这是所选实现 / 规则库的已知差异。
const ALLOWED_FAILURES: &[(&str, &str, &str)] = &[];

struct Report {
    file: &'static str,
    total: usize,
    failed: Vec<String>, // UA 字符串
}

impl Report {
    fn new(file: &'static str, total: usize) -> Self {
        Self {
            file,
            total,
            failed: Vec::new(),
        }
    }

    fn finish(self) {
        let passed = self.total - self.failed.len();
        println!(
            "{}: {passed}/{} passed ({:.3}%)",
            self.file,
            self.total,
            100.0 * passed as f64 / self.total as f64
        );
        let allowed: BTreeSet<&str> = ALLOWED_FAILURES
            .iter()
            .filter(|(f, _, _)| *f == self.file)
            .map(|(_, ua, _)| *ua)
            .collect();
        let failed: BTreeSet<&str> = self.failed.iter().map(String::as_str).collect();
        let unexpected: Vec<_> = failed.difference(&allowed).collect();
        let stale: Vec<_> = allowed.difference(&failed).collect();
        assert!(
            unexpected.is_empty(),
            "{}: 未列入允许失败清单的失败用例 {} 条，前几条：{:#?}",
            self.file,
            unexpected.len(),
            &unexpected[..unexpected.len().min(5)]
        );
        assert!(
            stale.is_empty(),
            "{}: 清单里已经通过的条目：{stale:?}",
            self.file
        );
    }
}

#[test]
fn test_ua_yaml() {
    let parser = UaParser::embedded().unwrap();
    let cases: Vec<UaCase> = load("uap-test_ua.yaml");
    let mut rep = Report::new("test_ua.yaml", cases.len());
    for c in &cases {
        let r = parser.parse_raw(&c.user_agent_string);
        let ok = r.browser == c.family
            && same(&c.major, &r.browser_major)
            && same(&c.minor, &r.browser_minor)
            && same(&c.patch, &r.browser_patch);
        if !ok {
            println!(
                "FAIL ua {:?}\n  want {} {:?} {:?} {:?}\n  got  {} {:?} {:?} {:?}",
                c.user_agent_string,
                c.family,
                c.major,
                c.minor,
                c.patch,
                r.browser,
                r.browser_major,
                r.browser_minor,
                r.browser_patch
            );
            rep.failed.push(c.user_agent_string.clone());
        }
    }
    rep.finish();
}

#[test]
fn test_os_yaml() {
    let parser = UaParser::embedded().unwrap();
    let cases: Vec<OsCase> = load("uap-test_os.yaml");
    let mut rep = Report::new("test_os.yaml", cases.len());
    for c in &cases {
        let r = parser.parse_raw(&c.user_agent_string);
        let ok = r.os == c.family
            && same(&c.major, &r.os_major)
            && same(&c.minor, &r.os_minor)
            && same(&c.patch, &r.os_patch)
            && same(&c.patch_minor, &r.os_patch_minor);
        if !ok {
            println!(
                "FAIL os {:?}\n  want {} {:?} {:?} {:?} {:?}\n  got  {} {:?} {:?} {:?} {:?}",
                c.user_agent_string,
                c.family,
                c.major,
                c.minor,
                c.patch,
                c.patch_minor,
                r.os,
                r.os_major,
                r.os_minor,
                r.os_patch,
                r.os_patch_minor
            );
            rep.failed.push(c.user_agent_string.clone());
        }
    }
    rep.finish();
}

#[test]
fn test_device_yaml() {
    let parser = UaParser::embedded().unwrap();
    let cases: Vec<DeviceCase> = load("uap-test_device.yaml");
    let mut rep = Report::new("test_device.yaml", cases.len());
    for c in &cases {
        let r = parser.parse_raw(&c.user_agent_string);
        let ok = r.device == c.family
            && same(&c.brand, &r.device_brand)
            && same(&c.model, &r.device_model);
        if !ok {
            println!(
                "FAIL device {:?}\n  want {} {:?} {:?}\n  got  {} {:?} {:?}",
                c.user_agent_string,
                c.family,
                c.brand,
                c.model,
                r.device,
                r.device_brand,
                r.device_model
            );
            rep.failed.push(c.user_agent_string.clone());
        }
    }
    rep.finish();
}

/// UA 长度上限（`MAX_UA_BYTES`）对语料没有影响：语料里最长的 UA 不超过上限，
/// 或者超过的那些截断后 uap-core 的结果不变。
#[test]
fn truncation_cap_does_not_change_corpus_results() {
    let parser = UaParser::embedded().unwrap();
    let mut uas: Vec<String> = Vec::new();
    uas.extend(
        load::<UaCase>("uap-test_ua.yaml")
            .into_iter()
            .map(|c| c.user_agent_string),
    );
    uas.extend(
        load::<OsCase>("uap-test_os.yaml")
            .into_iter()
            .map(|c| c.user_agent_string),
    );
    uas.extend(
        load::<DeviceCase>("uap-test_device.yaml")
            .into_iter()
            .map(|c| c.user_agent_string),
    );
    let max = uas.iter().map(String::len).max().unwrap();
    let over: Vec<&String> = uas
        .iter()
        .filter(|u| u.len() > rivu_ua::MAX_UA_BYTES)
        .collect();
    let mut changed = 0;
    for u in &over {
        let cut = &u[..u.floor_char_boundary(rivu_ua::MAX_UA_BYTES)];
        if parser.parse_raw(u) != parser.parse_raw(cut) {
            changed += 1;
        }
    }
    println!(
        "语料 {} 条，最长 {max} 字节，超过 {} 字节的 {} 条，其中截断后结果变化 {changed} 条",
        uas.len(),
        rivu_ua::MAX_UA_BYTES,
        over.len()
    );
    assert_eq!(changed, 0);
}
