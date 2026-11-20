//! 更换登录邮箱的两封邮件：发到新邮箱的确认链接，和改好后发到旧邮箱的通知。
//!
//! 与周报的确认邮件同一套排版（`designs/weekly-report/email.html`）：纯文本加 HTML，表格布局、行内样式，
//! 没有脚本、图片和远程资源。插进 HTML 的值都经过转义；链接只用服务端拼出来的地址。

use crate::weekly_report::{Tone, button, escape, footer, heading, para, shell};
use crate::{Lang, Mail};

/// 确认新邮箱的邮件，发到新邮箱。`link` 是 `{base}/email-change/confirm/{token}`，`hours` 是有效期。
pub fn confirmation(lang: Lang, to: &str, link: &str, hours: u32) -> Mail {
    let colon = match lang {
        Lang::Zh => "：",
        Lang::En => ": ",
    };
    let (subject, title, lead, action, terms, ignore) = match lang {
        Lang::Zh => (
            "确认 Rivu 的新登录邮箱".to_owned(),
            "确认新的登录邮箱",
            "确认以后用这个邮箱登录 Rivu。".to_owned(),
            "确认更换",
            format!("链接 {hours} 小时内有效，只能用一次。确认后，其他设备需要重新登录。"),
            "如果不是你本人操作，忽略这封邮件即可，登录邮箱不会改变。".to_owned(),
        ),
        Lang::En => (
            "Confirm your new Rivu sign-in email".to_owned(),
            "Confirm your new sign-in email",
            "Confirm that you want to sign in to Rivu with this address from now on.".to_owned(),
            "Confirm change",
            format!(
                "The link works once and expires in {hours} hours. After confirming, \
                 you\u{2019}ll need to sign in again on your other devices."
            ),
            "If you didn\u{2019}t ask for this, you can ignore this email and your sign-in \
             email stays the same."
                .to_owned(),
        ),
    };
    let text = format!("{lead}\n\n{action}{colon}{link}\n\n{terms}\n\n{ignore}\n");
    let body = format!(
        "\n<tr><td class=\"px\" style=\"padding:20px 32px 0;\">\n{}\n{}\n</td></tr>\n\
         <tr><td class=\"px\" style=\"padding:24px 32px 0;\">\n{}\n{}\n</td></tr>\n\
         {}",
        heading(&escape(title)),
        para(Tone::Body, &escape(&lead), "margin-top:8px;"),
        button(link, action),
        para(Tone::Muted, &escape(&terms), "margin-top:12px;"),
        footer(&[para(Tone::Muted, &escape(&ignore), "")]),
    );
    Mail {
        to: to.to_owned(),
        html: Some(shell(lang, &subject, &lead, &body)),
        subject,
        body: text,
        unsubscribe_url: None,
    }
}

/// 改好之后发到旧邮箱的通知。`masked_new` 是用 [`mask_email`] 遮住的新邮箱。没有链接：
/// 旧邮箱已经不属于这个账号，不能凭它做任何事。
pub fn changed_notice(lang: Lang, to: &str, masked_new: &str) -> Mail {
    let (subject, lead, help) = match lang {
        Lang::Zh => (
            "Rivu 登录邮箱已更改",
            format!(
                "你的 Rivu 账号的登录邮箱已改为 {masked_new}。以后请用新邮箱登录，其他设备上的登录已退出。"
            ),
            "如果不是你本人操作，请尽快联系这个 Rivu 的管理员。",
        ),
        Lang::En => (
            "Your Rivu sign-in email was changed",
            format!(
                "The email you use to sign in to Rivu was changed to {masked_new}. Use the new \
                 address from now on. Your other devices were signed out."
            ),
            "If you didn\u{2019}t make this change, contact the administrator of this Rivu \
             right away.",
        ),
    };
    let text = format!("{lead}\n\n{help}\n");
    let body = format!(
        "\n<tr><td class=\"px\" style=\"padding:20px 32px 0;\">\n{}\n{}\n</td></tr>\n{}",
        heading(&escape(subject)),
        para(Tone::Body, &escape(&lead), "margin-top:8px;"),
        footer(&[para(Tone::Muted, &escape(help), "")]),
    );
    Mail {
        to: to.to_owned(),
        subject: subject.to_owned(),
        body: text,
        html: Some(shell(lang, subject, &lead, &body)),
        unsubscribe_url: None,
    }
}

