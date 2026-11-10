#!/usr/bin/env bash
set -euo pipefail
out=$1
features=rdma,cuda,io-uring,gds,metrics,rdma-adaptive
mkdir -p "$out"
cargo test --locked --lib --features "$features" --no-run --message-format=json > "$out/build.json"
binary=$(jq -ers 'map(select(.reason == "compiler-artifact" and .target.kind == ["lib"] and .profile.test and .executable != null)) | last.executable' "$out/build.json")
cargo rustc --locked --lib --features "$features" --crate-type cdylib
cargo build --locked --features "$features" --example cpu_trace
python3 scripts/check-no-native-link.py target/debug/liborbite.so target/debug/examples/cpu_trace "$binary"
# Prove the ELF guard rejects the previous link shape, rather than only observing
# that this build happens to pass it.
printf 'extern void *ibv_get_device_list(int *); void *probe(int *n) { return ibv_get_device_list(n); }\n' > "$out/linked.c"
cc -shared -fPIC "$out/linked.c" -libverbs -o "$out/linked.so"
if python3 scripts/check-no-native-link.py "$out/linked.so" > "$out/negative.log" 2>&1; then
  echo 'ELF guard accepted a linked libibverbs dependency' >&2
  exit 1
fi
grep -F 'native runtime link' "$out/negative.log"
python3 -m maturin build --locked --release --interpreter "$(command -v python3)" \
  --features "pyo3/extension-module,$features" --out "$out/wheel"
python3 scripts/check-python-artifacts.py "$out/wheel"/*.whl
python3 scripts/check-no-native-link.py "$out/wheel"/*.whl
mkdir -p "$out/image/headers"
cp -a /usr/include/infiniband /usr/include/rdma "$out/image/headers/"
cp tests/fixtures/no-native-runtime/Dockerfile "$out/image/"
docker build -t orbite-no-native-runtime "$out/image"
# The real-file controls need ext4 and an unrestricted io_uring syscall. This
# changes neither installed userspace libraries nor the absence checks.
ext="$out/ext4"
mkdir -p "$ext"
if test "$(findmnt -n -o FSTYPE -T "$ext")" != ext4; then
  truncate -s 128M "$out/ext4.img"
  mkfs.ext4 -q -F "$out/ext4.img"
  sudo mount -o loop "$out/ext4.img" "$ext"
  trap 'sudo umount "$ext"' EXIT
fi
if test -e /proc/sys/kernel/io_uring_disabled; then sudo sysctl -w kernel.io_uring_disabled=0; fi
docker run --rm --security-opt seccomp=unconfined --shm-size=512m \
  -v "$PWD:$PWD:ro" -v "$out:$out:ro" -v "$ext:/ext4" -w "$PWD" \
  -e ORBITE_REQUIRE_IO_URING=1 -e ORBITE_REQUIRE_NO_NATIVE_LIBRARIES=1 \
  -e ORBITE_EXT4_DIR=/ext4 -e ORBITE_TMPFS_DIR=/dev/shm \
  -e RUST_TEST_THREADS="$(nproc)" orbite-no-native-runtime \
  bash -euc '
    python -m pip install --no-index "$1"/wheel/*.whl
    python -I tests/python/test_native_runtime.py
    python -I tests/python/test_installed.py -v
    "$2"
  ' _ "$out" "$binary" | tee "$out/clean-runtime.log"
