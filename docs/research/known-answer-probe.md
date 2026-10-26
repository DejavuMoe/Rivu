# 已知答案探针

验收项：`docs/brief-p1.md`「已知答案探针（定时制造已知访问的无头浏览器）连续 7 天全部通过」。
工具：`apps/tracker/probe/probe.mjs`（探针）、`summary.mjs`（汇总与判定）、`run-local.sh`（本机演练）。

**状态（2026-10-27）**：探针与判定已实现，本机对临时实例跑了 30 轮全部通过（下文）。**连续 7 天的正式运行还没有开始**：它需要一个长期运行的实例（线上或常驻的测试机），按下面「正式运行」部署后开始计时。

## 每一轮做什么

1. 新开一个无头 Chromium 上下文。UA 每轮不同（版本号里带轮次），所以每轮是一个新访客、一个新会话；不用无头 Chromium 自带的 UA（含 `HeadlessChrome`，会被判为机器人）。
2. 打开本机测试页 `/p/<rid>/a`。脚本从被测的 rivu 加载：`GET <base>/js/<site_key>.js`，即站点的真实配置前缀加 `full.js`。
3. 依次执行已知动作：
   - SPA `pushState` 到 `/p/<rid>/b`；
   - 点链接整页跳转到 `/p/<rid>/c`；
   - 关闭页面，flush engagement。
4. 用公开查询 API（`POST /api/v1/query`，只要 `stats:read` 的 API key），按本轮独有的路径前缀 `/p/<rid>/` 过滤，轮询到数字出现为止，然后与已知答案逐项比较：

   | 指标 | 答案 |
   |---|---|
   | `pageviews` | 3 |
   | `visits` | 1 |
   | `visitors` | 1 |
   | `bounce_rate` | 0 |
   | `events` | 3（`events` 不含 engagement） |

5. 3 秒后再查一次，数字不能再变（没有重复计数）。

每轮写一行 JSON：`at`、`rid`、`ok`、`got`、`visible_ms`（从关闭页面到数字完全出现）、`requests`（浏览器发出的采集请求数，含 engagement）、`error`。

覆盖的链路：
- 脚本分发（站点前缀与 full 产物）；
- 浏览器端：排除逻辑、首个 pageview、SPA 路由、整页跳转、engagement；
- 线协议；
- 管线：hostname 白名单、机器人判定、身份、会话；
- 写缓冲 → ClickHouse；
- 查询引擎与公开 API（鉴权、过滤、指标口径）。

本身不覆盖的部分：
- 地理与来源：测试页在 127.0.0.1，直接打开；
- Firefox / WebKit：由 `test/e2e-rivu` 覆盖。

## 判定

`summary.mjs` 判定连续 7 天全部通过，需要同时满足：

- 最后一次失败之后，连续通过的记录覆盖 ≥ 7 天；
- 这段时间里相邻两轮的间隔不超过 `--max-gap`（默认 1200 秒，即 10 分钟间隔的 2 倍）。中途停跑会让连续计时归零。

它还报告通过率、可见延迟分位数和最近的失败。

## 本机演练（2026-10-27）

`ROUNDS=30 INTERVAL=10 bash apps/tracker/probe/run-local.sh`：

- 环境：开发机 WSL2、`rivu all`（debug 构建，写缓冲默认 1 秒刷新）、dev 的 PostgreSQL 18 / ClickHouse 25.8。
- 结果：30 轮全部通过。
- 可见延迟：p50 607 ms，p95 1216 ms，max 1217 ms。延迟主要来自写缓冲的 1 秒刷新间隔，与压测报告一致。

更早的 4 轮试跑同样全部通过，p50 892 ms。

## 正式运行（7 天）

前提：
- 一个长期运行的 rivu 实例；
- 一个专门的探针站点，hostname 白名单含 `127.0.0.1`，时区随意；
- 一个只读 API key：`rivu apikey create --org <org> --user <user> --name probe --scopes stats:read`。
- 探针机上有 Node 24 与 `apps/tracker` 的依赖：`pnpm install`、`pnpm exec playwright install chromium`。

```bash
cd apps/tracker
RIVU_PROBE_API_KEY=… nohup node probe/probe.mjs --base https://stats.example.com --site <site_key> \
  --out /var/lib/rivu-probe/probe.jsonl --interval 600 --rounds 0 >/var/log/rivu-probe.log 2>&1 &
# 每天看一次
node probe/summary.mjs /var/lib/rivu-probe/probe.jsonl --days 7
```

运行注意：

- 也可以用 systemd 服务或 cron（`--rounds 1`，每 10 分钟一次），JSON 行追加到同一个文件即可。
- 探针站点的数据会出现在该站点的统计里：用专门的站点，不要用业务站点。
- 失败时 `got` 记录查到的数字，`error` 记录原因。原因可能是：
  - 浏览器没发出请求；
  - 60 秒内没等到答案；
  - 数字随后又变了。
- 失败后要查清并修复，再重新累计 7 天。
