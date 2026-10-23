//! 来源（referrer → source）与渠道分类（`system-design.md` §4.2 第 8 步，§4.4）。
//!
//! 纯库，不接管线。输入是已经规范化的 referrer host（内部引荐由调用方判定并传 `None`）、
//! UTM、click id 参数名，输出 [`SourceInfo`]：`referrer_source` 可以直接填 `SessionAttrs`，
//! `channel` 写入 ClickHouse 的 `channel` 列。
//!
//! 数据与实现：
//! - 来源名数据是 Matomo 的 `searchengine-and-social-list`（CC0-1.0，固定 commit，见
//!   `data/matomo/PROVENANCE.md`）加自维护的补充表 `data/supplement.yml`；
//! - `build.rs` 在编译期把它们转成排好序的静态表，查询是二分，没有运行时解析和初始化；
//! - host 用**后缀匹配**：`www.google.co.jp` 命中键 `google.co.jp`；键里结尾的 `.{}` 是国家/地区
//!   后缀占位符，`google.{}` 命中 `google.com`、`google.de`、`google.co.uk`；
//!   同时命中多个键时，覆盖标签更多（更具体）的赢，平手取精确键；
//! - 渠道规则是一张有序表，先匹配先赢，取值集合与规则出处是 Google Analytics 4 默认渠道分组，
//!   偏离处都记在研究文档里。
//!
//! 研究、数据来源、规则表、测试统计：`docs/research/enrich-source.md`。

mod channel;
mod rules;
pub mod spam;
mod table;

use rivu_domain::Utm;

pub use channel::Channel;
pub use table::DATA_FINGERPRINT;

/// 规则版本。**渠道规则、渠道取值、来源数据（`data/`）任何一处变化都要加一**，
/// 规则变更后用离线 mutation 回填历史数据（`system-design.md` §4.2）。
///
/// 防漏改：`tests/source_data.rs` 把 `(RULES_VERSION, DATA_FINGERPRINT)` 钉在一起，数据变了而版本没变会测试失败。
pub const RULES_VERSION: u16 = 1;

/// 分类输入。
#[derive(Clone, Copy, Debug)]
pub struct SourceInput<'a> {
    /// 已按 hostname 规则规范化的 referrer host。**内部引荐（host 命中站点白名单）由调用方判定并传 `None`**，
    /// 没有 referrer 也是 `None`；本库不区分这两种情况。
    pub referrer_host: Option<&'a str>,
    /// 白名单内的 UTM 参数。`source` 已经包含 `utm_source` → `source` → `ref` 的回退。
    pub utm: &'a Utm,
    /// 第一个值非空的 click id 参数的**名字**（`gclid`、`msclkid`……）。
    pub click_id_param: Option<&'a str>,
}

/// 分类结果。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceInfo {
    /// 规范来源名（`Google`、`X (Twitter)`……）。认不出的 utm_source 原样保留（含大小写），
    /// 认不出的 referrer 用 host；没有来源时为空串。
    pub referrer_source: String,
    pub channel: Channel,
}

/// 分类。全函数、确定性：任何输入都有结果，同样的输入永远得到同样的结果，不 panic。
///
/// 来源优先级：`utm.source` > `referrer_host` > click id 推断；渠道按 `rules.rs` 里的有序规则表判定。
pub fn classify(input: &SourceInput<'_>) -> SourceInfo {
    let src = rules::resolve_source(
        input.referrer_host,
        input.utm.source.as_deref(),
        input.click_id_param,
    );
    let channel = rules::decide(&src, input.utm, input.click_id_param);
    SourceInfo {
        referrer_source: src.name.into_owned(),
        channel,
    }
}
