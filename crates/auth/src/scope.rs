//! API key 的作用域（`platform-and-ops.md` §4.3）与角色权限（§4.1 的子集）。
//!
//! scope 语法 `resource:action:target`，`target` 是 `*`（全部站点）或某个 `site_key`，如 `stats:read:*`。
//! 可用的 scope：`stats:read`、`sites:read`、`sites:write`、`exports:read`。
//! 成员管理、org 设置、API key 管理、删除站点与统计数据不开放给 key，只能由会话用户执行。
//! 默认只有 `stats:read:*`；Rivu 不做 Plausible 那种隐式的 `sites:read:*`。

use std::fmt;

/// scope 的资源。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Resource {
    Stats,
    Sites,
    Exports,
}

impl Resource {
    pub fn as_str(self) -> &'static str {
        match self {
            Resource::Stats => "stats",
            Resource::Sites => "sites",
            Resource::Exports => "exports",
        }
    }

    fn parse(s: &str) -> Option<Resource> {
        match s {
            "stats" => Some(Resource::Stats),
            "sites" => Some(Resource::Sites),
            "exports" => Some(Resource::Exports),
            _ => None,
        }
    }
}

/// scope 的动作。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ScopeAction {
    Read,
    Write,
}

impl ScopeAction {
    pub fn as_str(self) -> &'static str {
        match self {
            ScopeAction::Read => "read",
            ScopeAction::Write => "write",
        }
    }

    fn parse(s: &str) -> Option<ScopeAction> {
        match s {
            "read" => Some(ScopeAction::Read),
            "write" => Some(ScopeAction::Write),
            _ => None,
        }
    }
}

/// 允许的 `(资源, 动作)` 组合。
pub const ALLOWED_SCOPES: [(Resource, ScopeAction); 4] = [
    (Resource::Stats, ScopeAction::Read),
    (Resource::Sites, ScopeAction::Read),
    (Resource::Sites, ScopeAction::Write),
    (Resource::Exports, ScopeAction::Read),
];

/// 默认 scope（建 key 时没有指定）。
pub const DEFAULT_SCOPE: &str = "stats:read:*";

/// scope 的目标。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    /// `*`：key 所属组织的全部站点。
    All,
    Site(String),
}

/// 一条 scope。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Scope {
    pub resource: Resource,
    pub action: ScopeAction,
    pub target: Target,
}

impl fmt::Display for Scope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let target = match &self.target {
            Target::All => "*",
            Target::Site(k) => k,
        };
        write!(
            f,
            "{}:{}:{target}",
            self.resource.as_str(),
            self.action.as_str()
        )
    }
}

pub use rivu_domain::is_site_key;

impl Scope {
    /// 解析 `resource:action:target`。
    pub fn parse(s: &str) -> Result<Scope, String> {
        let mut parts = s.split(':');
        let (Some(res), Some(act), Some(target), None) =
            (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            return Err(format!(
                "scope `{s}` 的格式应为 resource:action:target，例如 stats:read:*"
            ));
        };
        let resource = Resource::parse(res).ok_or_else(|| {
            format!("scope `{s}` 的资源 `{res}` 未知，可选 stats、sites、exports")
        })?;
        let action = ScopeAction::parse(act)
            .ok_or_else(|| format!("scope `{s}` 的动作 `{act}` 未知，可选 read、write"))?;
        if !ALLOWED_SCOPES.contains(&(resource, action)) {
            return Err(format!(
                "scope `{s}` 不可用；可用的：stats:read、sites:read、sites:write、exports:read"
            ));
        }
        let target = if target == "*" {
            Target::All
        } else if is_site_key(target) {
            Target::Site(target.to_owned())
        } else {
            return Err(format!("scope `{s}` 的目标必须是 `*` 或 12 位的 site_key"));
        };
        Ok(Scope {
            resource,
            action,
            target,
        })
    }

    /// 命令行用：`stats:read`（省略目标）按 `stats:read:*` 处理，再校验并规范化。
    pub fn normalize(s: &str) -> Result<String, String> {
        let s = s.trim();
        let full = if s.matches(':').count() == 1 {
            format!("{s}:*")
        } else {
            s.to_owned()
        };
        Scope::parse(&full).map(|sc| sc.to_string())
    }
}

/// 一把 key 的全部 scope。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Scopes(Vec<Scope>);

