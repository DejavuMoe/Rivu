//! 把 `data/` 里的来源数据在编译期转成静态查找表（`$OUT_DIR/tables.rs`）。
//!
//! 为什么用 build.rs 而不是启动时解析 YAML：
//! - 运行时没有 YAML 依赖、没有解析成本、没有「数据文件缺失或损坏」这类启动期失败；
//! - 表是排好序的 `&'static [(&str, u16)]`，查询是二分，不需要任何分配或初始化；
//! - 数据文件的 SHA-256 在这里核对：上游文件被改动（包括换行转换）会让构建失败，
//!   升级数据必须同时改这里的哈希、`RULES_VERSION` 和研究文档，不会悄悄漂移。
//!
//! 合并规则（先登记的赢，冲突不报错，由 `tests/source_data.rs` 逐条列出并断言取舍）：
//! 补充表 `supplement.yml` > Matomo AIAssistants > Socials > SearchEngines。

use std::collections::BTreeMap;
use std::env;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use sha2::{Digest, Sha256};

/// 上游数据文件（Matomo searchengine-and-social-list，固定到 commit 4254b8d）的 SHA-256。
/// 哈希在把 CRLF 归一成 LF 之后计算。
const PINNED: [(&str, &str); 4] = [
    (
        "data/matomo/AIAssistants.yml",
        "d4713bb4905e61eed35b89f29fe3808d0464cd269c48f9d37a1d3ee86a078ab5",
    ),
    (
        "data/matomo/Socials.yml",
        "2e9c40cac1e3a976cad8101e0892895a6b865618930018633c9f08a4b2f3cb51",
    ),
    (
        "data/matomo/SearchEngines.yml",
        "809c1733728e258cf8f8c63233132a0aae2fad14fd855ee2b190938f044ac422",
    ),
    (
        "data/matomo/LICENSE",
        "36ffd9dc085d529a7e60e1276d73ae5a030b020313e6c5408593a6ae2af39673",
    ),
];

#[derive(Deserialize)]
struct Supplement {
    #[serde(default)]
    renames: BTreeMap<String, String>,
    entries: Vec<SupEntry>,
}

#[derive(Deserialize)]
struct SupEntry {
    name: String,
    kind: String,
    #[serde(default)]
    domains: Vec<String>,
    #[serde(default)]
    aliases: Vec<String>,
    #[serde(default)]
    paid_aliases: Vec<String>,
    basis: String,
}

#[derive(Deserialize)]
struct SearchConfig {
    urls: Vec<String>,
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Kind {
    Search,
    Social,
    Video,
    Shopping,
    Ai,
    Email,
    Other,
}

impl Kind {
    fn parse(s: &str) -> Kind {
        match s {
            "search" => Kind::Search,
            "social" => Kind::Social,
            "video" => Kind::Video,
            "shopping" => Kind::Shopping,
            "ai" => Kind::Ai,
            "email" => Kind::Email,
            "other" => Kind::Other,
            _ => panic!("supplement.yml: 未知 kind {s:?}"),
        }
    }

    fn variant(self) -> &'static str {
        match self {
            Kind::Search => "Search",
            Kind::Social => "Social",
            Kind::Video => "Video",
            Kind::Shopping => "Shopping",
            Kind::Ai => "Ai",
            Kind::Email => "Email",
            Kind::Other => "Other",
        }
    }
}

/// 域名键的两种形态。
enum Key {
    /// 后缀键：`google.co.jp` 命中 `google.co.jp` 及其所有子域。
    Suffix(String),
    /// 结尾带 `.{}` 的国家/地区后缀占位符：保存 `{}` 之前的部分（`google`）。
    TldStem(String),
}

#[derive(Default)]
struct Tables {
    entries: Vec<(String, Kind)>,
    entry_ids: BTreeMap<(String, Kind), u16>,
    hosts: BTreeMap<String, u16>,
    stems: BTreeMap<String, u16>,
    aliases: BTreeMap<String, (u16, bool)>,
}

impl Tables {
    fn entry(&mut self, name: &str, kind: Kind) -> u16 {
        if let Some(&id) = self.entry_ids.get(&(name.to_owned(), kind)) {
            return id;
        }
        let id = u16::try_from(self.entries.len()).expect("条目数超过 u16");
        self.entries.push((name.to_owned(), kind));
        self.entry_ids.insert((name.to_owned(), kind), id);
        id
    }

    fn domain(&mut self, raw: &str, id: u16) {
        match normalize_domain(raw) {
            Some(Key::Suffix(k)) => {
                self.hosts.entry(k).or_insert(id);
            }
            Some(Key::TldStem(k)) => {
                self.stems.entry(k).or_insert(id);
            }
            None => {}
        }
    }

    fn alias(&mut self, token: &str, id: u16, paid: bool) {
        let key = token.trim().to_lowercase();
        assert!(!key.is_empty(), "空别名");
        self.aliases.entry(key).or_insert((id, paid));
    }
}

/// 数据里的域名 → 表里的键。带路径的条目（`google.com/products`、`x.com/i/grok`）返回 `None`：
/// 分类接口只有 host，没有路径，把它们降级成 host 会让整个站点变成一个子产品。
///
/// `{}` 的处理（`docs/research/enrich-source.md` §4.3）：
/// - 结尾的 `.{}` 是国家/地区后缀占位符，保存成 [`Key::TldStem`]；
/// - 其他位置的 `{}` 是国家子域名（`{}.bing.com`、`images.{}.ask.com`），
///   由后缀匹配天然覆盖，只保留它后面那一段（`bing.com`、`ask.com`）。
fn normalize_domain(raw: &str) -> Option<Key> {
    let mut s = raw.trim().to_lowercase();
    if s.is_empty() || s.contains('/') {
        return None;
    }
    let trailing = s.ends_with(".{}");
    if trailing {
        s.truncate(s.len() - 3);
    }
    if let Some(i) = s.rfind("{}.") {
        s = s[i + 3..].to_owned();
    }
    while let Some(rest) = s.strip_prefix("www.") {
        s = rest.to_owned();
    }
    if s.is_empty() || s.contains("{}") {
        return None;
    }
    Some(if trailing {
        Key::TldStem(s)
    } else {
        Key::Suffix(s)
    })
}

