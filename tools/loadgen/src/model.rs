//! 事件模型：多站点、Zipf 页面、真实 UA、会话内多次 pageview 与 engagement、referrer / utm。
//!
//! 每个生成器分片维护一个「活跃会话池」：每次要发一个事件时，要么开一个新会话（入口 pageview，带 referrer / utm），
//! 要么从池里随机取一个会话推进一步（engagement 补报上一页、下一页 pageview、偶尔一个自定义事件）。
//! 会话的 IP 与 UA 在会话内不变，所以 rivu 的 visitor / session 分配走真实路径。会话的时间被压缩了
//! （池大小除以事件率就是一个会话的存续时间，通常几十秒），所以「每会话事件数」真实、「会话时长」不真实。

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::sync::Arc;

use rand::rngs::SmallRng;
use rand::{Rng, SeedableRng};
use serde_json::{Value, json};

/// 一个被压测的站点。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Site {
    pub key: String,
    pub id: u64,
    /// hostname 白名单里的精确主机名（事件 URL 的 host 从中取）。
    pub hosts: Vec<String>,
}

impl Site {
    /// 解析 `KEY:ID:host1,host2`。
    pub fn parse(s: &str) -> Result<Site, String> {
        let mut parts = s.splitn(3, ':');
        let (key, id, hosts) = (parts.next(), parts.next(), parts.next());
        let (Some(key), Some(id), Some(hosts)) = (key, id, hosts) else {
            return Err(format!("`{s}` 不是 KEY:ID:host1,host2"));
        };
        let id: u64 = id.parse().map_err(|_| format!("`{id}` 不是站点 id"))?;
        let hosts: Vec<String> = hosts
            .split(',')
            .map(str::trim)
            .filter(|h| !h.is_empty())
            .map(str::to_owned)
            .collect();
        if key.is_empty() || hosts.is_empty() {
            return Err(format!("`{s}` 缺少 key 或 hostname"));
        }
        Ok(Site {
            key: key.to_owned(),
            id,
            hosts,
        })
    }
}

/// Zipf 分布：排名 `k`（从 0 起）的概率正比于 `1 / (k + 1)^s`。预先算好累积分布，二分采样。
#[derive(Clone, Debug)]
pub struct Zipf {
    cdf: Vec<f64>,
}

impl Zipf {
    pub fn new(n: usize, s: f64) -> Zipf {
        assert!(n > 0);
        let mut cdf = Vec::with_capacity(n);
        let mut acc = 0.0;
        for k in 0..n {
            acc += 1.0 / ((k + 1) as f64).powf(s);
            cdf.push(acc);
        }
        for c in &mut cdf {
            *c /= acc;
        }
        Zipf { cdf }
    }

    pub fn sample(&self, rng: &mut impl Rng) -> usize {
        let u: f64 = rng.random();
        self.cdf.partition_point(|&c| c < u).min(self.cdf.len() - 1)
    }
}

/// 页面排名 -> pathname。前几名是固定的常见页面，其余按三类模板展开。
pub fn page_path(rank: usize) -> String {
    match rank {
        0 => "/".to_owned(),
        1 => "/pricing".to_owned(),
        2 => "/blog".to_owned(),
        3 => "/docs".to_owned(),
        4 => "/about".to_owned(),
        5 => "/login".to_owned(),
        _ => match rank % 3 {
            0 => format!("/blog/post-{rank}"),
            1 => format!("/docs/guide/page-{rank}"),
            _ => format!("/products/item-{rank}"),
        },
    }
}

