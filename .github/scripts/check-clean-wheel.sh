#!/usr/bin/env bash
set -euo pipefail
out=$(cd "$1" && pwd)
minor=$2
# Only the already-built publication artifacts and tests enter this runtime;
# unlike the builder, it has no rdma-core or NVIDIA libraries or development SDKs.
docker run --rm --security-opt seccomp=unconfined --shm-size=512m \
  -v "$PWD:/io:ro" -v "$out:/wheel:ro" -w /tmp \
  -e ORBITE_REQUIRE_IO_URING=1 "python:$minor-slim-bookworm" \
  sh -ec 'python -m pip install --no-index /wheel/*.whl
          python -I /io/tests/python/test_native_runtime.py
          python -I /io/tests/python/test_installed.py -v'