/// 遮住邮箱：本地部分只留第一个字符，域名完整，`name@example.com` → `n***@example.com`。
pub fn mask_email(email: &str) -> String {
    match email.rsplit_once('@') {
        Some((local, domain)) => {
            let first: String = local.chars().take(1).collect();
            format!("{first}***@{domain}")
        }
        None => "***".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LINK: &str =
        "https://rivu.example.com/email-change/confirm/q7Rk2vX9mB4nT0pL8sW1yZ3cF6hJ5dG2aE7uK9wQ0rM";

    fn hrefs(html: &str) -> Vec<String> {
        html.split("href=\"")
            .skip(1)
            .map(|rest| rest[..rest.find('"').unwrap()].replace("&amp;", "&"))
            .collect()
    }

    fn self_contained(html: &str) {
        assert!(html.len() < 100_000);
        let lower = html.to_ascii_lowercase();
        for banned in [
            "<script",
            "<img",
            "<link",
            "<iframe",
            "<form",
            "url(",
            "javascript:",
        ] {
            assert!(!lower.contains(banned), "{banned}");
        }
        assert!(html.contains("@media (prefers-color-scheme: dark) {"));
    }

    #[test]
    fn confirmation_in_both_languages() {
        let zh = confirmation(Lang::Zh, "new@example.com", LINK, 24);
        assert_eq!(zh.subject, "确认 Rivu 的新登录邮箱");
        assert_eq!(
            zh.body,
            format!(
                "确认以后用这个邮箱登录 Rivu。\n\n确认更换：{LINK}\n\n\
                 链接 24 小时内有效，只能用一次。确认后，其他设备需要重新登录。\n\n\
                 如果不是你本人操作，忽略这封邮件即可，登录邮箱不会改变。\n"
            )
        );
        assert_eq!(zh.to, "new@example.com");
        assert_eq!(zh.unsubscribe_url, None);
        let html = zh.html.unwrap();
        self_contained(&html);
        assert_eq!(hrefs(&html), [LINK]);
        assert!(html.contains(">确认新的登录邮箱</h1>") && html.contains(">确认更换</a>"));

        let en = confirmation(Lang::En, "new@example.com", LINK, 24);
        assert_eq!(en.subject, "Confirm your new Rivu sign-in email");
        assert!(en.body.contains(&format!("Confirm change: {LINK}\n")));
        assert!(en.body.contains("expires in 24 hours"));
        let html = en.html.unwrap();
        self_contained(&html);
        assert_eq!(hrefs(&html), [LINK]);
    }

    #[test]
    fn notice_masks_the_new_address_and_has_no_links() {
        let masked = mask_email("newname@example.com");
        assert_eq!(masked, "n***@example.com");
        for lang in [Lang::Zh, Lang::En] {
            let mail = changed_notice(lang, "old@example.com", &masked);
            assert_eq!(mail.to, "old@example.com");
            assert!(mail.body.contains("n***@example.com"), "{}", mail.body);
            assert!(!mail.body.contains("newname"));
            assert!(!mail.body.contains("://"));
            let html = mail.html.unwrap();
            self_contained(&html);
            assert!(hrefs(&html).is_empty());
            assert!(html.contains("n***@example.com") && !html.contains("newname"));
        }
        assert_eq!(
            changed_notice(Lang::Zh, "o@x.co", &masked).subject,
            "Rivu 登录邮箱已更改"
        );
    }

    #[test]
    fn masking_edge_cases() {
        assert_eq!(mask_email("a@b.co"), "a***@b.co");
        assert_eq!(mask_email("ñandú@ejemplo.es"), "ñ***@ejemplo.es");
        assert_eq!(mask_email("weird"), "***");
        // 转义在 HTML 里做
        let mail = changed_notice(Lang::En, "o@x.co", "<b>***@x.co");
        assert!(mail.html.unwrap().contains("&lt;b&gt;***@x.co"));
    }
}