impl Scopes {
    /// 解析库里的 scope 文本。无法解析的条目被忽略并记 `warn`（fail-closed：少授权，不多授权）。
    pub fn from_texts(texts: &[String]) -> Scopes {
        let mut out = Vec::with_capacity(texts.len());
        for t in texts {
            match Scope::parse(t) {
                Ok(s) => out.push(s),
                Err(e) => {
                    tracing::warn!(scope = %t, error = %e, "API key 里有无法解析的 scope，已忽略")
                }
            }
        }
        Scopes(out)
    }

    pub fn iter(&self) -> impl Iterator<Item = &Scope> {
        self.0.iter()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// 有没有任何一条 scope 授予该资源与动作（不论目标）。这是路由层的「403」判定：
    /// key 压根没有这个能力。
    pub fn grants(&self, resource: Resource, action: ScopeAction) -> bool {
        self.0
            .iter()
            .any(|s| s.resource == resource && s.action == action)
    }

    /// 该资源与动作是否覆盖这个站点：有 `*` 目标，或目标恰好是这个 `site_key`。
    pub fn covers_site(&self, resource: Resource, action: ScopeAction, site_key: &str) -> bool {
        self.0.iter().any(|s| {
            s.resource == resource
                && s.action == action
                && match &s.target {
                    Target::All => true,
                    Target::Site(k) => k == site_key,
                }
        })
    }
}

/// 用户在组织里的角色（`memberships.role`）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum OrgRole {
    Viewer,
    Editor,
    Admin,
    Owner,
}

impl OrgRole {
    pub fn parse(s: &str) -> Option<OrgRole> {
        match s {
            "owner" => Some(OrgRole::Owner),
            "admin" => Some(OrgRole::Admin),
            "editor" => Some(OrgRole::Editor),
            "viewer" => Some(OrgRole::Viewer),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            OrgRole::Owner => "owner",
            OrgRole::Admin => "admin",
            OrgRole::Editor => "editor",
            OrgRole::Viewer => "viewer",
        }
    }
}

/// 动作：API key 能碰到的那几行（`platform-and-ops.md` §4.1）加上看板站点管理端点用的几行（`Site*`、`SharedLinkManage`、
/// `SiteSettingsRead`；key 的 scope 不映射到它们）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    /// `stats.read`：所有角色。
    StatsRead,
    /// 读站点信息：所有角色（矩阵没有单独一行，按「成员都能看到站点」处理）。
    SitesRead,
    /// `site.settings.write`：owner、admin。
    SitesWrite,
    /// 读导出：与 `export.create` 同级，owner、admin、editor（矩阵没有「读导出」一行）。
    ExportsRead,
    /// 看板读站点设置、屏蔽规则与共享链接列表（矩阵没有单独一行；取与它们的写动作之下一级）：owner、admin、editor。
    SiteSettingsRead,
    /// `site.settings.write`（设置、hostnames、shield）看板入口：owner、admin。与 [`Action::SitesWrite`] 同级，
    /// 单列是因为看板的判定只看组织角色，不看 API key 的 scope。
    SiteSettingsWrite,
    /// `site.public.toggle`：owner、admin。
    SitePublicToggle,
    /// `site.create`：owner、admin。
    SiteCreate,
    /// `site.delete`：owner、admin。
    SiteDelete,
    /// `stats.delete`（按日期范围删除统计数据）：owner、admin。
    StatsDelete,
    /// `shared_link.manage`：owner、admin、editor。
    SharedLinkManage,
    /// `site.content.write`（目标的增删）：owner、admin、editor；看板按有效角色判定，站点访客 editor 也可以。
    SiteContentWrite,
    /// `member.invite`、`member.remove`、`member.role.change`：owner、admin（admin 能碰到哪些角色见 [`can_assign`]、[`can_modify`]）。
    MemberManage,
    /// `site_membership.manage`（站点访客的邀请与移除）：owner、admin。
    SiteMembershipManage,
    /// `api_key.manage_own`（管理自己的 API key）：所有组织角色（站点访客不是组织成员，没有 key）。
    ApiKeyManageOwn,
}

