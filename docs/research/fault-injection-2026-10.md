# 单实例故障注入与备份恢复演练（2026-10-28）

P3 的验收（`docs/roadmap.md`）：持续写入时对 rivu 执行 `kill -9`、重启 ClickHouse，各 100 次。ClickHouse 重启不丢事件；rivu 崩溃丢失的事件不超过崩溃前写缓冲里的数量；`events` 与 `sessions` 没有重复。

## 做法

`tools/faults/run.sh`（WSL，dev 的 PostgreSQL / ClickHouse，release 构建）：

```bash
& D:\WSL\Scripts\linux-task.ps1 -Mode build -Project 'D:\Forgejo\Rivu' -Command 'bash tools/faults/run.sh'
```

- 一个专用站点；每轮 loadgen 以 300 事件/s 发送 20 s（约 6000 个事件，多访客、多 UA、含 engagement 与自定义事件），在 3–14 s 之间的随机时刻注入一次故障。
- **kill 轮**：读一次 `rivu_writer_queue_depth`，立即对 `rivu serve` 执行 `kill -9`，马上重启（没有关停快照，会话从 ClickHouse 预热）。loadgen 不停，重启期间的请求连接失败。
- **ch 轮**：单数轮 `docker restart`（优雅停止），双数轮 `docker kill` 后 `docker start`；rivu 不动，写入任务在 ClickHouse 恢复前退避重试。
- 每轮结束、写入排空之后对账：丢失 = 生成器收到的 202 接受数 − 这一轮新增的 `events` 行数。kill 轮要求 0 ≤ 丢失 ≤ kill 前的写缓冲行数 + 2 × 速率（正在写的一批，加上采样到 kill 之间的余量）；ch 轮要求丢失 = 0。每轮检查 `event_id` 没有重复。
- 全部结束后检查物化视图：`sessions` 的 `sum(pageviews)`、`sum(events)` 与 `events` 一致，也就是用同一个去重 token 重试写入时，会话表没有重复计数（`system-design.md` §4.7「去重前提」）。

## 结果：全部通过

| 故障 | 轮数 | 接受的事件 | 丢失 | 连接失败 / 503 | 重复 |
|---|---|---|---|---|---|
| ClickHouse 重启（50 次 restart + 50 次 kill/start） | 100 | 593,376 | **0** | 0 / 0 | 0 |
| rivu `kill -9` | 100 | 585,023 | 23,294（每轮 2–299，中位数 260，p90 289） | 1,320（平均每轮 13） | 0 |

- `events` 与 `sessions`：pageviews 783,037 / 783,037，events 790,517 / 790,517，会话 607,041。
- rivu 崩溃丢失的就是正在写的那一批：写缓冲每 1 s 刷一次，丢失上限约等于 1 s 的流量（这里 300 个），与设计的「至多一次，损失有上界」一致（`system-design.md` §4.7）。所有轮次都在上界内，没有一轮出现负数（负数意味着重复写入）。
- rivu 从 `kill -9` 到重新接收的停顿约 45 ms（300 事件/s 下平均 13 个请求连接失败）；前面有反向代理时，`lb_try_duration` / `proxy_next_upstream` 会让这些请求重试。
- ClickHouse 重启期间写入任务退避重试，事件留在写缓冲里，客户端照常收到 202，没有 503。

## 局限

- 开发机（8 核笔记本、Docker Desktop），速率 300 事件/s；更高的速率下 kill 的丢失按比例变大（约 1 s 的流量），ClickHouse 停机更久时写缓冲可能写满（容量 20 万行，满了回 503）。
- ClickHouse 是容器重启，宿主机的页缓存还在；整机断电时 ClickHouse 自己的持久性（未 fsync 的 part）不在本测试范围内。
- 会话被切开的影响没有逐项量化：kill 之后从 ClickHouse 预热只恢复每个会话的一个 key，跨午夜后的别名会丢失（`system-design.md` §4.6「失败语义」）。

## 备份与恢复演练

`tools/backup-drill/run.sh`：用生产 compose（`deploy/compose.yml`）起独立项目 `rivu-drill`，`rivu setup`、建站、发 500 个事件，`deploy/backup.sh`，然后 `docker compose down -v` 删掉全部卷（模拟整机丢失），`deploy/restore.sh` 恢复。

```bash
& D:\WSL\Scripts\linux-task.ps1 -Mode build -Project 'D:\Forgejo\Rivu' -Command 'bash tools/backup-drill/run.sh'
```

| 项 | 结果 |
|---|---|
| 备份 | 168 KB（`clickhouse.zip` 80 KB、`postgres.pgdump` 79 KB），两步都在 1 s 内 |
| 恢复（从空卷到 `/readyz` 就绪） | 11 s |
| 恢复前后 | 用户与站点数、`events`、`sessions` 的 pageviews、同一个查询（按页面的 visitors / visits / pageviews / bounce_rate）完全一致 |
| 盐 | 备份里没有盐的数据，恢复后 rivu 启动时重新生成（3 天） |
| 管理员登录 | 200 |

第一次演练发现 `restore.sh` 的一个问题：`docker compose cp` 拷进容器的备份文件保留宿主机的属主与权限（640），ClickHouse 进程读不到，`RESTORE` 报 `Permission denied`；改为拷入后 `chown` 给 clickhouse 用户、备份目录也以 clickhouse 用户创建之后通过。

数据量很小，11 s 只说明流程正确；真实部署的恢复时间主要取决于备份大小（ClickHouse `RESTORE` 与 `pg_restore` 的吞吐），需要在自己的数据上演练。