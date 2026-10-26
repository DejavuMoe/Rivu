//! 真实下载默认的数据中心网段来源（需要网络，默认忽略）：
//! `cargo test -p rivu-geo --test live_datacenter -- --ignored --nocapture`
//!
//! 核对每个来源都能下载、格式能识别、网段数在合理范围内，合并后的文件能加载并命中各家已知的地址。

use std::net::IpAddr;

use rivu_common::config::DatacenterConfig;
use rivu_geo::HttpClient;
use rivu_geo::datacenter::{DcDb, refresh};

#[tokio::test]
#[ignore = "需要网络"]
async fn default_sources_download_and_parse() {
    let dir = std::env::temp_dir().join(format!("rivu-live-dc-{}", std::process::id()));
    let path = dir.join("ranges.txt");
    let sources: Vec<String> = DatacenterConfig::DEFAULT_SOURCES
        .iter()
        .map(|s| (*s).to_owned())
        .collect();
    let http = HttpClient::new().unwrap();
    let report = refresh(&http, &sources, &path)
        .await
        .expect("全部来源都应可用");
    for (url, n) in sources.iter().zip(&report.per_source) {
        println!("{n:>6}  {url}");
        assert!(*n >= 20, "{url} 只有 {n} 条");
    }
    println!("合并去重后 {} 条", report.prefixes);

    let db = DcDb::new();
    assert!(db.reload_if_changed(&path).unwrap());
    // 各来源的清单里各取一条，地址落在其中
    let text = std::fs::read_to_string(&path).unwrap();
    let sample: Vec<&str> = text
        .lines()
        .filter(|l| !l.starts_with('#'))
        .step_by(97)
        .collect();
    for p in &sample {
        let addr: IpAddr = p.split('/').next().unwrap().parse().unwrap();
        assert!(db.contains(addr), "{p}");
    }
    // 常见的家庭宽带 / 公共 DNS 不在其中
    for ip in ["1.1.1.1", "9.9.9.9", "114.114.114.114", "223.5.5.5"] {
        assert!(!db.contains(ip.parse().unwrap()), "{ip}");
    }
    std::fs::remove_dir_all(dir).unwrap();
}
