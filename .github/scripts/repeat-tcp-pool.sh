#!/usr/bin/env bash
set -euo pipefail
binary=$1
out=$2
mkdir -p "$out"
filters=(transport::tcp:: runtime::control_plane::tests::tcp_reuse:: tcp_failed_retirement_keeps_actual shuffled_owners_keep_the_public_failure_prefix dropping_async_handle_retains_engine_allocator peer_retirement_tcp_reply_drain partial_read_preserves_destination a_trickling_response_cannot_extend tcp_faults_keep_a_successful_sibling)
count=$("$binary" "${filters[@]}" --list | awk '/: test$/ {n++} END {print n+0}')
expected=16
if [[ "${ORBITE_FEATURES:-}" == *rdma* && "${ORBITE_FEATURES:-}" == *cuda* ]]; then expected=17; fi
test "$count" -eq "$expected"
for i in $(seq 1 50); do
  RUST_TEST_THREADS=$(nproc) "$binary" "${filters[@]}" > "$out/$i.log" 2>&1 || { cat "$out/$i.log"; exit 1; }
  grep -F "test result: ok. $count passed; 0 failed;" "$out/$i.log"
done