impl Action {
    /// scope 的 `(资源, 动作)` 对应的矩阵动作。
    pub fn of(resource: Resource, action: ScopeAction) -> Action {
        match (resource, action) {
            (Resource::Stats, _) => Action::StatsRead,
            (Resource::Sites, ScopeAction::Read) => Action::SitesRead,
            (Resource::Sites, ScopeAction::Write) => Action::SitesWrite,
            (Resource::Exports, _) => Action::ExportsRead,
        }
    }
}

/// 角色 × 动作矩阵（纯函数，穷举 `match`：新增动作必须补全）。
pub fn allowed(role: OrgRole, action: Action) -> bool {
    match action {
        Action::StatsRead | Action::SitesRead | Action::ApiKeyManageOwn => true,
        Action::ExportsRead
        | Action::SiteSettingsRead
        | Action::SharedLinkManage
        | Action::SiteContentWrite => role >= OrgRole::Editor,
        Action::SitesWrite
        | Action::SiteSettingsWrite
        | Action::SitePublicToggle
        | Action::SiteCreate
        | Action::SiteDelete
        | Action::StatsDelete
        | Action::MemberManage
        | Action::SiteMembershipManage => role >= OrgRole::Admin,
    }
}

/// `actor` 能不能把某个成员的角色设成（或以此角色邀请）`target`：owner 可以给任何角色，admin 只能给 admin、editor、viewer
/// （不能造出 owner），editor 与 viewer 不能管理成员。调用方仍要先过 [`Action::MemberManage`]。
pub fn can_assign(actor: OrgRole, target: OrgRole) -> bool {
    match actor {
        OrgRole::Owner => true,
        OrgRole::Admin => target <= OrgRole::Admin,
        OrgRole::Editor | OrgRole::Viewer => false,
    }
}