/// 从 uap-core 测试集（`uap-test_ua.yaml`、`uap-test_device.yaml`）里取真实浏览器 UA：只要 `Mozilla/` 开头、
/// 不含明显的机器人字样（rivu 会丢弃机器人，压测要的是被接受的流量；剩下漏网的机器人由 rivu 自己丢）。
pub fn load_user_agents(dir: &std::path::Path) -> anyhow::Result<Vec<String>> {
    #[derive(serde::Deserialize)]
    struct Case {
        user_agent_string: String,
    }
    #[derive(serde::Deserialize)]
    struct Cases {
        test_cases: Vec<Case>,
    }
    let mut out = std::collections::BTreeSet::new();
    for file in ["uap-test_ua.yaml", "uap-test_device.yaml"] {
        let text = std::fs::read_to_string(dir.join(file))
            .map_err(|e| anyhow::anyhow!("读取 {}：{e}", dir.join(file).display()))?;
        let cases: Cases =
            serde_saphyr::from_str(&text).map_err(|e| anyhow::anyhow!("解析 {file}：{e}"))?;
        for c in cases.test_cases {
            if is_browser_like(&c.user_agent_string) {
                out.insert(c.user_agent_string);
            }
        }
    }
    anyhow::ensure!(!out.is_empty(), "{} 里没有可用的 UA", dir.display());
    Ok(out.into_iter().collect())
}

fn is_browser_like(ua: &str) -> bool {
    if !ua.starts_with("Mozilla/") || ua.len() < 40 || ua.len() > 400 || !ua.is_ascii() {
        return false;
    }
    let lower = ua.to_ascii_lowercase();
    ![
        "bot", "spider", "crawl", "slurp", "fetch", "monitor", "preview",
    ]
    .iter()
    .any(|w| lower.contains(w))
}

fn is_public_v4(a: Ipv4Addr) -> bool {
    let o = a.octets();
    !(o[0] == 0
        || o[0] == 10
        || o[0] == 127
        || o[0] >= 224
        || (o[0] == 100 && (64..128).contains(&o[1]))
        || (o[0] == 169 && o[1] == 254)
        || (o[0] == 172 && (16..32).contains(&o[1]))
        || (o[0] == 192 && o[1] == 168)
        || (o[0] == 192 && o[1] == 0)
        || (o[0] == 198 && (18..20).contains(&o[1])))
}

pub fn random_ip(rng: &mut impl Rng) -> IpAddr {
    if rng.random_bool(0.1) {
        let seg = |rng: &mut dyn rand::RngCore| (rng.next_u32() & 0xffff) as u16;
        return IpAddr::V6(Ipv6Addr::new(
            0x2a00 + (rng.next_u32() & 0x3ff) as u16,
            seg(rng),
            seg(rng),
            seg(rng),
            seg(rng),
            seg(rng),
            seg(rng),
            seg(rng),
        ));
    }
    loop {
        let a = Ipv4Addr::from(rng.next_u32());
        if is_public_v4(a) {
            return IpAddr::V4(a);
        }
    }
}

/// 生成的一个事件。
#[derive(Clone, Debug)]
pub struct Generated {
    pub body: String,
    pub ip: IpAddr,
    pub ua_idx: usize,
    // 下面两个字段只有单元测试读
    #[allow(dead_code)]
    pub site_idx: usize,
    #[allow(dead_code)]
    pub kind: Kind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Pageview,
    Engagement,
    Custom,
}

struct Session {
    site_idx: usize,
    ip: IpAddr,
    ua_idx: usize,
    /// 当前页面的 URL（不含 utm），engagement 要用同一个。
    url: String,
    pages_left: u32,
    pending_engagement: bool,
}

/// 一个生成器分片。分片之间互不共享状态，各自带独立的随机数种子。
pub struct Generator {
    rng: SmallRng,
    sites: Arc<Vec<Site>>,
    site_zipf: Arc<Zipf>,
    page_zipf: Arc<Zipf>,
    ua_count: usize,
    pool: Vec<Session>,
    target_pool: usize,
}

const SEARCH_REFERRERS: [&str; 4] = [
    "https://www.google.com/",
    "https://www.bing.com/",
    "https://duckduckgo.com/",
    "https://www.baidu.com/link?url=abc",
];
const SOCIAL_REFERRERS: [&str; 5] = [
    "https://t.co/Xk29sd",
    "https://www.facebook.com/",
    "https://news.ycombinator.com/item?id=41234567",
    "https://www.reddit.com/r/rust/comments/abc/",
    "https://github.com/rivu/rivu",
];
const UTM_CAMPAIGNS: [(&str, &str, &str); 4] = [
    ("newsletter", "email", "autumn-launch"),
    ("google", "cpc", "brand-search"),
    ("twitter", "social", "release-notes"),
    ("partner", "referral", "affiliate-q4"),
];

