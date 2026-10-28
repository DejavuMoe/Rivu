//! Plausible 数据导出的导入（`rivu_jobs::import_plausible`）：合成一个小导出，导入、查询、再删除。
//!
//! - 合计、各维度族的数字与 CSV 一致，走日汇总层（`meta.tiers = ["rollup"]`）；
//! - 名字换成 Rivu 的写法（设备、浏览器、系统），来源与渠道按 Rivu 的规则判定，`engagement` 事件不导入；
//! - 属性由汇总层回答；与汇总层不兼容的查询带 `imported_data_excluded` 提示；
//! - 同一段日期再导入一次被拒绝；删除导入后汇总行与记录都没有了。
//!
//! 需要真实的 PostgreSQL 和 ClickHouse，缺变量时跳过（同 `query_differential.rs`）。

use std::io::Write;

use jiff::Timestamp;
use jiff::tz::TimeZone;
use rivu::migrate;
use rivu_common::config::{ChAccount, Config, Env, Role, Secret};
use rivu_query::plan::Tier;
use rivu_query::result::QueryResult;
use rivu_query::{ExecOptions, QuerySpec, SiteContext, TenantScope, execute};
use rivu_storage_pg::sites::NewSite;
use serde_json::{Value, json};

fn var(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.is_empty())
}

fn account(role: &str) -> ChAccount {
    let upper = role.to_uppercase();
    ChAccount {
        user: var(&format!("RIVU_TEST_CH_{upper}_USER")).unwrap_or_else(|| format!("rivu_{role}")),
        password: Secret::new(
            var(&format!("RIVU_TEST_CH_{upper}_PASSWORD"))
                .unwrap_or_else(|| format!("rivu_dev_{role}")),
        ),
    }
}

fn ch_database() -> String {
    var("RIVU_TEST_CH_DATABASE").unwrap_or_else(|| "rivu".into())
}

