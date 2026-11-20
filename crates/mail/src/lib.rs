//! 发邮件：邀请、找回密码、更换登录邮箱、周报和月报（`RIVU_SMTP_URL`、`RIVU_SMTP_FROM`）。
//!
//! 邀请和找回密码只发纯文本。周报、月报及其确认邮件（[`weekly_report`]）和更换登录邮箱的两封邮件（[`email_change`]）另带一个 HTML 部分，
//! 以 `multipart/alternative` 发出，纯文本在前；HTML 也没有跟踪像素、远程图片和脚本。
//! 语言跟随触发它的请求（`Accept-Language` 以 `zh` 开头用中文，其余英文）。
//! 测试用 [`Mailer::memory`]，邮件存在内存里。

use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result};
use lettre::message::{
    Mailbox, MultiPart,
    header::{ContentType, HeaderName, HeaderValue},
};
use lettre::{AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor};

pub mod email_change;
pub mod weekly_report;

/// 一次 SMTP 会话的超时。
const SMTP_TIMEOUT: Duration = Duration::from_secs(20);

/// 邮件的语言。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lang {
    Zh,
    En,
}

impl Lang {
    /// 从 `Accept-Language` 取：第一个语言标签以 `zh` 开头为中文，其余（含缺失）为英文。
    pub fn from_accept_language(header: Option<&str>) -> Lang {
        let first = header
            .and_then(|h| h.split(',').next())
            .map(|t| t.trim().to_ascii_lowercase())
            .unwrap_or_default();
        if first.starts_with("zh") {
            Lang::Zh
        } else {
            Lang::En
        }
    }
}

/// 一封邮件。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Mail {
    pub to: String,
    pub subject: String,
    /// 纯文本正文。
    pub body: String,
    /// 有它时发 `multipart/alternative`（纯文本在前，HTML 在后）；没有时只发 `text/plain`。
    pub html: Option<String>,
    /// 有它时带 `List-Unsubscribe: <url>` 和 `List-Unsubscribe-Post: List-Unsubscribe=One-Click`
    /// （RFC 8058）：这个地址收到 POST 就退订，GET 只显示确认页。
    pub unsubscribe_url: Option<String>,
}

enum Transport {
    Smtp(AsyncSmtpTransport<Tokio1Executor>),
    Memory(Arc<Mutex<Vec<Mail>>>),
}

/// 发信器。
pub struct Mailer {
    transport: Transport,
    from: Mailbox,
}

/// [`Mailer::memory`] 的收件箱。
#[derive(Clone, Default)]
pub struct Outbox(Arc<Mutex<Vec<Mail>>>);

impl Outbox {
    pub fn take(&self) -> Vec<Mail> {
        self.0
            .lock()
            .map(|mut v| std::mem::take(&mut *v))
            .unwrap_or_default()
    }
}

impl Mailer {
    /// `url`：`smtps://user:pass@host:465` 或 `smtp://user:pass@host:587?tls=required`（lettre 的写法）。
    /// 要在 tokio 运行时里调用（连接池）。
    pub fn smtp(url: &str, from: &str) -> Result<Self> {
        let transport = AsyncSmtpTransport::<Tokio1Executor>::from_url(url)
            .context("RIVU_SMTP_URL 无效")?
            .timeout(Some(SMTP_TIMEOUT))
            .build();
        Ok(Mailer {
            transport: Transport::Smtp(transport),
            from: from.parse().context("RIVU_SMTP_FROM 不是有效的邮箱地址")?,
        })
    }

    /// 测试用：邮件存进返回的收件箱。
    pub fn memory(from: &str) -> (Self, Outbox) {
        let outbox = Outbox::default();
        let mailer = Mailer {
            transport: Transport::Memory(Arc::clone(&outbox.0)),
            from: from.parse().expect("测试用的发件人"),
        };
        (mailer, outbox)
    }

    /// 检查 SMTP 服务器能连上并通过认证（`rivu mail test`）。
    pub async fn check(&self) -> Result<()> {
        match &self.transport {
            Transport::Smtp(t) => {
                let ok = t.test_connection().await.context("连接 SMTP 服务器失败")?;
                anyhow::ensure!(ok, "SMTP 服务器没有响应");
                Ok(())
            }
            Transport::Memory(_) => Ok(()),
        }
    }