impl Generator {
    pub fn new(
        seed: u64,
        sites: Arc<Vec<Site>>,
        site_zipf: Arc<Zipf>,
        page_zipf: Arc<Zipf>,
        ua_count: usize,
        target_pool: usize,
    ) -> Generator {
        Generator {
            rng: SmallRng::seed_from_u64(seed),
            sites,
            site_zipf,
            page_zipf,
            ua_count,
            pool: Vec::new(),
            target_pool,
        }
    }

    fn host(&mut self, site_idx: usize) -> String {
        let hosts = &self.sites[site_idx].hosts;
        let h = &hosts[self.rng.random_range(0..hosts.len())];
        // 一部分流量带 www.：rivu 会规范化掉
        if self.rng.random_bool(0.15) && !h.starts_with("www.") {
            format!("www.{h}")
        } else {
            h.clone()
        }
    }

    /// 入口 pageview 的 referrer 与查询串。
    fn entry_attribution(&mut self) -> (Option<String>, String) {
        let r: f64 = self.rng.random();
        if r < 0.55 {
            (None, String::new())
        } else if r < 0.75 {
            let i = self.rng.random_range(0..SEARCH_REFERRERS.len());
            (Some(SEARCH_REFERRERS[i].to_owned()), String::new())
        } else if r < 0.87 {
            let i = self.rng.random_range(0..SOCIAL_REFERRERS.len());
            (Some(SOCIAL_REFERRERS[i].to_owned()), String::new())
        } else if r < 0.95 {
            let (s, m, c) = UTM_CAMPAIGNS[self.rng.random_range(0..UTM_CAMPAIGNS.len())];
            (
                None,
                format!("?utm_source={s}&utm_medium={m}&utm_campaign={c}"),
            )
        } else if r < 0.99 {
            let n: u32 = self.rng.random_range(1..500);
            (
                Some(format!("https://blog.example.net/post/{n}")),
                String::new(),
            )
        } else {
            (Some("https://semalt.com/".to_owned()), String::new())
        }
    }

    fn new_session(&mut self) -> Generated {
        let site_idx = self.site_zipf.sample(&mut self.rng);
        let host = self.host(site_idx);
        let path = page_path(self.page_zipf.sample(&mut self.rng));
        let (referrer, query) = self.entry_attribution();
        let url = format!("https://{host}{path}");
        let mut pages_left = 1;
        while pages_left < 12 && self.rng.random_bool(0.67) {
            pages_left += 1;
        }
        let ip = random_ip(&mut self.rng);
        let ua_idx = self.rng.random_range(0..self.ua_count);
        let mut body = json!({
            "v": 1, "sv": 1, "s": self.sites[site_idx].key, "n": "pageview",
            "u": format!("{url}{query}"),
        });
        if let Some(r) = referrer {
            body["r"] = Value::String(r);
        }
        let pending = self.rng.random_bool(0.85);
        self.pool.push(Session {
            site_idx,
            ip,
            ua_idx,
            url,
            pages_left: pages_left - 1,
            pending_engagement: pending,
        });
        Generated {
            body: body.to_string(),
            ip,
            ua_idx,
            site_idx,
            kind: Kind::Pageview,
        }
    }

