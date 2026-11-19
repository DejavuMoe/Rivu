//! 周报的两封邮件：确认邮箱的链接邮件和周报本身。
//!
//! 每封都有纯文本和 HTML 两个部分，内容相同、链接相同。排版和文案来自
//! `designs/weekly-report/email.html`（2026-11-19 确认）：表格布局、行内样式，没有脚本、图片和
//! 远程资源，深色由 `prefers-color-scheme: dark` 切换。插进 HTML 的每个值都经过转义；
//! 链接只用服务端拼出来的地址（`RIVU_BASE_URL` 加固定路径和令牌），不用任何用户输入。

use jiff::civil::Date;

use crate::{Lang, Mail};

/// 一封周报用到的数据：`deliver` 已有的那些，不多查。
#[derive(Clone, Copy, Debug)]
pub struct Report<'a> {
    pub lang: Lang,
    /// 收件地址，也写在页脚里。
    pub to: &'a str,
    /// `RIVU_BASE_URL`，末尾不带 `/`。
    pub base_url: &'a str,
    pub site_id: i64,
    pub site_name: &'a str,
    pub timezone: &'a str,
    /// 这一周的第一天和最后一天（站点时区的日期，含两端）。
    pub from: Date,
    pub until: Date,
    pub visitors: u64,
    pub visits: u64,
    pub pageviews: u64,
    /// 这次发送的退订地址，同时放进 `List-Unsubscribe`。
    pub unsubscribe_url: &'a str,
}

/// 周报邮件。
pub fn report(r: &Report<'_>) -> Mail {
    let s = Words::of(r.lang);
    let (long, short) = period(r.from, r.until, r.lang);
    let subject = match r.lang {
        Lang::Zh => format!("{} 周报：{short}", r.site_name),
        Lang::En => format!("{} weekly report: {short}", r.site_name),
    };
    let numbers = [r.visitors, r.visits, r.pageviews].map(grouped);
    let [visitors, visits, pageviews] = &numbers;
    let quiet = r.visitors == 0 && r.visits == 0 && r.pageviews == 0;
    let dashboard = format!("{}/s/{}", r.base_url, r.site_id);
    let manage = format!("{}/account", r.base_url);
    let reason = match r.lang {
        Lang::Zh => format!("你订阅了 {} 的周报，收件地址 {}。", r.site_name, r.to),
        Lang::En => format!(
            "You subscribed to the weekly report for {} at {}.",
            r.site_name, r.to
        ),
    };
    let (open_zone, close_zone) = match r.lang {
        Lang::Zh => ("（", "）"),
        Lang::En => (" (", ")"),
    };
    let colon = s.colon;

    let mut text = format!(
        "{}\n{long}{open_zone}{}{close_zone}\n\n",
        r.site_name, r.timezone
    );
    for (label, value) in [s.visitors, s.visits, s.pageviews].iter().zip(&numbers) {
        text.push_str(&format!("{label}{colon}{value}\n"));
    }
    if quiet {
        text.push_str(s.no_visits);
        text.push('\n');
    }
    text.push_str(&format!(
        "\n{}{colon}{dashboard}\n\n{reason}\n{}{colon}{manage}\n{}{colon}{}\n",
        s.open, s.manage, s.unsubscribe, r.unsubscribe_url
    ));

    let metric = |label: &str, value: &str, first: bool| {
        format!(
            "<td class=\"metric line\" width=\"33%\" valign=\"top\" style=\"width:33%;padding:14px 16px;{}\">\n{}\n\
             <p class=\"tx\" style=\"margin:4px 0 0;font-family:{SANS};font-size:24px;line-height:32px;font-weight:600;color:{TEXT};font-variant-numeric:tabular-nums;mso-line-height-rule:exactly;\">{}</p>\n</td>",
            if first {
                String::new()
            } else {
                format!("border-left:1px solid {LINE};")
            },
            para(Tone::Muted, &escape(label), ""),
            escape(value),
        )
    };
    let range = format!(
        "<span class=\"mu\" style=\"color:{MUTED};\">{}{open_zone}{}{close_zone}</span>",
        escape(&long),
        escape(r.timezone)
    );
    let footer_link = |href: &str, label: &str| {
        para(
            Tone::Muted,
            &format!(
                "<a class=\"lk\" href=\"{}\" style=\"color:{LINK};text-decoration:underline;\">{}</a>",
                escape(href),
                escape(label)
            ),
            "margin-top:4px;",
        )
    };
    let body = format!(
        "\n<tr><td class=\"px\" style=\"padding:20px 32px 0;\">\n{}\n{}\n</td></tr>\n\
         <tr><td class=\"px\" style=\"padding:20px 32px 0;\">\n\
         <table role=\"presentation\" class=\"line\" width=\"100%\" cellpadding=\"0\" cellspacing=\"0\" border=\"0\" style=\"border:1px solid {LINE};border-radius:8px;border-collapse:separate;\">\n\
         <tr>{}{}{}</tr>\n</table>\n{}\n</td></tr>\n\
         <tr><td class=\"px\" style=\"padding:24px 32px 0;\">\n{}\n</td></tr>\n\
         {}",
        heading(&escape(r.site_name)),
        para(Tone::Body, &range, "margin-top:2px;"),
        metric(s.visitors, visitors, true),
        metric(s.visits, visits, false),
        metric(s.pageviews, pageviews, false),
        if quiet {
            para(Tone::Muted, &escape(s.no_visits), "margin-top:8px;")
        } else {
            String::new()
        },
        button(&dashboard, s.open),
        footer(&[
            para(Tone::Muted, &escape(&reason), ""),
            footer_link(&manage, s.manage),
            footer_link(r.unsubscribe_url, s.unsubscribe),
        ]),
    );
    let preheader = match r.lang {
        Lang::Zh => format!("访客 {visitors}，访问 {visits}，浏览量 {pageviews}"),
        Lang::En => format!("{visitors} visitors, {visits} visits, {pageviews} pageviews"),
    };
    let html = shell(r.lang, &subject, &preheader, &body);
    Mail {
        to: r.to.to_owned(),
        subject,
        body: text,
        html: Some(html),
        unsubscribe_url: Some(r.unsubscribe_url.to_owned()),
    }
}

