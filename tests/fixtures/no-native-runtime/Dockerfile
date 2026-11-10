FROM ubuntu:24.04
# No rdma-core or NVIDIA runtime packages. C headers alone let the lib-suite's
# fake-provider fixtures compile; they cannot satisfy a dlopen.
RUN apt-get update -o Acquire::Retries=3 && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends gcc libc6-dev python3 python3-venv binutils && rm -rf /var/lib/apt/lists/*
COPY headers/ /usr/include/
RUN python3 -m venv /opt/python
ENV PATH="/opt/python/bin:$PATH"
