# 解析器模糊测试（cargo-fuzz / libFuzzer）

日期：2026-10-23。代码基线：提交 `061593d`（`fuzz/` 目录与下文的修复在其之上，未提交）。

## 结论

- 5 个模糊目标，每个在 WSL 里跑满 10 分钟（6 个并行 job，`-max_total_time=600`），累计 **9 千 5 百多万次执行**，没有崩溃、断言失败或超时。
- 一个缺陷是在 15 秒冒烟运行（正式长跑之前）里发现的：`validated_pattern("www.www.071")` 返回的规范形式不是不动点，存下来重新编译后规则含义变了（见「发现与修复」）。已修复并加了回归测试。修复之后的 10 分钟长跑里同一目标没有再失败。
- 用故意植入的缺陷验证过差分目标确实有牙：把页面通配匹配的后缀边界 `>=` 改成 `>`，`shield_diff` 在 5 秒内就报出反例（改动已还原，没有留在工作区）。

## 工具与环境

| 项 | 值 |
|---|---|
| cargo-fuzz | 0.13.2（`cargo install cargo-fuzz --locked`） |
| libfuzzer-sys | 0.4.13；`arbitrary` 1.5.0 |
| Rust（模糊测试用） | nightly：`rustc 1.101.0-nightly (7189d39d9 2026-10-22)`，装在 Debian 的 rustup 里（`rustup toolchain install nightly --profile minimal`），没有动 Windows 工具链 |
| Rust（仓库其余部分） | stable：Debian 上 `rustc 1.98.1`，Windows 上 1.94 起（`rust-toolchain.toml`） |
| 机器 | Intel Core Ultra 7 255H，WSL2 分到 8 个逻辑核、约 12 GB 内存，Debian 13，内核 6.18.33.2-microsoft-standard-WSL2 |
| 并行 | 每个目标 `-jobs=6 -workers=6`，`-max_len=16384`，各目标有字典（`fuzz/dicts/`，只有 `parse_event` 与 `query_spec` 有） |

`fuzz/` 是独立 crate（`rivu-fuzz`），不在工作区 `members` 里：根 `Cargo.toml` 的 `exclude = ["fuzz"]`，有自己的 `Cargo.lock` 与 `target/`，也不继承工作区的 `unsafe_code = "forbid"`（`fuzz_target!` 宏展开里有 unsafe）。依赖都是指向 `../crates/*` 的路径依赖。

## 目标

| 目标 | 被测对象 | 输入 | 断言 |
|---|---|---|---|
| `parse_event` | `rivu_ingest::protocol::parse_event`（间接覆盖页面 URL 规范化、hostname 规范化、referrer、props、click id） | 任意字节 | 不 panic；同一输入两次结果相等（确定性）；成功时 `pathname` 以 `/` 开头且 ≤ 1024 字节、hostname 非空、事件名 ≤ 120 字符、props ≤ 30、`engagement_ms` < 30 天、`scroll_depth` ≤ 100 或 255、referrer 的落库形态不含 `?` `#`；失败时错误非空且 `to_json()` 是 JSON 对象 |
| `query_spec` | `QuerySpec::parse_str` → resolve → SQL 生成（`rivu_query::explain`）；站点上下文用固定值（站点 4242；时区 UTC / Europe/Berlin / Pacific/Kiritimati，周起始日各异；`now` 固定在 2026-10-23；`all` 的数据起点固定） | 任意 UTF-8 JSON 文本 | 解析成功的 `QuerySpec` 序列化再解析得到同一个值（往返）；能生成 SQL 时：SQL 里没有 `?`、带 `{site:UInt64}`、每个 `{name:Type}` 占位符都有同名同型的参数、过滤里长度 ≥ 6 的取值字符串不出现在 SQL 文本里（以「去掉全部过滤后的 SQL」作基线，结构词与含 `{` 的取值除外） |
| `hostname` | `normalize_hostname`、`HostnamePatterns::new/matches`、`validated_pattern` | 第一行是被测 host，其余每行一个模式 | 不 panic；`normalize_hostname` 幂等（文档写明的 `www.` 开头结果除外）；`validated_pattern` 成功时结果被 `HostnamePatterns` 接受，且再校验得到同一个值（不动点） |
| `shield_diff` | `ShieldRules`：IP / CIDR 二叉前缀树（IPv4、IPv6、IPv4-mapped）、页面通配、deny / allow 语义 | `arbitrary` 结构化输入：一组 IP/CIDR 规则 + 一组页面模式 + 若干查询 IP 与路径 | `ip_blocked`、`page_blocked` 与朴素实现逐一相等（朴素实现：整数掩码比较；O(nm) 的 DP 通配）。规则里的网段端点、把模式里的 `*` 换成字面量得到的路径也被当作查询（边界）。另外对随机文本喂 `normalize_value`（ip / cidr / page / country），成功的结果必须是不动点 |
| `source_classify` | `rivu_source::classify`、`spam::is_spam_referrer` | `arbitrary` 结构化输入：referrer host、五个 UTM 字段、click id 名、一个垃圾引荐候选 host | `classify` 不 panic、确定性；`is_spam_referrer` 与朴素后缀匹配（遍历 `spam::entries()`）逐一相等 |

