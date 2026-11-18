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
use jiff::civil::{Date, date};
use jiff::tz::TimeZone;
use rivu::migrate;
use rivu_common::config::{ChAccount, Config, Env, Role, Secret};
use rivu_query::plan::Tier;
use rivu_query::result::QueryResult;
use rivu_query::rollup::{self, Family, ImportRow, Measures};
use rivu_query::{ExecOptions, QuerySpec, SiteContext, TenantScope, execute};
use rivu_storage_ch::rows::{EventRow, insert_events};
use rivu_storage_pg::sites::NewSite;
use serde_json::{Value, json};
use sqlx::PgPool;

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

fn export_zip() -> Vec<u8> {
    export_zip_from(date(2026, 6, 1))
}

fn export_zip_from(first: Date) -> Vec<u8> {
    let second = first.tomorrow().unwrap();
    let third = second.tomorrow().unwrap();
    let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let opts = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    for (family, body) in FILES {
        zip.start_file(
            format!(
                "imported_{family}_{}_{}.csv",
                first.strftime("%Y%m%d"),
                third.strftime("%Y%m%d")
            ),
            opts,
        )
        .unwrap();
        let mut lines = body.lines();
        writeln!(zip, "{}", lines.next().unwrap()).unwrap();
        for line in lines {
            let (day, columns) = line.split_once(',').unwrap();
            let day = match day {
                "\"2026-06-01\"" => first,
                "\"2026-06-02\"" => second,
                "\"2026-06-03\"" => third,
                _ => panic!("未知的夹具日期：{day}"),
            };
            writeln!(zip, "\"{day}\",{columns}").unwrap();
        }
    }
    zip.finish().unwrap().into_inner()
}

async fn run(reader: &clickhouse::Client, site: &SiteContext, spec: Value) -> QueryResult {
    run_at(reader, site, spec, "2026-07-01T12:00:00Z".parse().unwrap()).await
}