/// `actor` 能不能改动或移除当前角色是 `member` 的成员：owner 可以改任何人，admin 不能碰 owner。
pub fn can_modify(actor: OrgRole, member: OrgRole) -> bool {
    match actor {
        OrgRole::Owner => true,
        OrgRole::Admin => member != OrgRole::Owner,
        OrgRole::Editor | OrgRole::Viewer => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: &str = "abcdefghijkl";
    const OTHER: &str = "mnopqrstuvwx";

    #[test]
    fn scopes_parse_and_round_trip() {
        for ok in [
            "stats:read:*",
            "sites:read:*",
            "sites:write:*",
            "exports:read:*",
            "stats:read:abcdefghijkl",
        ] {
            assert_eq!(Scope::parse(ok).unwrap().to_string(), ok);
        }
        assert_eq!(Scope::parse("stats:read:*").unwrap().target, Target::All);
        assert_eq!(
            Scope::parse(&format!("stats:read:{KEY}")).unwrap().target,
            Target::Site(KEY.into())
        );
    }

    #[test]
    fn invalid_scopes_are_rejected_with_a_reason() {
        for bad in [
            "",
            "stats",
            "stats:read",
            "stats:read:*:x",
            "stats:write:*",
            "exports:write:*",
            "members:read:*",
            "stats:delete:*",
            "STATS:read:*",
            "stats:read:site",
            "stats:read:ABCDEFGHIJKL",
            "stats:read:abcdefghijk",
            "stats:read:abcdefghijkl1",
            "stats:read:abcdefghij18",
        ] {
            let e = Scope::parse(bad).unwrap_err();
            assert!(!e.is_empty(), "{bad:?}");
        }
    }

    #[test]
    fn normalize_adds_the_wildcard_target_for_the_two_part_form() {
        assert_eq!(Scope::normalize("stats:read").unwrap(), "stats:read:*");
        assert_eq!(Scope::normalize(" sites:write ").unwrap(), "sites:write:*");
        assert_eq!(Scope::normalize("stats:read:*").unwrap(), "stats:read:*");
        assert_eq!(
            Scope::normalize(&format!("stats:read:{KEY}")).unwrap(),
            format!("stats:read:{KEY}")
        );
        assert!(Scope::normalize("stats").is_err());
        assert!(Scope::normalize("exports:write").is_err());
    }

    #[test]
    fn coverage_distinguishes_resource_action_and_target() {
        let s = Scopes::from_texts(&["stats:read:*".into(), format!("sites:read:{KEY}")]);
        // 路由层：有没有这个能力
        assert!(s.grants(Resource::Stats, ScopeAction::Read));
        assert!(s.grants(Resource::Sites, ScopeAction::Read));
        assert!(!s.grants(Resource::Sites, ScopeAction::Write));
        assert!(!s.grants(Resource::Exports, ScopeAction::Read));
        // 站点层：目标
        assert!(s.covers_site(Resource::Stats, ScopeAction::Read, KEY));
        assert!(s.covers_site(Resource::Stats, ScopeAction::Read, OTHER));
        assert!(s.covers_site(Resource::Sites, ScopeAction::Read, KEY));
        assert!(!s.covers_site(Resource::Sites, ScopeAction::Read, OTHER));
        assert!(!s.covers_site(Resource::Sites, ScopeAction::Write, KEY));
    }

    #[test]
    fn unparsable_stored_scopes_grant_nothing() {
        let s = Scopes::from_texts(&["bogus".into(), "stats:write:*".into(), "stats:read:".into()]);
        assert!(s.is_empty());
        assert!(!s.grants(Resource::Stats, ScopeAction::Read));
        // 一条坏的不影响好的
        let s = Scopes::from_texts(&["bogus".into(), "stats:read:*".into()]);
        assert!(s.grants(Resource::Stats, ScopeAction::Read));
        assert_eq!(s.iter().count(), 1);
    }

    #[test]
    fn role_action_matrix() {
        use OrgRole::*;
        let cases = [
            (Action::StatsRead, [true, true, true, true]),
            (Action::SitesRead, [true, true, true, true]),
            (Action::ExportsRead, [true, true, true, false]),
            (Action::SitesWrite, [true, true, false, false]),
            (Action::SiteSettingsRead, [true, true, true, false]),
            (Action::SiteSettingsWrite, [true, true, false, false]),
            (Action::SitePublicToggle, [true, true, false, false]),
            (Action::SiteCreate, [true, true, false, false]),
            (Action::SiteDelete, [true, true, false, false]),
            (Action::StatsDelete, [true, true, false, false]),
            (Action::SharedLinkManage, [true, true, true, false]),
            (Action::SiteContentWrite, [true, true, true, false]),
            (Action::MemberManage, [true, true, false, false]),
            (Action::SiteMembershipManage, [true, true, false, false]),
            (Action::ApiKeyManageOwn, [true, true, true, true]),
        ];
        for (action, expected) in cases {
            for (role, want) in [Owner, Admin, Editor, Viewer].into_iter().zip(expected) {
                assert_eq!(allowed(role, action), want, "{role:?} {action:?}");
            }
        }
        assert_eq!(OrgRole::parse("owner"), Some(Owner));
        assert_eq!(OrgRole::parse("Owner"), None);
        assert_eq!(OrgRole::parse("guest"), None);
        for r in [Owner, Admin, Editor, Viewer] {
            assert_eq!(OrgRole::parse(r.as_str()), Some(r));
        }
    }

    #[test]
    fn member_role_assignment_and_modification_rules() {
        use OrgRole::*;
        let all = [Owner, Admin, Editor, Viewer];
        // owner：任何角色都能给、任何人都能改
        for r in all {
            assert!(can_assign(Owner, r), "owner 给 {r:?}");
            assert!(can_modify(Owner, r), "owner 改 {r:?}");
        }
        // admin：不能造出 owner，也不能碰 owner
        assert!(!can_assign(Admin, Owner));
        for r in [Admin, Editor, Viewer] {
            assert!(can_assign(Admin, r), "admin 给 {r:?}");
            assert!(can_modify(Admin, r), "admin 改 {r:?}");
        }
        assert!(!can_modify(Admin, Owner));
        // editor、viewer：什么都不行
        for actor in [Editor, Viewer] {
            for r in all {
                assert!(!can_assign(actor, r), "{actor:?} 给 {r:?}");
                assert!(!can_modify(actor, r), "{actor:?} 改 {r:?}");
            }
        }
    }

    #[test]
    fn scope_to_action_mapping_is_total_for_allowed_scopes() {
        for (res, act) in ALLOWED_SCOPES {
            let _ = Action::of(res, act);
        }
        assert_eq!(
            Action::of(Resource::Stats, ScopeAction::Read),
            Action::StatsRead
        );
        assert_eq!(
            Action::of(Resource::Sites, ScopeAction::Write),
            Action::SitesWrite
        );
    }
}