    /// 下一个事件。
    pub fn next(&mut self) -> Generated {
        let q = if self.pool.len() < self.target_pool {
            0.5
        } else {
            0.05
        };
        if self.pool.is_empty() || self.rng.random_bool(q) {
            return self.new_session();
        }
        let idx = self.rng.random_range(0..self.pool.len());
        if self.pool[idx].pending_engagement {
            let sd: u32 = self.rng.random_range(5..=100);
            let e: u32 = self.rng.random_range(500..120_000);
            let s = &mut self.pool[idx];
            s.pending_engagement = false;
            let body = json!({
                "v": 1, "sv": 1, "s": self.sites[s.site_idx].key, "n": "engagement",
                "u": s.url, "sd": sd, "e": e,
            });
            let out = Generated {
                body: body.to_string(),
                ip: s.ip,
                ua_idx: s.ua_idx,
                site_idx: s.site_idx,
                kind: Kind::Engagement,
            };
            if s.pages_left == 0 {
                self.pool.swap_remove(idx);
            }
            return out;
        }
        if self.pool[idx].pages_left == 0 {
            self.pool.swap_remove(idx);
            return self.new_session();
        }
        if self.rng.random_bool(0.04) {
            let s = &self.pool[idx];
            let body = json!({
                "v": 1, "sv": 1, "s": self.sites[s.site_idx].key, "n": "Signup",
                "u": s.url, "p": {"plan": "pro", "source": "pricing"},
            });
            return Generated {
                body: body.to_string(),
                ip: s.ip,
                ua_idx: s.ua_idx,
                site_idx: s.site_idx,
                kind: Kind::Custom,
            };
        }
        let site_idx = self.pool[idx].site_idx;
        let host = self.host(site_idx);
        let path = page_path(self.page_zipf.sample(&mut self.rng));
        let pending = self.rng.random_bool(0.85);
        let s = &mut self.pool[idx];
        let prev = s.url.clone();
        s.url = format!("https://{host}{path}");
        s.pages_left -= 1;
        s.pending_engagement = pending;
        let body = json!({
            "v": 1, "sv": 1, "s": self.sites[site_idx].key, "n": "pageview",
            "u": s.url, "r": prev,
        });
        Generated {
            body: body.to_string(),
            ip: s.ip,
            ua_idx: s.ua_idx,
            site_idx,
            kind: Kind::Pageview,
        }
    }

