# 边缘日志对账（2026-10，开发机）

日期：2026-10-27。验收项：`docs/brief-p1.md`「边缘日志里 `/api/event` 的请求数与 `ingest_counters` 严格对账」。
工具：`tools/reconcile`（对账程序）与 `tools/reconcile/run.sh`（编排）；边缘是 Caddy，配置与 `deploy/Caddyfile` 相同的日志过滤和重试参数。
原始记录：`docs/research/edge-reconciliation-2026-10/`（`run.log`、`Caddyfile`、`loadgen.txt`、`reconcile.txt`、`reconcile.json`）。

**先说清楚：这是开发机上的一次演练，不是线上站点的对账。** 流量来自 `tools/loadgen` 和构造的请求，不是真实访客；brief 要的「自有站点的边缘访问日志」需要把同一套工具接到线上站点的 Caddy 日志上再跑一次（见「接到线上」）。

## 结论

| 判定 | 边缘（Caddy 访问日志） | `ingest_counters` | 结果 |
|---|---:|---:|---|
| 202 且没有 `x-rivu-dropped` = `accepted` | 17,803 | 17,803 | 一致 |
| 202 且有 `x-rivu-dropped` = 全部 `dropped_*`（不含 `overloaded`） | 238 | 238 | 一致 |
| 503 = `dropped_overloaded` | 0 | 0 | 一致 |
| 合计 | 18,041 | 18,041 | **严格一致** |
| `events` 行数 = `accepted` | | 17,803 | 一致 |

窗口里中途重启了一次 rivu（SIGTERM → 写会话快照、排空写缓冲 → 重新启动，约 1 秒），Caddy 的 `lb_try_duration 5s` 让这段时间的请求排队重试：边缘没有任何 502，loadgen 没有连接错误，只是这批请求的延迟（从计划发送时刻起）p99.9 到了 415 ms、最大 474 ms，其余时间 p99 为 3.9 ms。

不进 `ingest_counters` 的边缘请求单独列出：400（坏 JSON）40 个、413（请求体超过 8 KB）2 个，与构造的数目一致；它们只进 Prometheus 的 `rivu_ingest_rejected_total`，这是设计如此（`system-design.md` §10 Q13）。

逐分钟比较有两个分钟各差 1 个 accepted（一个分钟多 1、相邻分钟少 1）：边缘按 Caddy 收到请求的时刻分钟，计数按 rivu 收到的时刻分钟，恰好跨分钟边界的请求会落到相邻分钟。所以严格对账以窗口合计为准，逐分钟只做参考。

## 方法

拓扑：`loadgen` 与 `curl` → Caddy v2.11.7（本机 http，`trusted_proxies static 127.0.0.1/32`，保留 loadgen 带来的 `X-Forwarded-For`）→ `rivu all`（release）→ ClickHouse 25.8 / PostgreSQL 18（Docker Desktop）。

流量（60 秒）：

- `loadgen` 固定到达率 300 事件/s，3 个站点，事件分布同 `docs/research/load-test-2026-10.md`（pageview、engagement、4% 自定义事件；1% 垃圾引荐开场的会话、UA 池里少量被判为机器人的 UA）；
- 构造的请求：未知站点 40 个（202 + `x-rivu-dropped`，记在 `site_id = 0`）、坏 JSON 40 个（400）、超大请求体 2 个（413）；
- 第 30 秒重启 rivu。

判定规则（`tools/reconcile/src/main.rs` 文件头）：只看 `POST /api/event`；202 按响应头 `x-rivu-dropped` 分成接受与丢弃，503 对 `dropped_overloaded`；窗口是日志里第一个与最后一个请求所在的分钟；站点限定为本次的站点加上 `site_id = 0`，所以对账期间同一个 ClickHouse 库里不能有别的采集流量。访问日志按 `deploy/Caddyfile` 删除客户端 IP 与全部请求头，保留响应头（`x-rivu-dropped` 就在里面）。

丢弃原因分布（`ingest_counters`）：`dropped_no_session` 67、`dropped_spam_referrer` 75、`dropped_bot` 56、`dropped_unknown_site` 40。