    pub async fn send(&self, mail: Mail) -> Result<()> {
        match &self.transport {
            Transport::Memory(v) => {
                if let Ok(mut v) = v.lock() {
                    v.push(mail);
                }
                Ok(())
            }
            Transport::Smtp(t) => {
                let msg = self.message(mail)?;
                t.send(msg).await.context("SMTP 发信失败")?;
                Ok(())
            }
        }
    }

    fn message(&self, mail: Mail) -> Result<Message> {
        let mut msg = Message::builder()
            .from(self.from.clone())
            .to(mail.to.parse().context("收件人不是有效的邮箱地址")?)
            .subject(mail.subject);
        if let Some(url) = mail.unsubscribe_url {
            msg = msg
                .raw_header(HeaderValue::new(
                    HeaderName::new_from_ascii_str("List-Unsubscribe"),
                    format!("<{url}>"),
                ))
                .raw_header(HeaderValue::new(
                    HeaderName::new_from_ascii_str("List-Unsubscribe-Post"),
                    "List-Unsubscribe=One-Click".to_owned(),
                ));
        }
        match mail.html {
            Some(html) => msg.multipart(MultiPart::alternative_plain_html(mail.body, html)),
            None => msg.header(ContentType::TEXT_PLAIN).body(mail.body),
        }
        .context("构造邮件失败")
    }
}

/// 邀请邮件。`inviter` 是邀请人的显示名或邮箱，`target` 是组织名（站点访客时是站点名）。
pub fn invitation(
    lang: Lang,
    to: &str,
    inviter: &str,
    target: &str,
    link: &str,
    days: u32,
) -> Mail {
    let (subject, body) = match lang {
        Lang::Zh => (
            format!("{inviter} 邀请你加入 {target}"),
            format!(
                "{inviter} 邀请你在 Rivu 上查看 {target} 的网站统计。\n\n\
                 打开下面的链接接受邀请（{days} 天内有效）：\n\n{link}\n\n\
                 如果你不认识邀请人，忽略这封邮件即可。\n"
            ),
        ),
        Lang::En => (
            format!("{inviter} invited you to {target}"),
            format!(
                "{inviter} invited you to view website analytics for {target} on Rivu.\n\n\
                 Open this link to accept the invitation (valid for {days} days):\n\n{link}\n\n\
                 If you don't know who sent this, you can ignore this email.\n"
            ),
        ),
    };
    Mail {
        to: to.to_owned(),
        subject,
        body,
        html: None,
        unsubscribe_url: None,
    }
}

/// 找回密码邮件。
pub fn password_reset(lang: Lang, to: &str, link: &str, minutes: u32) -> Mail {
    let (subject, body) = match lang {
        Lang::Zh => (
            "重设 Rivu 密码".to_owned(),
            format!(
                "有人请求重设这个邮箱在 Rivu 上的密码。\n\n\
                 打开下面的链接设置新密码（{minutes} 分钟内有效，只能用一次）：\n\n{link}\n\n\
                 重设后所有设备都需要重新登录。如果不是你本人操作，忽略这封邮件即可，密码不会改变。\n"
            ),
        ),
        Lang::En => (
            "Reset your Rivu password".to_owned(),
            format!(
                "Someone asked to reset the Rivu password for this email address.\n\n\
                 Open this link to choose a new password (valid for {minutes} minutes, single use):\n\n{link}\n\n\
                 After the reset you'll need to sign in again on every device. \
                 If you didn't ask for this, ignore this email and your password stays the same.\n"
            ),
        ),
    };
    Mail {
        to: to.to_owned(),
        subject,
        body,
        html: None,
        unsubscribe_url: None,
    }
}