async fn run_at(
    reader: &clickhouse::Client,
    site: &SiteContext,
    spec: Value,
    now: Timestamp,
) -> QueryResult {
    execute(
        reader,
        &TenantScope::new(site.site_id),
        site,
        QuerySpec::parse(&spec).unwrap(),
        ExecOptions::new(15).with_now(now).with_rollup(1_000_000),
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

async fn databases() -> Option<(PgPool, String)> {
    let (Some(pg_url), Some(ch_url)) = (
        var("RIVU_TEST_DATABASE_URL"),
        var("RIVU_TEST_CLICKHOUSE_URL"),
    ) else {
        println!(
            "需要设置 RIVU_TEST_DATABASE_URL 和 RIVU_TEST_CLICKHOUSE_URL 才能运行导入集成测试"
        );
        return None;
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
    Some((pg, ch_url))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn plausible_export_is_imported_into_the_rollup_tier() {
    let Some((pg, ch_url)) = databases().await else {
        return;
    };
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

    // ---- 导入 -------------------------------------------------------------------------------
    let (id, summary) = rivu_jobs::import_plausible::import_file(
        &pg,
        &maintainer,
        created.id,
        export_zip(),
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
    let completed = rivu_storage_pg::imports::completed_imports(&pg, created.id)
        .await
        .unwrap();
    let range = completed.range.unwrap();
    let mut site = SiteContext::new(created.id, TimeZone::UTC);
    site.imported = Some((range.0.parse().unwrap(), range.1.parse().unwrap()));
    assert_eq!(completed.ids, [id as u64]);
    site.completed_import_ids = completed.ids;

    // An import with no PG row can still have visible ClickHouse parts.
    let scope = TenantScope::new(created.id);
    let measures = Measures {
        events: 20,
        pageviews: 20,
        ..Measures::default()
    };
    for (table, day, dims) in [
        ("rollup_events", date(2026, 6, 1), vec!["pageview"]),
        (
            "rollup_props",
            date(2026, 6, 1),
            vec!["url", "https://a.test/"],
        ),
        ("rollup_site", date(2026, 6, 4), vec![]),
    ] {
        rollup::insert_import(
            &maintainer,
            &scope,
            table,
            "UTC",
            u64::MAX,
            &[ImportRow {
                day,
                dims: dims.into_iter().map(str::to_owned).collect(),
                m: measures,
            }],
        )
        .await
        .unwrap();
    }

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

    // 属性键列表也包括导入的键（看板属性卡片与导出用）
    let keys = rivu_query::property_keys(
        &reader,
        &TenantScope::new(site.site_id),
        &site,
        QuerySpec::parse(&json!({"metrics": ["events"], "date_range": all})).unwrap(),
        ExecOptions::new(15).with_now("2026-07-01T12:00:00Z".parse::<Timestamp>().unwrap()),
    )
    .await
    .unwrap();
    assert_eq!(
        keys.iter()
            .map(|k| (k.key.as_str(), k.events))
            .collect::<Vec<_>>(),
        [("url", 5), ("plan", 2)]
    );

    let writer = rivu_storage_ch::client(&ch_url, &account("writer"), Some(&ch_database()));
    insert_pageview(
        &writer,
        &reader,
        created.id,
        "2026-06-04T12:00:00Z".parse().unwrap(),
    )
    .await;
    let mixed = run(
        &reader,
        &site,
        json!({"metrics": ["events"], "date_range": ["2026-06-01", "2026-06-04"], "dimensions": ["event:name"]}),
    )
    .await;
    assert_eq!(rows(&mixed)[0], ("pageview".to_owned(), vec![43.0]));
    assert_eq!(mixed.meta.tiers, vec![Tier::Raw, Tier::Rollup]);
    let compared = run(
        &reader,
        &site,
        json!({
            "metrics": ["events"], "date_range": ["2026-06-04", "2026-06-06"],
            "dimensions": ["event:name"], "include": {"compare": "previous_period"}
        }),
    )
    .await;
    assert_eq!(rows(&compared), vec![("pageview".to_owned(), vec![1.0])]);
    assert_eq!(
        compared.results[0].comparison.as_ref().unwrap().metrics[0].as_f64(),
        42.0
    );

    // ---- 重复导入被拒绝；删除 ------------------------------------------------------------------
    let again = rivu_jobs::import_plausible::import_file(
        &pg,
        &maintainer,
        created.id,
        export_zip(),
        "export.zip",
        None,
    )
    .await;
    assert!(
        again.is_err_and(|e| rivu_jobs::import_plausible::error_code(&e) == "overlap"),
        "同一段日期不能导入两次"
    );
    let imports = rivu_storage_pg::imports::list(&pg, created.id)
        .await
        .unwrap();
    assert_eq!(imports.len(), 2);
    assert_eq!(imports[0].status, "failed");
    assert_eq!(imports[0].error_code.as_deref(), Some("overlap"));
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
    site.completed_import_ids.clear();
    let gone = run(
        &reader,
        &site,
        json!({"metrics": ["visitors"], "date_range": all}),
    )
    .await;
    assert_eq!(rows(&gone)[0].1, vec![0.0]);

    // ---- 不是 Plausible 导出、不是 zip：带代码的失败 -------------------------------------------
    for (bytes, code) in [
        (b"not a zip".to_vec(), "invalid_zip"),
        (empty_zip(), "not_plausible"),
    ] {
        let e = rivu_jobs::import_plausible::import_file(
            &pg,
            &maintainer,
            created.id,
            bytes,
            "x.zip",
            None,
        )
        .await
        .expect_err(code);
        assert_eq!(rivu_jobs::import_plausible::error_code(&e), code);
    }

    // A cancelled handoff releases the lock even when the ordinary pool has just one slot.
    let pg_url = var("RIVU_TEST_DATABASE_URL").unwrap();
    let one = rivu_storage_pg::connect(&pg_url, 1).await.unwrap();
    let pending = rivu_jobs::import_plausible::start(&one, created.id, "cancelled.zip", None)
        .await
        .unwrap()
        .unwrap();
    let id = pending.row().id;
    let recover = || rivu_jobs::import_plausible::recover_interrupted(&one, &maintainer);
    assert_eq!(recover().await.unwrap(), 0);
    drop(pending);
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while recover().await.unwrap() == 0 {
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("dropping the executor must release its lock");
    let rows = rivu_storage_pg::imports::list(&one, created.id)
        .await
        .unwrap();
    assert_eq!(rows[0].id, id);
    assert_eq!(rows[0].error_code.as_deref(), Some("interrupted"));
}

fn empty_zip() -> Vec<u8> {
    let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    zip.start_file("readme.txt", zip::write::SimpleFileOptions::default())
        .unwrap();
    zip.write_all(b"hi").unwrap();
    zip.finish().unwrap().into_inner()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn plausible_import_keeps_today_on_the_raw_tier() {
    let Some((pg, ch_url)) = databases().await else {
        return;
    };
    let now = Timestamp::now();
    let utc = now.to_zoned(TimeZone::UTC);
    // 让站点本地日期与 UTC 不同，并留出至少一小时避免运行中跨午夜。
    let tz_name = if utc.hour() < 11 {
        "Etc/GMT+12"
    } else {
        "Etc/GMT-14"
    };
    let tz = TimeZone::get(tz_name).unwrap();
    let today = now.to_zoned(tz.clone()).date();
    assert_ne!(today, utc.date());
    let yesterday = today.yesterday().unwrap();
    let created = rivu_storage_pg::sites::create_site(
        &pg,
        &NewSite {
            name: "plausible-import-cutoff",
            timezone: tz_name,
            hostnames: &["example.test".to_owned()],
        },
    )
    .await
    .expect("建站");
    let site_id = created.id;
    let maintainer = rivu_storage_ch::client(&ch_url, &account("maintainer"), Some(&ch_database()));
    let reader = rivu_storage_ch::client(&ch_url, &account("reader"), Some(&ch_database()));
    let writer = rivu_storage_ch::client(&ch_url, &account("writer"), Some(&ch_database()));
    let scope = TenantScope::new(site_id);
    let before = rivu_query::rollup::build_state(&reader, &scope, &tz)
        .await
        .unwrap();
    assert_eq!((before.native_from, before.built_to), (None, None));

    let import = |first| {
        rivu_jobs::import_plausible::import_file(
            &pg,
            &maintainer,
            site_id,
            export_zip_from(first),
            "cutoff.zip",
            None,
        )
    };
    let (id, summary) = import(yesterday).await.expect("导入昨、今、明三天");
    insert_pageview(&writer, &reader, site_id, now).await;

    let completed = rivu_storage_pg::imports::completed_imports(&pg, site_id)
        .await
        .unwrap();
    let range = completed.range.unwrap();
    let mut site = SiteContext::new(site_id, tz.clone());
    site.imported = Some((range.0.parse().unwrap(), range.1.parse().unwrap()));
    site.completed_import_ids = completed.ids;
    let current = run_at(
        &reader,
        &site,
        json!({"metrics": ["pageviews"], "date_range": "day"}),
        now,
    )
    .await;
    assert_eq!(
        (rows(&current)[0].1[0], current.meta.tiers),
        (1.0, vec![Tier::Raw]),
        "站点本地今天只计入原生 pageview"
    );
    let previous = run_at(
        &reader,
        &site,
        json!({"metrics": ["pageviews"], "date_range": [yesterday.to_string(), yesterday.to_string()]}),
        now,
    )
    .await;
    assert_eq!(
        (rows(&previous)[0].1[0], previous.meta.tiers),
        (20.0, vec![Tier::Rollup])
    );
    assert_eq!(summary.clipped_from, Some(today));
    assert_eq!((summary.start, summary.end), (yesterday, yesterday));
    assert_eq!(site.imported, Some((yesterday, yesterday)));
    for table in Family::TABLES {
        let days = reader
            .query(&format!(
                "SELECT DISTINCT toString(day) FROM {table} WHERE site_id = ? AND origin = 'import' AND import_id = ?"
            ))
            .bind(site_id)
            .bind(id)
            .fetch_all::<String>()
            .await
            .unwrap();
        assert_eq!(days, [yesterday.to_string()], "{table}");
    }

    let error = import(today).await.expect_err("今天起没有可导入的历史日");
    assert_eq!(rivu_jobs::import_plausible::error_code(&error), "no_days");
    let failed = &rivu_storage_pg::imports::list(&pg, site_id).await.unwrap()[0];
    assert_eq!(failed.status, "failed");
    assert_eq!(failed.error_code.as_deref(), Some("no_days"));
    assert_eq!(
        (&failed.start_date, &failed.end_date, failed.rows_imported),
        (&None, &None, 0)
    );

    // 更早的原生数据仍优先。压缩包原本与已有导入重叠，截断后恰好不重叠。
    let prior_local_time = yesterday.at(12, 0, 0, 0).to_zoned(tz).unwrap().timestamp();
    insert_pageview(&writer, &reader, site_id, prior_local_time).await;
    let earlier = yesterday.yesterday().unwrap();
    let (id, summary) = import(earlier).await.expect("按已有原生日截断");
    assert_eq!(summary.clipped_from, Some(yesterday));
    assert_eq!((summary.start, summary.end), (earlier, earlier));
    let saved = rivu_storage_pg::imports::list(&pg, site_id)
        .await
        .unwrap()
        .into_iter()
        .find(|row| row.id == id)
        .expect("导入记录已保存");
    assert_eq!(
        (saved.start_date, saved.end_date),
        (Some(earlier.to_string()), Some(earlier.to_string()))
    );
}

async fn insert_pageview(
    writer: &clickhouse::Client,
    reader: &clickhouse::Client,
    site_id: u64,
    ts: Timestamp,
) {
    let ts = time::OffsetDateTime::from_unix_timestamp_nanos(ts.as_nanosecond()).unwrap();
    let event_id = uuid::Uuid::now_v7();
    let row = EventRow {
        site_id,
        event_id,
        ts,
        session_start: ts,
        visitor_id: 1,
        session_id: u64::try_from(ts.unix_timestamp()).unwrap(),
        name: "pageview".into(),
        hostname: "example.test".into(),
        pathname: "/".into(),
        entry_pathname: "/".into(),
        ..EventRow::default()
    };
    insert_events(writer, &[row], &event_id.to_string(), false)
        .await
        .unwrap();
    let count = reader
        .query("SELECT count() FROM events WHERE site_id = ? AND name = 'pageview' AND event_id = toUUID(?)")
        .bind(site_id)
        .bind(event_id.to_string())
        .fetch_one::<u64>()
        .await
        .unwrap();
    assert_eq!(count, 1, "原生 pageview 已落库");
}
