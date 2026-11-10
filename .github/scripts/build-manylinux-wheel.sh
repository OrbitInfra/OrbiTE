#!/usr/bin/env bash
set -euo pipefail
if [[ ${1:-} == --inside ]]; then
    minor=$2
    python="/opt/python/cp${minor//./}-cp${minor//./}/bin/python"
    dnf install -y rdma-core-devel clang-devel
    export LIBCLANG_PATH=/usr/lib64
    export CARGO_HOME=/out/cargo RUSTUP_HOME=/out/rustup CARGO_TARGET_DIR=/out/target
    curl --fail --silent --show-error --location https://sh.rustup.rs -o /tmp/rustup.sh
    sh /tmp/rustup.sh -y --profile minimal --default-toolchain "$RUSTUP_TOOLCHAIN"
    export PATH="$CARGO_HOME/bin:$PATH"
    "$python" -m pip install 'maturin==1.10.2' 'patchelf==0.17.2.4'
    "$python" -m maturin build --locked --release --interpreter "$python" --manylinux 2_28 \
        --features pyo3/extension-module,rdma,cuda,io-uring,gds,metrics --out /out
    chown -R "$HOST_UID:$HOST_GID" /out
    exit 0
fi
out=$1
minor=$2
mkdir -p "$out"
out=$(cd "$out" && pwd)
case "$(uname -m)" in
    x86_64) image=quay.io/pypa/manylinux_2_28_x86_64@sha256:9555ba6c479fa69a2aa80ad1dc57154a60d562b19df159361d9379a02beb1e36 ;;
    aarch64|arm64) image=quay.io/pypa/manylinux_2_28_aarch64@sha256:cb37f0d66c8a75abb93cf6085cad1f1427cd7208f0c0fc48359bb1eea9924f21 ;;
    *) echo 'unsupported wheel architecture' >&2; exit 1 ;;
esac
echo "Publication wheel: $image; CPython $minor; Rust $RUSTUP_TOOLCHAIN; rdma,cuda,io-uring,gds,metrics"
docker run --rm -v "$PWD:/io:ro" -v "$out:/out" -w /io \
    -e RUSTUP_TOOLCHAIN -e HOST_UID="$(id -u)" -e HOST_GID="$(id -g)" \
    "$image" bash .github/scripts/build-manylinux-wheel.sh --inside "$minor"
python scripts/check-python-artifacts.py "$out"/*.whl
python scripts/check-package-contents.py "$out"/*.whl
python scripts/check-no-native-link.py "$out"/*.whl
python -m pip install --force-reinstall "$out"/*.whl
python -I tests/python/test_installed.py -v