页面通配匹配器与 CIDR 前缀树在 `rivu_ingest::shield` 里是私有类型，只能经公开的 `ShieldRules` 间接测，所以差分目标走的是 `ShieldRules::compile` + `ip_blocked` / `page_blocked`（与线上路径一致，连规则校验和规范化一起测）。

## 种子语料

入库的种子在 `fuzz/corpus/<target>/`，都是几十到几百字节的小文件：

| 目标 | 文件数 | 来源 |
|---|---|---|
| `parse_event` | 9 | `crates/ingest/src/protocol.rs` 单元测试里的请求体（最小 pageview、带 utm 与 referrer、自定义事件与收入、engagement、错误体）与 `tracker-and-protocol.md` 的示例，外加 hash 模式、IPv6/百分号转义、非 http scheme、非对象 JSON |
| `query_spec` | 12 | `crates/query/src/sql_tests.rs` 快照用的查询（overview、国家榜、页面榜、过滤树全部算子、时间序列、对比、`exit_rate`、`percentage`、`all`）与注入用例，外加自定义时间范围 |
| `hostname` | 4 | `crates/ingest/src/hostname.rs` 单元测试里的写法：大小写与端口、通配、punycode / Unicode、IP 字面量、无效模式 |
| `shield_diff`、`source_classify` | 6、7 | 输入是 `arbitrary` 的二进制编码，没有可读的「合法输入」，种子是固定随机种子生成的短二进制文件（另有一个含 `semalt.com` 的） |

跑的时候新语料写进 `~/fuzzwork/corpus/<target>/`（在构建镜像之外，不会被下一次同步删掉），入库种子只读。运行后的语料没有入库：5 万个左右的小文件对仓库没有意义，需要时重跑即可。

## 运行结果

每个目标 601 秒，6 个 job 并行，同一时刻只有一个目标在跑。执行次数是 6 个 job 的 `Done N runs` 之和；覆盖（`cov`，libFuzzer 的覆盖计数，含被测代码所依赖的 crate）与特征（`ft`）取各 job 的最大值，语料大小取单个 job 最终的 `corp:`。

| 目标 | 执行次数 | 平均每秒 | cov | ft | 单 job 语料 | 崩溃 / 失败 |
|---|---:|---:|---:|---:|---|---:|
| `parse_event` | 18,682,219 | 31,085 | 5,285 | 19,661 | 约 4,550 个 / 1.57 MB | 0 |
| `query_spec` | 6,812,039 | 11,335 | 14,806 | 55,150 | 约 4,535 个 / 2.0 MB | 0 |
| `hostname` | 15,481,330 | 25,759 | 2,958 | 21,507 | 约 4,900 个 / 1.3 MB | 0 |
| `shield_diff` | 24,510,564 | 40,783 | 1,349 | 7,736 | 约 1,900 个 / 0.52 MB | 0 |
| `source_classify` | 29,572,740 | 49,206 | 1,394 | 3,754 | 约 1,185 个 / 0.23 MB | 0 |
| 合计 | 95,058,892 | | | | | 0 |

说明与局限：

- `parse_event` 的长跑期间，我同时在同一台机器上演练过一次压测脚本（几分钟的 rivu + loadgen），该目标的吞吐因此略低；其余目标独占机器。这不影响「有没有崩溃」的结论。
- 覆盖曲线在最后几分钟基本平了（见各 job 日志里 `NEW` 行的稀疏程度），但 10 分钟远不是「穷尽」：`query_spec` 的覆盖最大（14.8k），增长也最慢，值得以后在专门的机器上跑几小时。
- `query_spec` 里「取值不出现在 SQL」是启发式断言（基线对比，长度 ≥ 6）；更强的保证来自 `crates/query` 里已有的注入测试与「所有取值走参数」的构造方式，这里只是多一层随机输入的检查。
- 模糊目标只验证「不崩、确定、与朴素实现一致」，不验证业务正确性（口径对不对由各 crate 的测试与参考实现差分负责）。

## 发现与修复

### 1. `validated_pattern` 的结果不是不动点：`www.www.x` 存下来后含义改变（已修复）