/// `rivu mail test` 的测试邮件。
pub fn test_mail(to: &str) -> Mail {
    Mail {
        to: to.to_owned(),
        subject: "Rivu test email".to_owned(),
        body: "This is a test email from Rivu. SMTP is configured correctly.\n\
               这是 Rivu 的测试邮件，SMTP 配置正确。\n"
            .to_owned(),
        html: None,
        unsubscribe_url: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn smtp_message_contains_a_list_unsubscribe_uri() {
        let (mailer, _) = Mailer::memory("rivu@example.test");
        let url = format!(
            "https://stats.example.test/api/weekly-report/unsubscribe/{}",
            "a".repeat(43)
        );
        let mut mail = test_mail("recipient@example.test");
        mail.unsubscribe_url = Some(url.clone());
        let message = String::from_utf8(mailer.message(mail).unwrap().formatted()).unwrap();
        let unfolded = message.replace("\r\n ", " ");
        assert!(
            unfolded.contains(&format!("List-Unsubscribe: <{url}>\r\n")),
            "{unfolded}"
        );
        assert!(
            unfolded.contains("List-Unsubscribe-Post: List-Unsubscribe=One-Click\r\n"),
            "{unfolded}"
        );
    }

    #[test]
    fn plain_mail_stays_text_plain() {
        let (mailer, _) = Mailer::memory("rivu@example.test");
        let mail = password_reset(Lang::En, "a@example.test", "https://x/reset/t", 60);
        let message = String::from_utf8(mailer.message(mail).unwrap().formatted()).unwrap();
        assert!(message.contains("Content-Type: text/plain; charset=utf-8\r\n"));
        assert!(!message.contains("multipart"));
        assert!(!message.contains("List-Unsubscribe"));
    }

    #[test]
    fn html_mail_is_multipart_alternative_with_text_first() {
        let (mailer, _) = Mailer::memory("rivu@example.test");
        let mail = weekly_report::confirmation(
            Lang::Zh,
            "a@example.test",
            "https://x/weekly-report/confirm/t",
        );
        let message = String::from_utf8(mailer.message(mail).unwrap().formatted()).unwrap();
        let (head, parts) = message.split_once("\r\n\r\n").unwrap();
        assert!(
            head.contains("Content-Type: multipart/alternative;"),
            "{head}"
        );
        // 确认邮件不属于任何站点，没有退订头。
        assert!(!head.contains("List-Unsubscribe"));
        let plain = parts
            .find("Content-Type: text/plain; charset=utf-8")
            .unwrap();
        let html = parts
            .find("Content-Type: text/html; charset=utf-8")
            .unwrap();
        assert!(plain < html);
    }

    #[test]
    fn language_follows_the_first_tag() {
        for (h, want) in [
            (Some("zh-CN,zh;q=0.9,en;q=0.8"), Lang::Zh),
            (Some("ZH-tw"), Lang::Zh),
            (Some("en-US,zh;q=0.5"), Lang::En),
            (Some(""), Lang::En),
            (None, Lang::En),
        ] {
            assert_eq!(Lang::from_accept_language(h), want, "{h:?}");
        }
    }

    #[tokio::test]
    async fn memory_transport_records_mail() {
        let (m, outbox) = Mailer::memory("Rivu <rivu@example.test>");
        let mail = password_reset(Lang::Zh, "a@example.test", "https://x/reset/t", 60);
        assert!(mail.body.contains("https://x/reset/t") && mail.body.contains("60 分钟"));
        m.send(mail.clone()).await.unwrap();
        assert_eq!(outbox.take(), vec![mail]);
        assert!(outbox.take().is_empty());
        let inv = invitation(
            Lang::En,
            "b@example.test",
            "Lin",
            "Northwind",
            "https://x/invite/t",
            7,
        );
        assert_eq!(inv.subject, "Lin invited you to Northwind");
    }

    // 连接池在 drop 时要用 tokio 运行时
    #[tokio::test]
    async fn smtp_urls_are_parsed() {
        assert!(
            Mailer::smtp(
                "smtps://u:p@mail.example.com:465",
                "Rivu <rivu@example.com>"
            )
            .is_ok()
        );
        assert!(
            Mailer::smtp(
                "smtp://mail.example.com:587?tls=required",
                "rivu@example.com"
            )
            .is_ok()
        );
        assert!(Mailer::smtp("smtps://mail.example.com", "not an address").is_err());
        assert!(Mailer::smtp("http://mail.example.com", "rivu@example.com").is_err());
    }
}