/// 确认邮箱的邮件。`link` 是 `{base}/weekly-report/confirm/{token}`。没有退订链接，也不带
/// `List-Unsubscribe`：确认属于账号，不属于某个站点。
pub fn confirmation(lang: Lang, to: &str, link: &str) -> Mail {
    let s = Words::of(lang);
    let (subject, title, lead, action, terms, ignore) = match lang {
        Lang::Zh => (
            "确认接收 Rivu 周报",
            "确认接收周报",
            "确认用这个邮箱接收 Rivu 周报。",
            "确认邮箱",
            "链接 1 小时内有效，只能用一次。",
            "如果不是你本人操作，忽略这封邮件即可。",
        ),
        Lang::En => (
            "Confirm your email for Rivu weekly reports",
            "Confirm your email",
            "Confirm that Rivu weekly reports should be sent to this address.",
            "Confirm email",
            "The link works once and expires in 1 hour.",
            "If you didn\u{2019}t ask for this, you can ignore this email.",
        ),
    };
    let text = format!(
        "{lead}\n\n{action}{}{link}\n\n{terms}\n\n{ignore}\n",
        s.colon
    );
    let body = format!(
        "\n<tr><td class=\"px\" style=\"padding:20px 32px 0;\">\n{}\n{}\n</td></tr>\n\
         <tr><td class=\"px\" style=\"padding:24px 32px 0;\">\n{}\n{}\n</td></tr>\n\
         {}",
        heading(&escape(title)),
        para(Tone::Body, &escape(lead), "margin-top:8px;"),
        button(link, action),
        para(Tone::Muted, &escape(terms), "margin-top:12px;"),
        footer(&[para(Tone::Muted, &escape(ignore), "")]),
    );
    Mail {
        to: to.to_owned(),
        subject: subject.to_owned(),
        body: text,
        html: Some(shell(lang, subject, lead, &body)),
        unsubscribe_url: None,
    }
}

/// 两种语言的固定文案。
struct Words {
    visitors: &'static str,
    visits: &'static str,
    pageviews: &'static str,
    no_visits: &'static str,
    open: &'static str,
    manage: &'static str,
    unsubscribe: &'static str,
    colon: &'static str,
}

