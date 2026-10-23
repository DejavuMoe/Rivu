#!/usr/bin/env bash
# 用法：fuzz/run.sh <target> [秒数=600] [jobs=4]
# 在仓库根（或 WSL 构建镜像根）运行：工作语料与崩溃产物放在 $HOME/fuzzwork（不在镜像里，不会被同步删除），
# 入库的种子语料 fuzz/corpus/<target>/ 只读。日志写到 $HOME/fuzzwork/logs/<target>.log。
set -euo pipefail
target="$1"; secs="${2:-600}"; jobs="${3:-4}"
work="$HOME/fuzzwork"
mkdir -p "$work/corpus/$target" "$work/artifacts/$target" "$work/logs"
dict=()
[[ -f "fuzz/dicts/$target.dict" ]] && dict=("-dict=fuzz/dicts/$target.dict")
cargo +nightly fuzz run "$target" "$work/corpus/$target" "fuzz/corpus/$target" -- \
  -max_total_time="$secs" -jobs="$jobs" -workers="$jobs" -max_len=16384 \
  -artifact_prefix="$work/artifacts/$target/" "${dict[@]}" 2>&1 | tee "$work/logs/$target.log" | tail -5