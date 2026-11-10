#!/usr/bin/env bash
set -euo pipefail
out=$1
mkdir -p "$out/crate"
version=$(python3 -c 'import tomllib; print(tomllib.load(open("Cargo.toml", "rb"))["workspace"]["package"]["version"])')
target=$(cargo metadata --locked --format-version 1 --no-deps | python3 -c 'import json,sys; print(json.load(sys.stdin)["target_directory"])')
cargo package --locked --features io-uring
cargo package --locked --list > "$out/package-list.txt"
python3 scripts/check-package-contents.py --cargo-list "$out/package-list.txt" "$target/package/orbite-$version.crate"
cp "$target/package/orbite-$version.crate" "$out/"
tar -xf "$out/orbite-$version.crate" -C "$out/crate"
cargo install --locked --offline --path "$out/crate/orbite-$version" --features io-uring \
  --example orbite-quickstart --root "$out/installed" --target-dir "$out/target"
# Execute away from the checkout; unavailable io_uring is an error, never a skip.
(cd "$out" && ORBITE_REQUIRE_IO_URING=1 timeout 120 ./installed/bin/orbite-quickstart)
