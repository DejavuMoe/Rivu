//! 看板的站点可见性：登录用户能看到哪些站点、站点的归属（`platform-and-ops.md` §4.2 第 1、4、6 步）。
//!
//! 「可见」= 未删除的站点，其组织未删除，且用户是该组织的成员（`memberships`）或被授予站点访客角色
//! （`site_memberships`）。实例管理员（`users.is_instance_admin`）**没有**隐式访问权。

use anyhow::{Context, Result};
use sqlx::{PgPool, Row};

use crate::sites::SiteOwner;

/// 站点的引用：看板路径里的 `{site_id}` 可以是数字 id，也可以是 12 位的 `site_key`。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SiteRef {
    Id(u64),
    Key(String),
}

impl SiteRef {
    /// 解析路径段：`^[a-z2-7]{12}$` 是 `site_key`（全是 2–7 的 12 位数字也算 key：id 不会长到那个量级），
    /// 其余纯数字是 id，其他无效。
    pub fn parse(s: &str) -> Option<SiteRef> {
        let is_key = s.len() == 12
            && s.bytes()
                .all(|b| b.is_ascii_lowercase() || (b'2'..=b'7').contains(&b));
        if is_key {
            return Some(SiteRef::Key(s.to_owned()));
        }
        if !s.is_empty() && s.len() <= 18 && s.bytes().all(|b| b.is_ascii_digit()) {
            return s.parse().ok().map(SiteRef::Id);
        }
        None
    }
}

/// 按引用查未删除、组织未删除的站点。
pub async fn find_site(pool: &PgPool, site: &SiteRef) -> Result<Option<SiteOwner>> {
    let (id, key) = match site {
        SiteRef::Id(id) => (Some(i64::try_from(*id).context("site_id 超出范围")?), None),
        SiteRef::Key(k) => (None, Some(k.as_str())),
    };
    let row = sqlx::query(
        "SELECT s.id, s.site_key, s.organization_id, s.is_public
         FROM sites s JOIN organizations o ON o.id = s.organization_id AND o.deleted_at IS NULL
         WHERE s.deleted_at IS NULL AND (s.id = $1 OR s.site_key = $2)",
    )
    .bind(id)
    .bind(key)
    .fetch_optional(pool)
    .await
    .context("查询站点失败")?;
    row.map(|r| {
        let id: i64 = r.try_get("id")?;
        Ok(SiteOwner {
            id: u64::try_from(id).context("sites.id 为负数")?,
            site_key: r.try_get("site_key")?,
            organization_id: r.try_get("organization_id")?,
            is_public: r.try_get("is_public")?,
        })
    })
    .transpose()
}

/// 站点的展示信息（公开看板的入口用：站点名与时区）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SiteSummary {
    pub name: String,
    pub timezone: String,
}

pub async fn site_summary(pool: &PgPool, site_id: u64) -> Result<Option<SiteSummary>> {
    let id = i64::try_from(site_id).context("site_id 超出范围")?;
    let row = sqlx::query("SELECT name, timezone FROM sites WHERE id = $1 AND deleted_at IS NULL")
        .bind(id)
        .fetch_optional(pool)
        .await
        .context("查询站点信息失败")?;
    row.map(|r| {
        Ok(SiteSummary {
            name: r.try_get("name")?,
            timezone: r.try_get("timezone")?,
        })
    })
    .transpose()
}

/// 用户在站点上的角色：组织角色与站点访客角色（都可能为空；都为空 = 无权访问）。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct UserSiteRoles {
    pub org_role: Option<String>,
    pub guest_role: Option<String>,
}

impl UserSiteRoles {
    pub fn any(&self) -> bool {
        self.org_role.is_some() || self.guest_role.is_some()
    }
}

pub async fn user_site_roles(
    pool: &PgPool,
    user_id: i64,
    site: &SiteOwner,
) -> Result<UserSiteRoles> {
    let site_id = i64::try_from(site.id).context("site_id 超出范围")?;
    let org_role: Option<String> = sqlx::query_scalar(
        "SELECT role FROM memberships WHERE organization_id = $1 AND user_id = $2 AND deleted_at IS NULL",
    )
    .bind(site.organization_id)
    .bind(user_id)
    .fetch_optional(pool)
    .await
    .context("查询组织成员关系失败")?;
    let guest_role: Option<String> = sqlx::query_scalar(
        "SELECT role FROM site_memberships WHERE site_id = $1 AND user_id = $2 AND deleted_at IS NULL",
    )
    .bind(site_id)
    .bind(user_id)
    .fetch_optional(pool)
    .await
    .context("查询站点访客角色失败")?;
    Ok(UserSiteRoles {
        org_role,
        guest_role,
    })
}

/// 看板站点列表的一行。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VisibleSite {
    pub id: u64,
    pub site_key: String,
    pub name: String,
    pub timezone: String,
    pub is_public: bool,
    pub org_id: i64,
    pub org_slug: String,
    pub org_name: String,
    pub roles: UserSiteRoles,
}

/// 用户可见的站点，按组织名、站点名排序。
pub async fn list_visible_sites(pool: &PgPool, user_id: i64) -> Result<Vec<VisibleSite>> {
    let rows = sqlx::query(
        "SELECT s.id, s.site_key, s.name, s.timezone, s.is_public,
                o.id AS org_id, o.slug AS org_slug, o.name AS org_name,
                m.role AS org_role, sm.role AS guest_role
         FROM sites s
         JOIN organizations o ON o.id = s.organization_id AND o.deleted_at IS NULL
         LEFT JOIN memberships m ON m.organization_id = s.organization_id
                                AND m.user_id = $1 AND m.deleted_at IS NULL
         LEFT JOIN site_memberships sm ON sm.site_id = s.id AND sm.user_id = $1 AND sm.deleted_at IS NULL
         WHERE s.deleted_at IS NULL AND (m.id IS NOT NULL OR sm.id IS NOT NULL)
         ORDER BY o.name, o.id, s.name, s.id",
    )
    .bind(user_id)
    .fetch_all(pool)
    .await
    .context("列出可见站点失败")?;
    rows.iter()
        .map(|r| {
            let id: i64 = r.try_get("id")?;
            Ok(VisibleSite {
                id: u64::try_from(id).context("sites.id 为负数")?,
                site_key: r.try_get("site_key")?,
                name: r.try_get("name")?,
                timezone: r.try_get("timezone")?,
                is_public: r.try_get("is_public")?,
                org_id: r.try_get("org_id")?,
                org_slug: r.try_get("org_slug")?,
                org_name: r.try_get("org_name")?,
                roles: UserSiteRoles {
                    org_role: r.try_get("org_role")?,
                    guest_role: r.try_get("guest_role")?,
                },
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn site_refs_distinguish_keys_from_ids() {
        assert_eq!(
            SiteRef::parse("k7qm2xwatbrd"),
            Some(SiteRef::Key("k7qm2xwatbrd".into()))
        );
        assert_eq!(SiteRef::parse("42"), Some(SiteRef::Id(42)));
        assert_eq!(SiteRef::parse("007"), Some(SiteRef::Id(7)));
        for bad in [
            "",
            "-1",
            "4 2",
            "K7QM2XWATBRD",
            "k7qm2xwatbr",
            "k7qm2xwatbrde",
            "1234567890123456789",
            "0x10",
            "1e3",
            "42/../1",
        ] {
            assert_eq!(SiteRef::parse(bad), None, "{bad:?}");
        }
    }
}