    #[cfg(test)]
    pub fn pool_len(&self) -> usize {
        self.pool.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sites() -> Arc<Vec<Site>> {
        Arc::new(vec![
            Site::parse("k7Qm2xV9aTbR4cLd:1:a.test,b.test").unwrap(),
            Site::parse("Zx81mQ2pLk9TnB3w:2:c.test").unwrap(),
        ])
    }

    fn generator(seed: u64) -> Generator {
        Generator::new(
            seed,
            sites(),
            Arc::new(Zipf::new(2, 1.0)),
            Arc::new(Zipf::new(2000, 1.1)),
            50,
            200,
        )
    }

    #[test]
    fn site_parse_accepts_and_rejects() {
        let s = Site::parse("abc:7:x.com, y.com").unwrap();
        assert_eq!((s.key.as_str(), s.id, s.hosts.len()), ("abc", 7, 2));
        assert!(Site::parse("abc:7").is_err());
        assert!(Site::parse("abc:x:h.com").is_err());
        assert!(Site::parse(":1:h.com").is_err());
    }

    #[test]
    fn zipf_is_monotone_and_heavy_headed() {
        let z = Zipf::new(1000, 1.1);
        let mut rng = SmallRng::seed_from_u64(1);
        let mut counts = vec![0u32; 1000];
        for _ in 0..200_000 {
            counts[z.sample(&mut rng)] += 1;
        }
        assert!(
            counts[0] > counts[1] && counts[1] > counts[9],
            "{:?}",
            &counts[..10]
        );
        // 前 10 名占大头，且尾部也被采到
        let head: u32 = counts[..10].iter().sum();
        assert!(head > 60_000, "{head}");
        assert!(counts[500..].iter().any(|&c| c > 0));
        // 排名 1 与排名 2 的比值约为 2^1.1
        let ratio = f64::from(counts[0]) / f64::from(counts[1]);
        assert!((1.9..2.4).contains(&ratio), "{ratio}");
    }

    #[test]
    fn events_are_valid_protocol_json_with_session_structure() {
        let mut g = generator(7);
        let (mut pv, mut eng, mut custom) = (0, 0, 0);
        for _ in 0..20_000 {
            let e = g.next();
            let v: Value = serde_json::from_str(&e.body).unwrap();
            assert_eq!(v["v"], 1);
            assert!(v["s"].as_str().unwrap().len() >= 12);
            let u = v["u"].as_str().unwrap();
            assert!(u.starts_with("https://"), "{u}");
            let host_ok = sites()[e.site_idx]
                .hosts
                .iter()
                .any(|h| u.contains(&format!("//{h}/")) || u.contains(&format!("//www.{h}/")));
            assert!(host_ok, "{u}");
            match e.kind {
                Kind::Pageview => {
                    pv += 1;
                    assert_eq!(v["n"], "pageview");
                }
                Kind::Engagement => {
                    eng += 1;
                    assert_eq!(v["n"], "engagement");
                    assert!(v["sd"].as_u64().unwrap() <= 100);
                    assert!(v["e"].as_u64().unwrap() < 2_592_000_000);
                }
                Kind::Custom => {
                    custom += 1;
                    assert_eq!(v["n"], "Signup");
                }
            }
        }
        assert!(g.pool_len() <= 400, "{}", g.pool_len());
        // 会话内 pageview 多于一次：engagement 与 pageview 的比例接近 0.8，自定义事件几个百分点
        assert!(eng * 10 > pv * 6 && eng * 10 < pv * 11, "pv {pv} eng {eng}");
        assert!(custom > 100 && custom * 10 < pv, "custom {custom}");
    }

    #[test]
    fn every_generated_event_passes_the_real_protocol_parser() {
        let mut g = generator(11);
        for _ in 0..20_000 {
            let e = g.next();
            let ev = rivu_ingest::protocol::parse_event(e.body.as_bytes())
                .unwrap_or_else(|err| panic!("{}: {}", e.body, err.to_json()));
            assert!(e.body.len() < rivu_ingest::protocol::MAX_BODY_BYTES);
            assert!(ev.pathname.starts_with('/'));
        }
    }

    #[test]
    fn same_seed_same_events_and_sessions_keep_ip_and_ua() {
        let (mut a, mut b) = (generator(3), generator(3));
        let (mut by_key, mut seen) = (std::collections::HashMap::new(), 0);
        for _ in 0..2000 {
            let (x, y) = (a.next(), b.next());
            assert_eq!(x.body, y.body);
            assert_eq!(x.ip, y.ip);
            // 同一个 IP 永远对应同一个 UA（会话内不变）
            let prev = by_key.insert(x.ip, x.ua_idx);
            if let Some(p) = prev {
                assert_eq!(p, x.ua_idx);
                seen += 1;
            }
        }
        assert!(seen > 500, "{seen}");
    }

    #[test]
    fn random_ips_are_public() {
        let mut rng = SmallRng::seed_from_u64(9);
        for _ in 0..20_000 {
            match random_ip(&mut rng) {
                IpAddr::V4(a) => assert!(is_public_v4(a), "{a}"),
                IpAddr::V6(a) => assert!(a.segments()[0] >= 0x2a00),
            }
        }
    }

    #[test]
    fn browser_filter_drops_bots_and_short_strings() {
        assert!(is_browser_like(
            "Mozilla/5.0 (X11; Linux x86_64; rv:126.0) Gecko/20100101 Firefox/126.0"
        ));
        assert!(!is_browser_like(
            "Mozilla/5.0 (compatible; Googlebot/2.1; +http://www.google.com/bot.html)"
        ));
        assert!(!is_browser_like("Luminary/1.0"));
    }

    #[test]
    fn user_agents_load_from_the_uap_core_test_data() {
        let dir =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../crates/ua/tests/data");
        let uas = load_user_agents(&dir).unwrap();
        assert!(uas.len() > 500, "{}", uas.len());
        assert!(uas.iter().all(|u| u.starts_with("Mozilla/")));
    }
}
