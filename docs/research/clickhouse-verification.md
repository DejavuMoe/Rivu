# ClickHouse behaviour verification for Rivu (target 25.8 LTS .. 26.x)

Method: official docs pages (clickhouse.com/docs), plus the authoritative setting declarations and
change history in `src/Core/Settings.cpp` and `src/Storages/MergeTree/MergeTreeSettings.cpp` on
ClickHouse master (about 26.8/26.10 at time of writing; per-setting history entries give the version of each default change),
plus docs.rs for the Rust crate (clickhouse 0.15.3). Items marked (unverified) come from my own knowledge or inference
and were not confirmed against a primary source; check them with `toTypeName()`, `EXPLAIN`, or a test insert.

## Biggest version-dependent traps (read first)

| Setting | <= 25.x default | >= 26.2 default | Source |
|---|---|---|---|
| `async_insert` | 0 | **1** (history entry "26.2, false -> true") | Settings.cpp |
| `deduplicate_insert` (new) | n/a (`insert_deduplicate=1`, `async_insert_deduplicate=0`) | `enable` (covers sync + async; `insert_deduplicate=0` no longer disables it alone) | Settings.cpp |
| `deduplicate_blocks_in_dependent_materialized_views` | 0 | **1** | Settings.cpp |
| `max_insert_threads` | 1 | auto in 26.8 (parallel INSERT SELECT) | Settings.cpp |
| `optimize_aggregation_in_order_limit` | n/a | true since 26.7 (had wrong-result bug, issue #116849, fix PR #117573) | GitHub |

Consequence: Rivu must set `async_insert=0` explicitly on its own batched INSERTs (or deliberately choose async),
and must set the dedup settings explicitly instead of relying on defaults. `compatibility` setting can restore old defaults, but explicit settings always win.

## Verdict table

| # | Verdict | One-line |
|---|---|---|
| 1 | CORRECT | Needs window > 0; token replaces data hash. Caveats on block splitting. |
| 2 | PARTIALLY CORRECT | "Source skipped => MV not re-triggered" was true only for the old default (setting = 0). Since 26.2 default is 1: retries re-feed the MV, which dedups by its own log. |
| 3 | CORRECT | Synchronous in the INSERT pipeline; visible on return (sync insert, or async with wait=1). |
| 4 | CORRECT | Type name for -If is right; plain-state alternative exists (see below, partly unverified). |
| 5 | CORRECT | Uniformity is a practical requirement, not enforced. |
| 6 | PARTIALLY CORRECT | The example GROUP BY is only a partial in-order match (prefix = site_id, start_date). |
| 7 | CORRECT | Table-level MergeTree setting, values throw/drop/rebuild; rebuild has an open bug. |
| 8 | CORRECT (with caveat) | Works, but monthly partitions mean retention is N to N+~31 days, not N. |
| 9 | CORRECT (with caveats) | Efficient; check auto cross-partition merge conditions and PREWHERE semantics. |
| 10 | CORRECT | Array(String) and Identifier work; not usable "anywhere". |
| 11 | PARTIALLY CORRECT | Both hyperscan limits default to 0 = unlimited, so they protect nothing until set. |
| 12 | CORRECT | Client-side batching still recommended; async default flipped to ON in 26.2. |
| 13 | PARTIALLY CORRECT | Settings per insert: yes (`with_setting`). Server-side params: yes (`.param()`). Client-side `bind` is the other path. Details below. |
| 14 | PARTIALLY CORRECT | `uniq` determinism not stated in its docs; `uniqCombined64` is HLL only for large sets (array/hash table first). |
| 15 | CORRECT / guidance | JSON is production-ready since 25.3; Map guidance below. |

## Detail per claim

### 1. Non-replicated dedup and `insert_deduplication_token` - CORRECT
- `non_replicated_deduplication_window` (MergeTree table setting) default is `0` = disabled; positive N keeps hash sums of the last N blocks in a local file (MergeTreeSettings.cpp). Replicated tables default `replicated_deduplication_window` = 10000 on master.
- Token: "ClickHouse doesn't use the hash sum of the data when the token is provided"; a retry with the same token but different data "is recognized as a duplicate and is dropped" (dedup guide). Token used only when non-empty.
- Caveats: (a) dedup is per block, not per statement. A batch above `max_insert_block_size` (about 1M rows) or spanning several partitions becomes several blocks; keep each batch one block, single partition if possible (unverified detail: per-block token suffixes are derived from the token). (b) Window is a count of recent blocks: size it above the max blocks possible in your retry horizon. (c) 26.2+: `deduplicate_insert=enable` must not be `disable`.
- Sources: https://clickhouse.com/docs/guides/developer/deduplicating-inserts-on-retries ; https://github.com/ClickHouse/ClickHouse/blob/master/src/Storages/MergeTree/MergeTreeSettings.cpp

### 2. MV and dedup - PARTIALLY CORRECT
- Current docs: MV block id = hash of source `block_id` + MV identifiers; retrying the same source insert is deduplicated "on both the source and MV target tables". Different source inserts that yield identical MV output are NOT merged.
- `deduplicate_blocks_in_dependent_materialized_views`: default was `false`, became `true` in 26.2 (Settings.cpp history entry `{"26.2", false, true, "Enable deduplication for dependent materialized views by default."}`). Works together with `deduplicate_insert` ("both switches must allow it").
- Old (default 0) semantics, from older docs text (recalled, unverified in current docs): if the source block is dropped as a duplicate, the MV is not fed; this breaks idempotency when source succeeded and MV failed, because the retry never reaches the MV. With the setting at 1, the retry reaches the MV, which runs its own dedup check and inserts only what was lost. So the claim holds only for the legacy default, and the legacy behaviour is the dangerous one. On 25.8 the legacy default was observed to fail differently, by double counting in the MV (E1).
- Async inserts + MV: "one block in, one block out"; a view emitting several blocks throws NOT_IMPLEMENTED unless the setting is 0 or the insert is sync.
- Docs warn that if retries all fail "the materialized views could be out of sync", and past the window "the same data can be inserted multiple times".
- Design for "retry must not double-count in MV-fed AggregatingMergeTree":
  1. Source table and MV target both need a dedup log: `non_replicated_deduplication_window` > 0 on both (docs examples set 1000 on both; the docs do not state the target requirement explicitly).
  2. Per-batch stable `insert_deduplication_token` (persist the batch id before sending; reuse on retry).
  3. Set `deduplicate_insert=enable` and `deduplicate_blocks_in_dependent_materialized_views=1` explicitly; one block per INSERT; `parallel_view_processing=0` (default).
  4. Window larger than max in-flight and replay horizon; AggregatingMergeTree states are not idempotent, so a missed dedup means permanent double counting.
  5. If a stronger guarantee is needed: keep raw events in a ReplacingMergeTree/dedup-keyed table and rebuild aggregates from it, or reconcile periodically.
- Sources: https://clickhouse.com/docs/guides/developer/deduplicating-inserts-on-retries ; Settings.cpp (`deduplicate_blocks_in_dependent_materialized_views`, `deduplicate_insert`)

### 3. MV synchronous - CORRECT
- MV is "a trigger that runs a query on blocks as they are inserted"; with three 1-second views one INSERT took about 3 s, sequential in uuid order (`parallel_view_processing` default false). Docs never use the word "synchronous" or state a visibility guarantee explicitly, but their examples read the target right after INSERT.
- Async inserts: MV work happens at flush; with `wait_for_async_insert=1` (default) the ack comes after flush.
- Caveats: multi-replica reads need quorum or `select_sequential_consistency`; MV failure semantics are not fully specified in docs ("newer versions isolate failures per block"). New setting `wait_for_part_commit_in_dependent_materialized_views` (default false) only matters for cascades that JOIN back to the source.
- Source: https://clickhouse.com/docs/materialized-view/incremental-materialized-view

### 4. AggregatingMergeTree column types - CORRECT
- `SimpleAggregateFunction` supports any, anyLast, min, max, sum, sumWithOverflow, groupBit*, groupArrayArray, sumMap/minMap/maxMap etc.; stores plain values; no -State/-Merge needed (docs). Docs do not say GROUP BY is needed, but merges are background and eventual, so unmerged parts coexist: always `GROUP BY sorting key` with the plain aggregate (or FINAL) at query time. Note `any` keeps an arbitrary value, so use `argMin/argMax` state for first/last semantics.
- `argMaxState(x, ts)` into `AggregateFunction(argMax, String, DateTime64(3))`, read with `argMaxMerge`: standard.
- -If: `argMinIfState(x, ts, cond)` column type `AggregateFunction(argMinIf, String, DateTime64(3), UInt8)` read with `argMinIfMerge` matches published examples (`sumIfState` -> `AggregateFunction(sumIf, Int64, Int8)`; Altinity, OneUptime). Docs' combinator page gives no concrete type name, so verify with `SELECT toTypeName(argMinIfState(...))`.
- Plain alternative (unverified): `argMinStateIf(x, ts, cond)` (If applied after State) should produce a plain `AggregateFunction(argMin, ...)` state, mergeable with `argMinMerge` and with other argMin states. A MV `WHERE` also works if every column of the MV shares the condition.
- Sources: https://clickhouse.com/docs/sql-reference/data-types/simpleaggregatefunction ; https://clickhouse.com/docs/sql-reference/aggregate-functions/combinators

### 5. SAMPLE BY - CORRECT
- "If specified, it must be contained in the primary key. The sampling expression must result in an unsigned integer." Example `SAMPLE BY intHash32(UserID)` with `ORDER BY (CounterID, EventDate, intHash32(UserID))`. Uniform distribution is not enforced; it is what makes samples representative.
- `_sample_factor` virtual column "created automatically when you create a table with the specified sampling key"; use it to scale `SAMPLE n` results. For `SAMPLE k` ratios aggregates are not auto-scaled. Same SELECT with SAMPLE is deterministic.
- Sources: https://clickhouse.com/docs/engines/table-engines/mergetree-family/mergetree ; https://clickhouse.com/docs/sql-reference/statements/select/sample

### 6. optimize_aggregation_in_order - PARTIALLY CORRECT
- Setting default false. Docs: applies when GROUP BY "contains at least prefix of sorting key or injective functions"; reduces memory, can slow queries.
- Internals (PR #120259): `AggregatingInOrderTransform` streams on the matched sorting-key prefix and hash-aggregates remaining GROUP BY keys inside each run of equal prefix values. If no prefix matches it falls back to normal hash aggregation.
- ORDER BY (site_id, start_date, visitor_id, session_id), GROUP BY (site_id, start_date, session_id): matched prefix is (site_id, start_date) only, because visitor_id is skipped. So it is partial in-order: memory is bounded by the number of distinct session_id inside one (site_id, start_date) run, not constant per group. GROUP BY (site_id, start_date, visitor_id, session_id) would be fully streaming.
- Fully streaming lists: any GROUP BY whose columns are exactly a leading prefix of the sorting key: (site_id), (site_id,start_date), (site_id,start_date,visitor_id), full key; plus injective functions of them. Order of listing in GROUP BY should not matter (unverified; check `EXPLAIN PIPELINE` for AggregatingInOrderTransform). Lists skipping the first key column (e.g. GROUP BY start_date) get no benefit.
- Risks: bugs fixed in #112200 (DESC key) and #117573 (LIMIT pushdown, `optimize_aggregation_in_order_limit` true since 26.7); test on your exact version.
- Sources: https://clickhouse.com/docs/sql-reference/statements/select/group-by ; https://github.com/ClickHouse/ClickHouse/pull/120259 ; https://github.com/ClickHouse/ClickHouse/issues/116849

### 7. Lightweight DELETE and projections - CORRECT
- "By default, DELETE does not work for tables with projections." `lightweight_mutation_projection_mode` is a MergeTree table-level setting (added 24.7, PR #65594): `throw` (default), `drop` (drop affected projection parts, queries fall back to base part), `rebuild` (rebuild affected projection parts). The session-level copy is obsolete (MAKE_OBSOLETE in Settings.cpp).
- Open bug: in `rebuild` mode a normal projection can keep stale deleted rows (issue #111791, seen on 26.8.1; fix PR #111809, merge status not confirmed). Prefer `drop`, or avoid projections on tables needing deletes.
- `ALTER TABLE ... DELETE` rewrites affected parts ("heavyweight"). `apply_deleted_mask` default true.
- Sources: https://clickhouse.com/docs/sql-reference/statements/delete ; https://github.com/ClickHouse/ClickHouse/issues/111791

### 8. TTL + ttl_only_drop_parts - CORRECT (caveat)
- `ttl_only_drop_parts` default false; when true the whole part is dropped only if all rows in it have expired by row TTL. TTL is evaluated at merges, not at insert; expired data triggers off-schedule merges (`merge_with_ttl_timeout`); SELECT between merges can return expired rows.
- With `PARTITION BY toYYYYMM(ts)`, parts never cross months, so whole partitions vanish, but only after the newest row in the month expires, so effective retention is N to about N+31 days. If exact cut-off matters, use a scheduled `ALTER TABLE ... DROP PARTITION` or per-day partitions.
- Sources: https://clickhouse.com/docs/engines/table-engines/mergetree-family/mergetree ; MergeTreeSettings.cpp

### 9. ReplacingMergeTree + FINAL - CORRECT (caveats)
- Docs: FINAL runs in parallel, slightly slower, reads PK columns extra, `max_final_threads` limits threads; cheaper than OPTIMIZE. Primary-key range pruning still applies to the (site_id, day) filter.
- Verified defaults: `do_not_merge_across_partitions_select_final=false`, but `enable_automatic_decision_for_merging_across_partitions_for_final=true` auto-enables the optimization when the partition expression is deterministic and all its columns are in the primary key (floats excluded). So with `PARTITION BY toYYYYMM(ts)` it only applies if `ts` (or what the expression needs) is in the sorting key; otherwise set the first setting manually, which is safe only if one dedup key never spans partitions. `split_parts_ranges_into_intersecting_and_non_intersecting_final` default true (since 24.1/24.2): non-overlapping parts skip the merge path.
- Semantic trap: WHERE/PREWHERE on non-key columns that can differ between versions is applied before FINAL picks the winner; `apply_prewhere_after_final` (default false) changes PREWHERE only.
- Source: https://clickhouse.com/docs/sql-reference/statements/select/from ; https://clickhouse.com/docs/engines/table-engines/mergetree-family/replacingmergetree

### 10. Query parameters - CORRECT
- `{name:Type}` with `param_name=` in the HTTP URL (or `SET param_x`, `--param_x`). Identifiers need type `Identifier`. Arrays/Maps/nested types work (`{d: Map(String, Array(UInt8))}` in docs; Array(String) uses the same literal syntax).
- Docs: not general text substitution, "primarily designed to work in SELECT statements in place of identifiers or literals". Docs contain no explicit injection-safety statement; the server parses the value as the declared type, which is why it is the recommended approach.
- Source: https://clickhouse.com/docs/sql-reference/syntax

### 11. Regex settings - PARTIALLY CORRECT
- `allow_hyperscan` default true (disable to avoid long compile/resource use). `max_hyperscan_regexp_length` default **0 (unlimited)**; `max_hyperscan_regexp_total_length` default **0 (unlimited)**. They apply to the hyperscan multi-match functions only. For untrusted patterns set non-zero limits (or disable hyperscan), and cap pattern count.
- `match` uses RE2 (docs: "uses the RE2 regular expression library"); `.` matches newlines by default unlike RE2's default. RE2 guarantees linear-time matching with no backtracking (general RE2 property, not stated in ClickHouse docs); no lookaround/backreferences. `multiMatchAny` uses Hyperscan/Vectorscan (automata, also no backtracking, but compile time/memory can blow up).
- Sources: Settings.cpp ; https://clickhouse.com/docs/sql-reference/functions/string-search-functions

### 12. Async inserts - CORRECT
- `wait_for_async_insert` default true: ack after data "successfully flushed to disk"; docs strongly recommend `async_insert=1,wait_for_async_insert=1`; 0 = fire-and-forget with data-loss risk.
- Defaults (Settings.cpp): `async_insert_use_adaptive_busy_timeout=true` (since 24.2), `async_insert_busy_timeout_min_ms=50`, `async_insert_busy_timeout_max_ms=200` (`async_insert_busy_timeout_ms` is its alias; Cloud 1000), `async_insert_max_query_number=450`, `async_insert_max_data_size=10485760` (10 MiB; docs page says 100 MiB, which is the Cloud default).
- 26.2: `async_insert` default became true, and async inserts now deduplicate (per query token, shared dedup log with sync inserts).
- Recommendation: "Use synchronous inserts if you can batch the data client side"; about one insert/s; at least 1,000 rows, ideally 10,000-100,000. Async is the fallback for many small writers. Rivu's single writer with 1-20k row batches should keep client batching and send `async_insert=0`.
- Sources: https://clickhouse.com/docs/best-practices/selecting-an-insert-strategy ; https://clickhouse.com/docs/optimize/asynchronous-inserts

### 13. Rust `clickhouse` crate (0.15.3) - PARTIALLY CORRECT
- HTTP only; `RowBinaryWithNamesAndTypes` (validation on by default) or `RowBinary` with `with_validation(false)` (1.1x-3x faster, vague errors). LZ4 (feature `lz4`, default) and ZSTD supported; `Compression::Lz4` default.
- `Inserter` (feature `inserter`): `with_max_rows`, `with_max_bytes`, `with_period` (checked only inside `commit()`), `with_period_bias`. Thresholds are soft.
- Per-insert settings: yes. `Client::with_setting`, `Insert`/`Inserter::with_setting` (scoped to generated INSERTs). `with_option` is deprecated since 0.14.3. So `insert_deduplication_token` can be passed, but the Inserter ends an INSERT automatically on limits, so a token fixed on the inserter would be reused across batches (wrongly deduplicating distinct batches). For retry-safe tokens use `client.insert::<T>(table).with_setting("insert_deduplication_token", id)` per batch, or Inserter with limits unset and `force_commit()` + a fresh setting per batch (not documented; setting change applies only at the next INSERT).
- Parameters: both exist. `Query::bind` replaces `?` with client-side escaped values; `Query::param(name, value)` sends server-side `param_<name>` for `{name:Type}`; `query_raw` skips `?` parsing. Use `.param()` for untrusted values.
- Sources: https://docs.rs/clickhouse/latest/clickhouse/struct.Client.html ; https://docs.rs/clickhouse/latest/clickhouse/inserter/struct.Inserter.html ; https://docs.rs/clickhouse/latest/clickhouse/query/struct.Query.html ; https://github.com/ClickHouse/clickhouse-rs

### 14. uniq family - PARTIALLY CORRECT
- `uniq`: "adaptive sampling algorithm", sample of element hashes up to 65536; extra 64-bit sample for very large cardinalities. The `uniq` page does not state determinism (in practice it is deterministic for the same data, but not documented). It is recommended "in almost all scenarios".
- `uniqCombined64`: combination of array, hash table, then HyperLogLog (error-correction table); precision 12-20, default 17 (about 96 KiB); "result deterministically (does not depend on query processing order)"; 64-bit hash for all types. Claim "HLL-based" is true only for large sets.
- `uniqExact` is exact (listed in See Also; not described on those pages).
- Sources: https://clickhouse.com/docs/sql-reference/aggregate-functions/reference/uniq ; https://clickhouse.com/docs/sql-reference/aggregate-functions/reference/uniqcombined64

### 15. Map vs parallel arrays vs JSON - guidance
- Map is stored as one `Array(Tuple(K,V))` stream by default; `m['k']` scans the whole column (linear in map size); `m.keys` / `m.values` subcolumns avoid reading the other half; `mapContains(m,k)` also requires reading keys. `LowCardinality(String)` keys are allowed (only Nullable-nested forms are not).
- New (docs mention 26.8 key-order fix): `map_serialization_version='with_buckets'` buckets keys by hash so `m['k']` reads one bucket: 1.6-3.2x faster at 10 keys, 21-49x at 10,000 keys; full scans about 2x slower, inserts 1.5-2.5x slower; `map_serialization_version_for_zero_level_parts` can keep new parts `basic`. Docs: keep `basic` for small maps, buckets for 100+ keys.
- Parallel `Array(String)` columns have the same scan cost as Map for lookup; a Map gives clearer semantics and `mapContains`. Neither is indexed (consider bloom_filter skip index on `mapKeys(m)` / `mapValues(m)`; unverified here). Promote hot properties to real columns.
- JSON type: "marked as production ready in version 25.3" (not recommended earlier); columns for typed paths, `max_dynamic_paths` default 1024, `max_dynamic_types` default 32; wins when keys vary and few paths are queried; slower inserts and whole-document reads, extra storage. For known/bounded custom-event properties, Map (or fixed columns plus a small Map) is simpler; use JSON with typed-path hints if schema is truly open and property filtering is frequent. Rust client reads JSON as String (24.10+).
- Sources: https://clickhouse.com/docs/sql-reference/data-types/map ; https://clickhouse.com/docs/sql-reference/data-types/newjson

## P0 empirical checks (ClickHouse 25.8.33.6, Rust crate 0.15.3)

Observed against `deploy/compose.dev.yml` while building the P0 scaffold. The regression test for E1, E3 and E4 is `bin/rivu/tests/clickhouse_sessions.rs`; the other rows were checked by hand.

| # | Observation | Consequence for Rivu |
|---|---|---|
| E1 | With `deduplicate_blocks_in_dependent_materialized_views=0`, replaying a batch with the same `insert_deduplication_token` was dropped by `events` but written again into `sessions` (pageviews doubled). With `=1` neither table changed. | On 25.8 the legacy default fails by double counting in the MV, not by "the retry never reaches the MV" as recalled under claim 2. Either way the setting must be 1. |
| E2 | `deduplicate_insert` does not exist on 25.8, and an unknown setting is rejected. | Send it only to 26.2+ servers; the test probes `system.settings` first. |
| E3 | A materialized view created without a `SQL SECURITY` clause stores none, because the server setting `ignore_empty_sql_security_in_create_view_query` is 1 by default. Pushes into such a view are checked against the inserting user, so `rivu_writer` needed `SELECT` on `events`. | Migrations write `DEFINER = CURRENT_USER SQL SECURITY DEFINER`. The view then runs as `rivu_migrator`, which must not be dropped, and `rivu_writer` keeps `INSERT` only. |
| E4 | `DESCRIBE TABLE`, which the crate runs to validate inserts, works with `INSERT` alone because a column-level grant implies `SHOW COLUMNS`. | Validated inserts need no `SELECT` grant. |
| E5 | `client.insert("rivu.events")` escapes the whole string as one identifier, i.e. a table named `` `rivu.events` `` in the default database. | Bind the database with `with_database()` and pass the bare table name, or use `insert_unescaped()`. |
| E6 | With validation on, the row struct must contain every column that has no DEFAULT. | Schema changes to `events` need a matching change to the row type. |
| E7 | There is no separate `ALTER DROP PARTITION` privilege; `DROP PARTITION` is authorised by `ALTER DELETE`. | `rivu_maintainer` gets `ALTER DELETE` and `SELECT`. |
| E8 | In a `readonly=1` profile, `<changeable_in_readonly/>` on `log_comment` lets `rivu_reader` tag its queries (seen in `system.query_log`), while `INSERT` is rejected. | The `CHANGEABLE_IN_READONLY` plan in platform-and-ops §8 works on 25.8. Lowering `max_execution_time` per query is not tested yet. |