impl Words {
    fn of(lang: Lang) -> Self {
        match lang {
            Lang::Zh => Words {
                visitors: "访客",
                visits: "访问",
                pageviews: "浏览量",
                no_visits: "这一周没有记录到访问。",
                open: "打开看板",
                manage: "管理周报",
                unsubscribe: "退订周报",
                colon: "：",
            },
            Lang::En => Words {
                visitors: "Visitors",
                visits: "Visits",
                pageviews: "Pageviews",
                no_visits: "No visits were recorded this week.",
                open: "Open dashboard",
                manage: "Manage weekly report",
                unsubscribe: "Unsubscribe",
                colon: ": ",
            },
        }
    }
}

// 浅色是默认，深色只在 `prefers-color-scheme: dark` 里覆盖。
const PAGE: &str = "#f9f9f9";
const CARD: &str = "#fcfcfc";
const LINE: &str = "#d9d9d9";
const TEXT: &str = "#202020";
const MUTED: &str = "#646464";
const LINK: &str = "#0d74ce";
const SANS: &str = "-apple-system, BlinkMacSystemFont, 'Segoe UI', 'PingFang SC', 'Hiragino Sans GB', 'Microsoft YaHei', Arial, Helvetica, sans-serif";
const DARK_RULES: &str = "
        .bg { background: #111111 !important; }
        .card { background: #191919 !important; border-color: #3a3a3a !important; }
        .line { border-color: #3a3a3a !important; }
        .tx { color: #eeeeee !important; }
        .mu { color: #b4b4b4 !important; }
        .lk { color: #70b8ff !important; }
        .metric + .metric { border-top-color: #3a3a3a !important; }";

/// 外框：标题、摘要（收件箱列表里显示的那一行，正文里隐藏）、卡片和左上角的 Rivu。
fn shell(lang: Lang, title: &str, preheader: &str, body: &str) -> String {
    let lang = match lang {
        Lang::Zh => "zh-CN",
        Lang::En => "en",
    };
    format!(
        "<!doctype html>\n<html lang=\"{lang}\">\n<head>\n<meta charset=\"utf-8\">\n\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n\
         <meta name=\"color-scheme\" content=\"light dark\">\n\
         <meta name=\"supported-color-schemes\" content=\"light dark\">\n\
         <title>{}</title>\n<style>\n  body {{ margin: 0; padding: 0; }}\n  a {{ text-decoration: none; }}\n  \
         @media (max-width: 480px) {{\n    \
         .px {{ padding-left: 20px !important; padding-right: 20px !important; }}\n    \
         .metric {{ display: block !important; width: auto !important; border-left: 0 !important; }}\n    \
         .metric + .metric {{ border-top: 1px solid {LINE} !important; }}\n  }}\n  \
         @media (prefers-color-scheme: dark) {{{DARK_RULES}\n  }}\n</style>\n</head>\n\
         <body class=\"bg\" style=\"margin:0;padding:0;background:{PAGE};\">\n\
         <div style=\"display:none;max-height:0;overflow:hidden;opacity:0;color:transparent;\">{}</div>\n\
         <table role=\"presentation\" class=\"bg\" width=\"100%\" cellpadding=\"0\" cellspacing=\"0\" border=\"0\" style=\"background:{PAGE};\">\n\
         <tr><td align=\"center\" style=\"padding:24px 12px;\">\n\
         <table role=\"presentation\" class=\"card\" width=\"600\" cellpadding=\"0\" cellspacing=\"0\" border=\"0\" style=\"width:100%;max-width:600px;background:{CARD};border:1px solid {LINE};border-radius:8px;border-collapse:separate;\">\n\
         <tr><td class=\"px tx\" style=\"padding:28px 32px 0;font-family:{SANS};font-size:14px;line-height:20px;font-weight:600;color:{TEXT};mso-line-height-rule:exactly;\">Rivu</td></tr>\n\
         {body}\n</table>\n</td></tr>\n</table>\n</body>\n</html>",
        escape(title),
        escape(preheader),
    )
}

#[derive(Clone, Copy)]
enum Tone {
    Body,
    Muted,
}

/// 一段文字。`content` 已经是 HTML（调用方负责转义）。
fn para(tone: Tone, content: &str, extra: &str) -> String {
    let (class, size, color) = match tone {
        Tone::Body => ("tx", "14px;line-height:22px", TEXT),
        Tone::Muted => ("mu", "12px;line-height:18px", MUTED),
    };
    format!(
        "<p class=\"{class}\" style=\"margin:0;font-family:{SANS};font-size:{size};color:{color};mso-line-height-rule:exactly;{extra}\">{content}</p>"
    )
}

/// `content` 已经转义。
fn heading(content: &str) -> String {
    format!(
        "<h1 class=\"tx\" style=\"margin:0;font-family:{SANS};font-size:16px;line-height:24px;font-weight:600;color:{TEXT};mso-line-height-rule:exactly;\">{content}</h1>"
    )
}

/// 蓝色主按钮。
fn button(href: &str, label: &str) -> String {
    format!(
        "<table role=\"presentation\" cellpadding=\"0\" cellspacing=\"0\" border=\"0\"><tr>\n\
         <td bgcolor=\"#0090ff\" style=\"border-radius:6px;background:#0090ff;\">\n\
         <a href=\"{}\" style=\"display:block;padding:8px 14px;font-family:{SANS};font-size:14px;line-height:20px;font-weight:500;color:#ffffff;text-decoration:none;border-radius:6px;mso-line-height-rule:exactly;\">{}</a>\n\
         </td></tr></table>",
        escape(href),
        escape(label)
    )
}

/// 分隔线下面的页脚，每项一行。
fn footer(lines: &[String]) -> String {
    format!(
        "<tr><td class=\"px\" style=\"padding:28px 32px 28px;\">\n\
         <table role=\"presentation\" width=\"100%\" cellpadding=\"0\" cellspacing=\"0\" border=\"0\"><tr><td class=\"line\" style=\"border-top:1px solid {LINE};padding-top:16px;\">\n\
         {}\n</td></tr></table>\n</td></tr>",
        lines.join("\n")
    )
}

/// 正文和属性值里都能用的转义。
fn escape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for c in value.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}

/// 千位分隔：`12840310` → `12,840,310`（中英文相同）。
fn grouped(value: u64) -> String {
    let digits = value.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];
/// 英文日期范围的分隔：空格、短横线（en dash）、空格，与 Chromium 156 的 `Intl.DateTimeFormat` 一致
/// （较早的 ICU 在短横线两边用细空格）。
const EN_DASH: &str = " \u{2013} ";

/// 这一周写成文字：（正文里的完整写法，主题里的短写法）。短写法省掉年份，跨年时与完整写法相同。
fn period(from: Date, until: Date, lang: Lang) -> (String, String) {
    let (y1, m1, d1) = (from.year(), from.month(), from.day());
    let (y2, m2, d2) = (until.year(), until.month(), until.day());
    match lang {
        Lang::Zh => {
            let tail = if y1 != y2 {
                format!("{y2} 年 {m2} 月 {d2} 日")
            } else if m1 != m2 {
                format!("{m2} 月 {d2} 日")
            } else {
                format!("{d2} 日")
            };
            let long = format!("{y1} 年 {m1} 月 {d1} 日至 {tail}");
            if y1 != y2 {
                return (long.clone(), long);
            }
            (long, format!("{m1} 月 {d1} 日至 {tail}"))
        }
        Lang::En => {
            let month = |m: i8| MONTHS[usize::try_from(m - 1).unwrap_or(0)];
            if y1 != y2 {
                let long = format!("{} {d1}, {y1}{EN_DASH}{} {d2}, {y2}", month(m1), month(m2));
                return (long.clone(), long);
            }
            let short = if m1 != m2 {
                format!("{} {d1}{EN_DASH}{} {d2}", month(m1), month(m2))
            } else {
                format!("{} {d1}{EN_DASH}{d2}", month(m1))
            };
            (format!("{short}, {y1}"), short)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASE: &str = "https://rivu.example.com";
    const CONFIRM: &str = "https://rivu.example.com/weekly-report/confirm/q7Rk2vX9mB4nT0pL8sW1yZ3cF6hJ5dG2aE7uK9wQ0rM";
    const UNSUBSCRIBE: &str = "https://rivu.example.com/api/weekly-report/unsubscribe/U-T6YSW5Iz5wLwwUwOIRmlrLLTd32AZim_9uftvAePg";

    /// 原型的三组数据：普通周、无访问、跨年加大数值。
    fn fixture(lang: Lang, name: &str) -> Report<'static> {
        let (site_id, site_name, timezone, from, until, counts) = match name {
            "studio" => (
                1,
                "studio.example.com",
                "Asia/Taipei",
                "2026-11-16",
                "2026-11-22",
                [1284, 1702, 4315],
            ),
            "quiet" => (
                2,
                "docs.example.com",
                "Asia/Taipei",
                "2026-11-15",
                "2026-11-21",
                [0, 0, 0],
            ),
            _ => (
                3,
                "shop.example.com",
                "America/New_York",
                "2026-12-28",
                "2027-01-03",
                [12_840_310, 18_755_902, 96_302_417],
            ),
        };
        Report {
            lang,
            to: "viewer@example.com",
            base_url: BASE,
            site_id,
            site_name,
            timezone,
            from: from.parse().unwrap(),
            until: until.parse().unwrap(),
            visitors: counts[0],
            visits: counts[1],
            pageviews: counts[2],
            unsubscribe_url: UNSUBSCRIBE,
        }
    }

    fn hrefs(html: &str) -> Vec<String> {
        html.split("href=\"")
            .skip(1)
            .map(|rest| rest[..rest.find('"').unwrap()].replace("&amp;", "&"))
            .collect()
    }

    fn text_links(text: &str) -> Vec<String> {
        text.lines()
            .filter_map(|line| line.find("https://").map(|i| line[i..].to_owned()))
            .collect()
    }

    /// 自包含：没有脚本、图片、外部样式和远程资源，远小于 100 KB，带深色规则。
    fn self_contained(html: &str) {
        assert!(html.len() < 100_000);
        let lower = html.to_ascii_lowercase();
        for banned in [
            "<script",
            "<img",
            "<link",
            "<iframe",
            "<object",
            "<embed",
            "<form",
            "@import",
            "url(",
            "javascript:",
        ] {
            assert!(!lower.contains(banned), "{banned}");
        }
        assert!(html.contains("@media (prefers-color-scheme: dark) {"));
        assert!(html.contains("<meta name=\"color-scheme\" content=\"light dark\">"));
        for href in hrefs(html) {
            assert!(href.starts_with("https://rivu.example.com/"), "{href}");
        }
    }

    #[test]
    fn typical_week_in_both_languages() {
        let zh = report(&fixture(Lang::Zh, "studio"));
        assert_eq!(zh.subject, "studio.example.com 周报：11 月 16 日至 22 日");
        assert_eq!(
            zh.body,
            format!(
                "studio.example.com\n2026 年 11 月 16 日至 22 日（Asia/Taipei）\n\n\
                 访客：1,284\n访问：1,702\n浏览量：4,315\n\n\
                 打开看板：{BASE}/s/1\n\n\
                 你订阅了 studio.example.com 的周报，收件地址 viewer@example.com。\n\
                 管理周报：{BASE}/account\n退订周报：{UNSUBSCRIBE}\n"
            )
        );
        assert_eq!(zh.unsubscribe_url.as_deref(), Some(UNSUBSCRIBE));
        let html = zh.html.unwrap();
        self_contained(&html);
        assert!(html.starts_with("<!doctype html>\n<html lang=\"zh-CN\">"));
        assert!(html.contains("<title>studio.example.com 周报：11 月 16 日至 22 日</title>"));
        assert!(html.contains(">访客 1,284，访问 1,702，浏览量 4,315</div>"));
        assert!(html.contains(">2026 年 11 月 16 日至 22 日（Asia/Taipei）</span>"));
        assert!(html.contains(">打开看板</a>") && html.contains(">退订周报</a>"));
        assert!(!html.contains("这一周没有记录到访问"));

        let en = report(&fixture(Lang::En, "studio"));
        assert_eq!(
            en.subject,
            "studio.example.com weekly report: Nov 16 \u{2013} 22"
        );
        assert_eq!(
            en.body,
            format!(
                "studio.example.com\nNov 16 \u{2013} 22, 2026 (Asia/Taipei)\n\n\
                 Visitors: 1,284\nVisits: 1,702\nPageviews: 4,315\n\n\
                 Open dashboard: {BASE}/s/1\n\n\
                 You subscribed to the weekly report for studio.example.com at viewer@example.com.\n\
                 Manage weekly report: {BASE}/account\nUnsubscribe: {UNSUBSCRIBE}\n"
            )
        );
        let html = en.html.unwrap();
        self_contained(&html);
        assert!(html.starts_with("<!doctype html>\n<html lang=\"en\">"));
        assert!(html.contains(">1,284 visitors, 1,702 visits, 4,315 pageviews</div>"));
        assert!(html.contains(">Manage weekly report</a>") && html.contains(">Unsubscribe</a>"));
    }

    #[test]
    fn a_week_without_visits_says_so() {
        for (lang, line) in [
            (Lang::Zh, "这一周没有记录到访问。"),
            (Lang::En, "No visits were recorded this week."),
        ] {
            let mail = report(&fixture(lang, "quiet"));
            let zero = if lang == Lang::Zh {
                "访问：0\n"
            } else {
                "Visits: 0\n"
            };
            assert!(mail.body.contains(zero));
            assert!(
                mail.body.contains(&format!("0\n{line}\n\n")),
                "{}",
                mail.body
            );
            assert!(mail.html.unwrap().contains(&format!("{line}</p>")));
        }
        assert_eq!(
            report(&fixture(Lang::Zh, "quiet")).subject,
            "docs.example.com 周报：11 月 15 日至 21 日"
        );
        // 只要有一项不为零就不写这一行。
        let mut some = fixture(Lang::En, "quiet");
        some.pageviews = 1;
        let mail = report(&some);
        assert!(!mail.body.contains("No visits"));
        assert!(!mail.html.unwrap().contains("No visits"));
    }

    #[test]
    fn a_year_boundary_with_large_numbers() {
        let zh = report(&fixture(Lang::Zh, "large"));
        assert_eq!(
            zh.subject,
            "shop.example.com 周报：2026 年 12 月 28 日至 2027 年 1 月 3 日"
        );
        assert!(zh.body.starts_with(
            "shop.example.com\n2026 年 12 月 28 日至 2027 年 1 月 3 日（America/New_York）\n\n\
             访客：12,840,310\n访问：18,755,902\n浏览量：96,302,417\n\n"
        ));
        let en = report(&fixture(Lang::En, "large"));
        let range = "Dec 28, 2026 \u{2013} Jan 3, 2027";
        assert_eq!(
            en.subject,
            format!("shop.example.com weekly report: {range}")
        );
        assert!(en.body.starts_with(&format!(
            "shop.example.com\n{range} (America/New_York)\n\n\
             Visitors: 12,840,310\nVisits: 18,755,902\nPageviews: 96,302,417\n\n"
        )));
        let html = en.html.unwrap();
        self_contained(&html);
        for value in ["12,840,310", "18,755,902", "96,302,417"] {
            assert!(html.contains(&format!(">{value}</p>")));
        }
    }

    #[test]
    fn dates_and_numbers_follow_the_language() {
        let d = |s: &str| s.parse::<Date>().unwrap();
        let both = |a, b, lang| period(d(a), d(b), lang);
        assert_eq!(
            both("2026-11-30", "2026-12-06", Lang::Zh),
            (
                "2026 年 11 月 30 日至 12 月 6 日".into(),
                "11 月 30 日至 12 月 6 日".into()
            )
        );
        assert_eq!(
            both("2026-11-30", "2026-12-06", Lang::En),
            (
                "Nov 30 \u{2013} Dec 6, 2026".into(),
                "Nov 30 \u{2013} Dec 6".into()
            )
        );
        assert_eq!(
            both("2026-09-01", "2026-09-07", Lang::En).1,
            "Sep 1 \u{2013} 7"
        );
        for (value, want) in [
            (0, "0"),
            (999, "999"),
            (1000, "1,000"),
            (1_284, "1,284"),
            (96_302_417, "96,302,417"),
            (u64::MAX, "18,446,744,073,709,551,615"),
        ] {
            assert_eq!(grouped(value), want);
        }
    }

    #[test]
    fn a_hostile_site_name_renders_as_text() {
        let name = "<img src=x onerror=alert(1)> \"Tom's\" & <script>x</script>";
        let mut r = fixture(Lang::En, "studio");
        r.site_name = name;
        r.timezone = "Etc/<b>";
        let mail = report(&r);
        // 纯文本和主题照原样。
        assert!(mail.subject.starts_with(name));
        assert!(mail.body.starts_with(&format!("{name}\n")));
        let html = mail.html.unwrap();
        self_contained(&html);
        let escaped = "&lt;img src=x onerror=alert(1)&gt; &quot;Tom&#39;s&quot; &amp; &lt;script&gt;x&lt;/script&gt;";
        assert!(html.contains(&format!("<title>{escaped} weekly report:")));
        assert!(html.contains(&format!(">{escaped}</h1>")));
        assert!(html.contains(&format!(
            "weekly report for {escaped} at viewer@example.com."
        )));
        assert!(html.contains("(Etc/&lt;b&gt;)"));
        assert!(!html.contains("<b>"));
        assert_eq!(hrefs(&html).len(), 3);

        // 地址也一样：放进 href 的值同样转义，不能跳出属性。
        let mut r = fixture(Lang::Zh, "studio");
        r.to = "a\"b@example.com";
        r.base_url = "https://rivu.example.com/x?a=1&b=\"2";
        let html = report(&r).html.unwrap();
        assert!(html.contains("收件地址 a&quot;b@example.com。"));
        assert!(html.contains("href=\"https://rivu.example.com/x?a=1&amp;b=&quot;2/account\""));
    }

    #[test]
    fn text_and_html_carry_the_same_links() {
        for lang in [Lang::Zh, Lang::En] {
            for name in ["studio", "quiet", "large"] {
                let r = fixture(lang, name);
                let mail = report(&r);
                let want = vec![
                    format!("{BASE}/s/{}", r.site_id),
                    format!("{BASE}/account"),
                    UNSUBSCRIBE.to_owned(),
                ];
                assert_eq!(text_links(&mail.body), want);
                assert_eq!(hrefs(mail.html.as_deref().unwrap()), want);
            }
            let mail = confirmation(lang, "viewer@example.com", CONFIRM);
            assert_eq!(text_links(&mail.body), [CONFIRM]);
            assert_eq!(hrefs(mail.html.as_deref().unwrap()), [CONFIRM]);
        }
    }

    #[test]
    fn confirmation_in_both_languages() {
        let zh = confirmation(Lang::Zh, "viewer@example.com", CONFIRM);
        assert_eq!(zh.subject, "确认接收 Rivu 周报");
        assert_eq!(
            zh.body,
            format!(
                "确认用这个邮箱接收 Rivu 周报。\n\n确认邮箱：{CONFIRM}\n\n\
                 链接 1 小时内有效，只能用一次。\n\n如果不是你本人操作，忽略这封邮件即可。\n"
            )
        );
        assert_eq!(zh.to, "viewer@example.com");
        assert_eq!(zh.unsubscribe_url, None);
        let html = zh.html.unwrap();
        self_contained(&html);
        assert!(html.contains("<title>确认接收 Rivu 周报</title>"));
        assert!(html.contains(">确认接收周报</h1>") && html.contains(">确认邮箱</a>"));
        assert!(!html.contains("退订"));

        let en = confirmation(Lang::En, "viewer@example.com", CONFIRM);
        assert_eq!(en.subject, "Confirm your email for Rivu weekly reports");
        assert_eq!(
            en.body,
            format!(
                "Confirm that Rivu weekly reports should be sent to this address.\n\n\
                 Confirm email: {CONFIRM}\n\nThe link works once and expires in 1 hour.\n\n\
                 If you didn\u{2019}t ask for this, you can ignore this email.\n"
            )
        );
        let html = en.html.unwrap();
        self_contained(&html);
        assert!(html.contains(">Confirm your email</h1>") && html.contains(">Confirm email</a>"));
        assert!(!html.to_ascii_lowercase().contains("unsubscribe"));
    }
}