/// 三天的合成导出（列与 Plausible CE v3 的导出相同）。
const FILES: [(&str, &str); 11] = [
    (
        "visitors",
        "\"date\",\"visitors\",\"pageviews\",\"bounces\",\"visits\",\"visit_duration\"\n\
         \"2026-06-01\",10,20,5,12,600\n\"2026-06-02\",8,15,4,9,300\n\"2026-06-03\",5,7,2,6,120\n",
    ),
    (
        "sources",
        "\"date\",\"source\",\"referrer\",\"utm_source\",\"utm_medium\",\"utm_campaign\",\"utm_content\",\"utm_term\",\"pageviews\",\"visitors\",\"visits\",\"visit_duration\",\"bounces\"\n\
         \"2026-06-01\",\"Bing\",\"cn.bing.com\",\"\",\"\",\"\",\"\",\"\",12,6,7,400,3\n\
         \"2026-06-01\",\"\",\"\",\"\",\"\",\"\",\"\",\"\",8,4,5,200,2\n\
         \"2026-06-02\",\"Bing\",\"www.bing.com\",\"\",\"\",\"\",\"\",\"\",15,8,9,300,4\n\
         \"2026-06-03\",\"newsletter\",\"\",\"newsletter\",\"email\",\"spring\",\"\",\"\",7,5,6,120,2\n",
    ),
    (
        "pages",
        "\"date\",\"hostname\",\"page\",\"visits\",\"visitors\",\"pageviews\",\"total_scroll_depth\",\"total_scroll_depth_visits\",\"total_time_on_page\",\"total_time_on_page_visits\"\n\
         \"2026-06-01\",\"example.test\",\"/\",8,7,12,300,6,240,6\n\
         \"2026-06-01\",\"www.example.test\",\"/\",1,1,1,50,1,10,1\n\
         \"2026-06-01\",\"example.test\",\"/docs\",5,4,7,200,4,120,4\n\
         \"2026-06-02\",\"example.test\",\"/\",9,8,15,400,8,300,8\n\
         \"2026-06-03\",\"example.test\",\"/docs\",6,5,7,350,5,150,5\n",
    ),
    (
        "entry_pages",
        "\"date\",\"entry_page\",\"visitors\",\"entrances\",\"visit_duration\",\"bounces\",\"pageviews\"\n\
         \"2026-06-01\",\"/\",8,9,500,4,15\n\"2026-06-01\",\"/docs\",3,3,100,1,5\n\
         \"2026-06-02\",\"/\",8,9,300,4,15\n\"2026-06-03\",\"/docs\",5,6,120,2,7\n",
    ),
    (
        "exit_pages",
        "\"date\",\"exit_page\",\"visitors\",\"visit_duration\",\"exits\",\"bounces\",\"pageviews\"\n\
         \"2026-06-01\",\"/\",6,300,7,3,10\n\"2026-06-01\",\"/docs\",4,300,5,2,10\n\
         \"2026-06-02\",\"/\",8,300,9,4,15\n\"2026-06-03\",\"/docs\",5,120,6,2,7\n",
    ),
    (
        "locations",
        "\"date\",\"country\",\"region\",\"city\",\"visitors\",\"visits\",\"visit_duration\",\"bounces\",\"pageviews\"\n\
         \"2026-06-01\",\"CN\",\"CN-GD\",1809858,6,7,400,3,12\n\
         \"2026-06-01\",\"US\",\"\",0,4,5,200,2,8\n\
         \"2026-06-02\",\"CN\",\"CN-GD\",1809858,8,9,300,4,15\n\
         \"2026-06-03\",\"CN\",\"\",0,1,1,0,1,0\n\
         \"2026-06-03\",\"US\",\"\",0,4,5,120,1,7\n",
    ),
    (
        "devices",
        "\"date\",\"device\",\"visitors\",\"visits\",\"visit_duration\",\"bounces\",\"pageviews\"\n\
         \"2026-06-01\",\"Desktop\",7,8,500,3,15\n\"2026-06-01\",\"Mobile\",3,4,100,2,5\n\
         \"2026-06-02\",\"Desktop\",8,9,300,4,15\n\"2026-06-03\",\"Tablet\",5,6,120,2,7\n",
    ),
    (
        "browsers",
        "\"date\",\"browser\",\"browser_version\",\"visitors\",\"visits\",\"visit_duration\",\"bounces\",\"pageviews\"\n\
         \"2026-06-01\",\"Microsoft Edge\",\"138.0\",10,12,600,5,20\n\
         \"2026-06-02\",\"Chrome\",\"138.0\",8,9,300,4,15\n\"2026-06-03\",\"Chrome\",\"139.0\",5,6,120,2,7\n",
    ),
    (
        "operating_systems",
        "\"date\",\"operating_system\",\"operating_system_version\",\"visitors\",\"visits\",\"visit_duration\",\"bounces\",\"pageviews\"\n\
         \"2026-06-01\",\"Mac\",\"14\",10,12,600,5,20\n\
         \"2026-06-02\",\"GNU/Linux\",\"\",8,9,300,4,15\n\"2026-06-03\",\"Windows\",\"10\",5,6,120,2,7\n",
    ),
    (
        "custom_events",
        "\"date\",\"name\",\"link_url\",\"path\",\"visitors\",\"events\"\n\
         \"2026-06-01\",\"Outbound Link: Click\",\"https://a.test/\",\"\",3,4\n\
         \"2026-06-01\",\"Outbound Link: Click\",\"https://b.test/\",\"\",1,1\n\
         \"2026-06-01\",\"engagement\",\"\",\"\",9,30\n\
         \"2026-06-02\",\"Signup\",\"\",\"\",2,2\n",
    ),
    (
        "custom_props",
        "\"date\",\"property\",\"value\",\"visitors\",\"events\"\n\
         \"2026-06-01\",\"url\",\"https://a.test/\",3,4\n\"2026-06-01\",\"url\",\"https://b.test/\",1,1\n\
         \"2026-06-02\",\"plan\",\"pro\",2,2\n",
    ),
];

fn write_zip(path: &std::path::Path) {
    let file = std::fs::File::create(path).unwrap();
    let mut zip = zip::ZipWriter::new(file);
    let opts = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    for (family, body) in FILES {
        zip.start_file(format!("imported_{family}_20260601_20260603.csv"), opts)
            .unwrap();
        zip.write_all(body.as_bytes()).unwrap();
    }
    zip.finish().unwrap();
}

async fn run(reader: &clickhouse::Client, site: &SiteContext, spec: Value) -> QueryResult {
    execute(
        reader,
        &TenantScope::new(site.site_id),
        site,
        QuerySpec::parse(&spec).unwrap(),
        ExecOptions::new(15)
            .with_now("2026-07-01T12:00:00Z".parse::<Timestamp>().unwrap())
            .with_rollup(1_000_000),
    )
    .await
    .unwrap_or_else(|e| panic!("查询失败：{e}\n{spec}"))
}