## 演练发现并修复的缺陷：未来时间戳的会话清掉整片的活跃会话

第一次带重启的演练对账同样严格一致，但 `dropped_no_session` 是 798，不重启的对照组只有 119。按访客查 `events`：544 个访客在重启前后被切成两个会话，上一个会话的最后事件就在重启前几秒、新会话在重启后，分片号相同；别处没有任何被切开的会话。重启时写下的会话快照里这些会话都在，key、分片、时间都对。

原因在会话清扫：清扫的门限是「分片内事件时间水位 − 30 分钟」，水位是分片里见过的最大事件时间。开发库里有测试写入的、时间戳在未来的事件，启动预热（从 ClickHouse 或快照）把这些会话放回去，所在分片的水位就被推到未来，第一次清扫把这些分片里所有真实的活跃会话都判成过期删掉（启动时放回 38,580 个，半秒后只剩 16,217 个）。线上同样会发生：时钟回拨（NTP 校时）、多节点时钟不一致，都会产生「未来」的会话。

修复（与本报告同一批提交）：清扫的水位封顶到墙钟（`min(水位, now) − 30 min`，积压重放时水位落后于墙钟，不受影响）；预热时最后活跃时间超过「现在 + 1 分钟」的会话不放回（本次丢弃 64 个）。修复后重跑：`dropped_no_session` 67，被切开的会话 0。回归测试：`crates/session` 的 `a_future_session_does_not_sweep_live_sessions_when_capped_by_wall_clock`、`many_sessions_survive_export_and_restore`，`crates/ingest` 的 `sessions_continue_after_export_and_restore_into_a_new_pipeline`。

## 局限

- 开发机、单次运行、合成流量；边缘与 rivu 在同一台机器的回环上，没有真实网络与 CDN。
- 窗口内 ClickHouse 里不能有别的采集流量（未知站点都记在 `site_id = 0`）；线上对账要么用独立实例，要么加 `--exclude-unknown-site` 只比对本站的站点（此时边缘日志里也不能有未知站点的请求，例如按站点的域名切分日志）。
- 对账以分钟窗口合计为准：窗口两端的分钟如果还有窗口外的请求，会造成差异；演练里日志的第一个与最后一个请求就是全部流量的两端，线上取窗口时应留出前后各一分钟的空闲，或者按完整的日志切片对账。
- Caddy v2.10.2 用 Go 1.27 编译后，只要配置了访问日志就会在启动时 panic（`interface is nil, not caddy.WriterOpener`），演练改用 v2.11.7；生产镜像选择 Caddy 版本时要注意这一点。

## 接到线上

1. 线上 Caddy 按 `deploy/Caddyfile` 配置访问日志（`RIVU_ACCESS_LOG` 指向文件，或从容器 stdout 收集为 JSON 行）。
2. 取一段完整的日志切片（例如一个小时），用 reader 账号对账：
   `RECONCILE_CH_PASSWORD=… reconcile --caddy-log access.log --sites <站点 id，逗号分隔> --ch-url … --ch-user rivu_reader --out result.json`
3. 不一致时看 `result.json` 的 `minute_diffs` 定位分钟，再结合 rivu 日志与 `rivu_ingest_events_total`。

## 复现

```powershell
# 前提：dev 的 PostgreSQL / ClickHouse 在运行；WSL 里有 Caddy：go install github.com/caddyserver/caddy/v2/cmd/caddy@v2.11.7
& D:\WSL\Scripts\linux-task.ps1 -Mode build -Project 'D:\Forgejo\Rivu' -Command 'bash tools/reconcile/run.sh'
# 不重启的对照组
& D:\WSL\Scripts\linux-task.ps1 -Mode build -Project 'D:\Forgejo\Rivu' -Command 'RESTART_AT=0 bash tools/reconcile/run.sh'
```

可调的环境变量：`SITES`、`RATE`、`SECS`、`RESTART_AT`、`CADDY`、`RECONCILE_OUT`。
