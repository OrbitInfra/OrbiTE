#!/usr/bin/env bash
set -euo pipefail
binary=$1
out=$2
mkdir -p "$out"
filters=(transport::tcp:: runtime::control_plane::tests::tcp_reuse:: tcp_failed_retirement_keeps_actual peer_retirement_tcp_reply_drain partial_read_preserves_destination a_trickling_response_cannot_extend)
count=$("$binary" "${filters[@]}" --list | awk '/: test$/ {n++} END {print n+0}')
test "$count" -eq 14
for i in $(seq 1 50); do
  RUST_TEST_THREADS=$(nproc) "$binary" "${filters[@]}" > "$out/$i.log" 2>&1 || { cat "$out/$i.log"; exit 1; }
  grep -F "test result: ok. $count passed; 0 failed;" "$out/$i.log"
done