fn rows(r: &QueryResult) -> Vec<(String, Vec<f64>)> {
    r.results
        .iter()
        .map(|x| {
            (
                x.dimensions.first().cloned().unwrap_or_default(),
                x.metrics.iter().map(|m| m.as_f64()).collect(),
            )
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn plausible_export_is_imported_into_the_rollup_tier() {
    let (Some(pg_url), Some(ch_url)) = (
        var("RIVU_TEST_DATABASE_URL"),
        var("RIVU_TEST_CLICKHOUSE_URL"),
    ) else {
        println!(
            "跳过 plausible_export_is_imported_into_the_rollup_tier：需要设置 RIVU_TEST_DATABASE_URL 和 RIVU_TEST_CLICKHOUSE_URL"
        );
        return;
    };
    let migrator = account("migrator");
    let cfg = Config::from_env(
        Role::Migrate,
        &Env::from_pairs([
            ("RIVU_DATABASE_URL", pg_url.as_str()),
            ("RIVU_CLICKHOUSE_URL", ch_url.as_str()),
            ("RIVU_CH_DATABASE", ch_database().as_str()),
            ("RIVU_CH_MIGRATOR_USER", migrator.user.as_str()),
            ("RIVU_CH_MIGRATOR_PASSWORD", migrator.password.expose()),
        ]),
    )
    .expect("测试配置");
    migrate::apply(&cfg, false).await.expect("migrate");

    let pg = rivu_storage_pg::connect(&pg_url, 2).await.expect("连接 PG");
    let created = rivu_storage_pg::sites::create_site(
        &pg,
        &NewSite {
            name: "plausible-import",
            timezone: "UTC",
            hostnames: &["example.test".to_owned()],
        },
    )
    .await
    .expect("建站");
    let maintainer = rivu_storage_ch::client(&ch_url, &account("maintainer"), Some(&ch_database()));
    let reader = rivu_storage_ch::client(&ch_url, &account("reader"), Some(&ch_database()));
    let dir = std::env::temp_dir().join(format!("rivu-import-{}", created.id));
    std::fs::create_dir_all(&dir).unwrap();
    let zip_path = dir.join("export.zip");
    write_zip(&zip_path);

    // ---- 导入 -------------------------------------------------------------------------------
    let (id, summary) = rivu_jobs::import_plausible::import_file(
        &pg,
        &maintainer,
        created.id,
        "UTC",
        &zip_path,
        "export.zip",
        None,
    )
    .await
    .expect("导入");
    assert_eq!(
        (summary.start.to_string(), summary.end.to_string()),
        ("2026-06-01".to_owned(), "2026-06-03".to_owned())
    );
    assert_eq!(summary.clipped_from, None);
    let range = rivu_storage_pg::imports::completed_range(&pg, created.id)
        .await
        .unwrap()
        .unwrap();
    let mut site = SiteContext::new(created.id, TimeZone::UTC);
    site.imported = Some((range.0.parse().unwrap(), range.1.parse().unwrap()));

    // ---- 合计与各维度族 ---------------------------------------------------------------------
    let all = json!(["2026-06-01", "2026-06-03"]);
    let total = run(
        &reader,
        &site,
        json!({"metrics": ["visitors", "visits", "pageviews", "events", "bounce_rate", "visit_duration"], "date_range": all}),
    )
    .await;
    assert_eq!(total.meta.tiers, vec![Tier::Rollup]);
    // 访客 23、访问 27、浏览 42、事件 = 浏览 + 自定义事件（不含 engagement）= 42 + 7、跳出 11/27、时长 1020/27
    assert_eq!(
        rows(&total)[0].1,
        vec![23.0, 27.0, 42.0, 49.0, 11.0 / 27.0, 1020.0 / 27.0]
    );
    // 「全部时间」从导入的第一天算起
    let all_time = run(
        &reader,
        &site,
        json!({"metrics": ["visitors"], "date_range": "all"}),
    )
    .await;
    assert_eq!(rows(&all_time)[0].1, vec![23.0]);

    let by = |dim: &str| json!({"metrics": ["visitors"], "date_range": all, "dimensions": [dim]});
    assert_eq!(
        rows(&run(&reader, &site, by("visit:device")).await),
        vec![
            ("desktop".to_owned(), vec![15.0]),
            ("tablet".to_owned(), vec![5.0]),
            ("mobile".to_owned(), vec![3.0])
        ]
    );
    let browsers = rows(&run(&reader, &site, by("visit:browser")).await);
    assert_eq!(browsers[0], ("Chrome".to_owned(), vec![13.0]));
    assert_eq!(browsers[1], ("Edge".to_owned(), vec![10.0]));
    let os: Vec<String> = rows(&run(&reader, &site, by("visit:os")).await)
        .into_iter()
        .map(|r| r.0)
        .collect();
    assert_eq!(os, vec!["macOS", "Linux", "Windows"]);
    // 有访客、浏览为 0 的行（只有自定义事件的访问）也算
    assert_eq!(
        rows(&run(&reader, &site, by("visit:country")).await),
        vec![("CN".to_owned(), vec![15.0]), ("US".to_owned(), vec![8.0])]
    );
    let channels = rows(&run(&reader, &site, by("visit:channel")).await);
    assert_eq!(channels[0], ("Organic Search".to_owned(), vec![14.0]));
    assert!(
        channels.iter().any(|(c, v)| c == "Email" && v[0] == 5.0),
        "{channels:?}"
    );
    // 页面：多个 hostname 相加；退出率 = 退出 / 浏览
    let pages = rows(
        &run(
            &reader,
            &site,
            json!({"metrics": ["visitors", "pageviews", "exit_rate"], "date_range": all, "dimensions": ["event:page"]}),
        )
        .await,
    );
    assert_eq!(pages[0], ("/".to_owned(), vec![16.0, 28.0, 16.0 / 28.0]));
    // 事件：pageview 行来自访客文件，engagement 不导入
    let events = rows(
        &run(
            &reader,
            &site,
            json!({"metrics": ["events"], "date_range": all, "dimensions": ["event:name"]}),
        )
        .await,
    );
    assert_eq!(
        events,
        vec![
            ("pageview".to_owned(), vec![42.0]),
            ("Outbound Link: Click".to_owned(), vec![5.0]),
            ("Signup".to_owned(), vec![2.0])
        ]
    );
    // 属性由汇总层回答
    let props = run(
        &reader,
        &site,
        json!({"metrics": ["visitors", "events"], "date_range": all, "dimensions": ["event:props:url"]}),
    )
    .await;
    assert_eq!(props.meta.tiers, vec![Tier::Rollup]);
    assert_eq!(
        rows(&props),
        vec![
            ("https://a.test/".to_owned(), vec![3.0, 4.0]),
            ("https://b.test/".to_owned(), vec![1.0, 1.0])
        ]
    );
    // 与汇总层不兼容（两个族的维度交叉）：原始层没有这些天的数据，带提示
    let cross = run(
        &reader,
        &site,
        json!({"metrics": ["visitors"], "date_range": all, "dimensions": ["visit:country", "visit:device"]}),
    )
    .await;
    assert!(cross.results.is_empty());
    assert!(
        cross
            .meta
            .warnings
            .iter()
            .any(|w| w.code == "imported_data_excluded"),
        "{:?}",
        cross.meta.warnings
    );

    // ---- 重复导入被拒绝；删除 ------------------------------------------------------------------
    let again = rivu_jobs::import_plausible::import_file(
        &pg,
        &maintainer,
        created.id,
        "UTC",
        &zip_path,
        "export.zip",
        None,
    )
    .await;
    assert!(
        again.is_err_and(|e| e.to_string().contains("重叠")),
        "同一段日期不能导入两次"
    );
    let imports = rivu_storage_pg::imports::list(&pg, created.id)
        .await
        .unwrap();
    assert_eq!(imports.len(), 2);
    assert_eq!(imports[0].status, "failed");
    assert_eq!(imports[1].status, "completed");

    rivu_query::rollup::delete_import(
        &maintainer,
        &TenantScope::new(created.id),
        u64::try_from(id).unwrap(),
    )
    .await
    .unwrap();
    rivu_storage_pg::imports::delete(&pg, created.id, id)
        .await
        .unwrap();
    assert_eq!(
        rivu_storage_pg::imports::completed_range(&pg, created.id)
            .await
            .unwrap(),
        None
    );
    site.imported = None;
    let gone = run(
        &reader,
        &site,
        json!({"metrics": ["visitors"], "date_range": all}),
    )
    .await;
    assert_eq!(rows(&gone)[0].1, vec![0.0]);
    let _ = std::fs::remove_dir_all(&dir);
}