fn read_normalized(path: &Path) -> String {
    let text = fs::read_to_string(path).unwrap_or_else(|e| panic!("读取 {}: {e}", path.display()));
    text.replace("\r\n", "\n")
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().fold(String::new(), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

fn sha256_hex(data: &[u8]) -> String {
    hex(&Sha256::digest(data))
}

fn main() {
    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=data");

    // 1. 核对上游文件哈希，同时算整体指纹（补充表也计入）。
    let mut fingerprint = Sha256::new();
    for (rel, want) in PINNED {
        let text = read_normalized(&manifest.join(rel));
        let got = sha256_hex(text.as_bytes());
        assert_eq!(
            got, want,
            "{rel} 的 SHA-256 与固定值不符：上游数据只能通过升级流程更新\
             （改 build.rs 的哈希、PROVENANCE.md、RULES_VERSION 和研究文档）"
        );
        fingerprint.update(rel.as_bytes());
        fingerprint.update(text.as_bytes());
    }
    let supplement_text = read_normalized(&manifest.join("data/supplement.yml"));
    fingerprint.update(b"data/supplement.yml");
    fingerprint.update(supplement_text.as_bytes());
    let fingerprint = hex(&fingerprint.finalize());

    // 2. 解析。
    let supplement: Supplement = serde_saphyr::from_str(&supplement_text)
        .unwrap_or_else(|e| panic!("解析 supplement.yml: {e}"));
    let ai: BTreeMap<String, Vec<String>> = serde_saphyr::from_str(&read_normalized(
        &manifest.join("data/matomo/AIAssistants.yml"),
    ))
    .expect("解析 AIAssistants.yml");
    let social: BTreeMap<String, Vec<String>> =
        serde_saphyr::from_str(&read_normalized(&manifest.join("data/matomo/Socials.yml")))
            .expect("解析 Socials.yml");
    let search: BTreeMap<String, Vec<SearchConfig>> = serde_saphyr::from_str(&read_normalized(
        &manifest.join("data/matomo/SearchEngines.yml"),
    ))
    .expect("解析 SearchEngines.yml");

    // 3. 合并：补充表 > AI > 社交 > 搜索；先登记的赢。
    let mut t = Tables::default();
    for e in &supplement.entries {
        assert!(
            !e.basis.trim().is_empty(),
            "补充条目 {:?} 缺少 basis",
            e.name
        );
        let id = t.entry(&e.name, Kind::parse(&e.kind));
        for d in &e.domains {
            t.domain(d, id);
        }
        t.alias(&e.name, id, false);
        for a in &e.aliases {
            t.alias(a, id, false);
        }
        for a in &e.paid_aliases {
            t.alias(a, id, true);
        }
    }
    let rename = |name: &str| -> String {
        supplement
            .renames
            .get(name)
            .cloned()
            .unwrap_or_else(|| name.to_owned())
    };
    for (name, urls) in &ai {
        let name = rename(name);
        let id = t.entry(&name, Kind::Ai);
        t.alias(&name, id, false);
        for u in urls {
            t.domain(u, id);
        }
    }
    for (name, urls) in &social {
        let name = rename(name);
        let id = t.entry(&name, Kind::Social);
        t.alias(&name, id, false);
        for u in urls {
            t.domain(u, id);
        }
    }
    for (name, configs) in &search {
        let name = rename(name);
        let id = t.entry(&name, Kind::Search);
        t.alias(&name, id, false);
        for c in configs {
            for u in &c.urls {
                t.domain(u, id);
            }
        }
    }

    // 4. 生成。BTreeMap 的迭代顺序是字节序，与运行时二分用的比较一致。
    let mut out = String::new();
    out.push_str("// 由 build.rs 生成，不要手改。\n");
    let _ = writeln!(out, "pub const DATA_FINGERPRINT: &str = {fingerprint:?};");
    out.push_str("pub(crate) static ENTRIES: &[Entry] = &[\n");
    for (name, kind) in &t.entries {
        let _ = writeln!(
            out,
            "    Entry {{ name: {name:?}, kind: Kind::{} }},",
            kind.variant()
        );
    }
    out.push_str("];\n");
    out.push_str("pub(crate) static HOSTS: &[(&str, u16)] = &[\n");
    for (k, id) in &t.hosts {
        let _ = writeln!(out, "    ({k:?}, {id}),");
    }
    out.push_str("];\n");
    out.push_str("pub(crate) static TLD_STEMS: &[(&str, u16)] = &[\n");
    for (k, id) in &t.stems {
        let _ = writeln!(out, "    ({k:?}, {id}),");
    }
    out.push_str("];\n");
    out.push_str("pub(crate) static ALIASES: &[(&str, u16, bool)] = &[\n");
    for (k, (id, paid)) in &t.aliases {
        let _ = writeln!(out, "    ({k:?}, {id}, {paid}),");
    }
    out.push_str("];\n");

    let dest = PathBuf::from(env::var("OUT_DIR").unwrap()).join("tables.rs");
    fs::write(dest, out).expect("写 tables.rs");
}