- 发现：`hostname` 目标，第一次 15 秒冒烟运行（2 个 job，约 30 万次执行）。断言：`validated_pattern("www.www.071")` 得到 `www.071`，再校验得到 `071`。
- 最小化：`cargo fuzz tmin hostname <崩溃文件>`（在去掉修复的工作区上重现后做的）得到 10 字节输入，等价于 `\nwww.www.x`（第一行空，第二行是模式）。
- 根因：`normalize_hostname` 只去**一个**前导 `www.`（文档如此规定），所以 `www.www.x` 规范化为 `www.x`。屏蔽规则的 `hostname` 类型把 `validated_pattern` 的结果写进 `shield_rules.value`，而站点快照加载规则时又用 `HostnamePatterns::new` 编译，它会对精确模式再规范化一次、再去掉一个 `www.`。结果：用户写的 `deny www.www.example.com` 在库里是 `www.example.com`，运行时编译成精确匹配 `example.com`——屏蔽的是另一个主机（`example.com`），要屏蔽的 `www.www.example.com` 反而放行。通配模式（`*.www.x`）与站点 hostname 白名单（存原文）不受影响。
- 修复（`crates/ingest/src/hostname.rs`，最小改动）：`validated_pattern` 在精确模式规范化后仍以 `www.` 开头时拒绝，错误信息说明原因。不动不动点之外的任何行为：合法输入的结果不变。这是一个行为变化：此前被接受的 `www.www.x` 现在被拒绝（它们本来也是错的）。
- 回归测试（同文件 `mod tests`）：
  - `validated_pattern_rejects_double_www_instead_of_changing_meaning`：`www.www.071`、`WWW.www.example.com.` 被拒；`*.www.example.com` 与 `www.example.com` 的结果不变；
  - `validated_patterns_are_fixed_points`：一组合法写法（大小写、端口、通配、punycode、IP）的结果再校验得到同一个值。
- 验证：`cargo test -p rivu-ingest` 通过；修复后 `hostname` 目标 10 分钟、1548 万次执行无失败。

没有其他发现。`parse_event`（1868 万次）、`query_spec`（681 万次，含序列化往返与 SQL 占位符检查）、`shield_diff`（2451 万次，与朴素 CIDR / 通配实现逐一相等）、`source_classify`（2957 万次）都没有失败。

## 复现

在仓库根（Windows 源码，经运行器在 WSL 里跑）：

```powershell
# 一次性：装 nightly 与 cargo-fuzz（Debian 里）
& D:\WSL\Scripts\linux-task.ps1 -Mode direct -Project 'D:\Forgejo\Rivu' -Command 'rustup toolchain install nightly --profile minimal; cargo install cargo-fuzz --locked'

# 构建全部目标
& D:\WSL\Scripts\linux-task.ps1 -Mode build -Project 'D:\Forgejo\Rivu' -Command 'cargo +nightly fuzz build'

# 长跑一个目标：fuzz/run.sh <目标> [秒数=600] [jobs=4]；工作语料、崩溃产物与日志在 ~/fuzzwork（构建镜像之外）
& D:\WSL\Scripts\linux-task.ps1 -Mode build -Project 'D:\Forgejo\Rivu' -Command 'bash fuzz/run.sh parse_event 600 6'

# 本文的数字：依次跑五个目标
& D:\WSL\Scripts\linux-task.ps1 -Mode build -Project 'D:\Forgejo\Rivu' -Command 'for t in parse_event query_spec hostname shield_diff source_classify; do bash fuzz/run.sh $t 600 6; done'

# 重放 / 最小化一个崩溃文件（崩溃产物在 ~/fuzzwork/artifacts/<目标>/）
& D:\WSL\Scripts\linux-task.ps1 -Mode build -Project 'D:\Forgejo\Rivu' -Command 'cargo +nightly fuzz run hostname ~/fuzzwork/artifacts/hostname/crash-xxxx; cargo +nightly fuzz tmin hostname ~/fuzzwork/artifacts/hostname/crash-xxxx'
```

注意：build 模式的同步会删掉镜像里不在 Windows 源码里的文件，所以 `cargo fuzz` 默认的 `fuzz/artifacts/` 与 `fuzz/corpus/` 里新增的文件会在下一次同步时消失；`run.sh` 把它们放到 `~/fuzzwork`，直接用 `cargo fuzz run` 时请自己加 `-artifact_prefix=` 并把工作语料目录放在镜像之外。

## CI

`.forgejo/workflows/ci.yml` 里有一个可选的 `fuzz-smoke` job（`continue-on-error: true`，不阻塞合并）：装 nightly 与 cargo-fuzz，每个目标跑 60 秒（`FUZZ_SECONDS`），崩溃输入作为 artifact 上传。它需要 runner 能装 nightly 工具链、有 C++ 编译器、能访问 crates.io，条件写在 job 上方的注释里；**尚未在真实的 Forgejo runner 上运行过**。60 秒只能抓刚引入的浅层崩溃，长跑仍然要在开发机上手工做。
