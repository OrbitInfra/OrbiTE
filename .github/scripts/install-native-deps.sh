#!/usr/bin/env bash
set -euo pipefail

missing=()
for package in "$@"; do
    if ! dpkg -s "$package" 2>/dev/null | grep -qx 'Status: install ok installed'; then
        missing+=("$package")
    fi
done
if ((${#missing[@]} == 0)); then
    echo 'Native build prerequisites already installed; no apt request needed.'
    exit 0
fi
sources="${RUNNER_TEMP:?RUNNER_TEMP must name the CI scratch directory}/orbite-ubuntu.sources"
mkdir -p "$RUNNER_TEMP/orbite-apt-sourceparts"
case "$(dpkg --print-architecture)" in
    amd64) archive_uri=https://archive.ubuntu.com/ubuntu; security_uri=https://security.ubuntu.com/ubuntu ;;
    arm64) archive_uri=https://ports.ubuntu.com/ubuntu-ports; security_uri=$archive_uri ;;
    *) echo 'unsupported CI package architecture' >&2; exit 1 ;;
esac
cat > "$sources" <<SOURCES
Types: deb
URIs: $archive_uri
Suites: noble noble-updates
Components: main universe
Signed-By: /usr/share/keyrings/ubuntu-archive-keyring.gpg

Types: deb
URIs: $security_uri
Suites: noble-security
Components: main universe
Signed-By: /usr/share/keyrings/ubuntu-archive-keyring.gpg
SOURCES
for attempt in 1 2 3; do
    source_file=''
    mirror=runner-default
    if ((attempt == 2)); then source_file=$sources; mirror=$archive_uri; fi
    echo "Native dependencies attempt $attempt/3: mirror=$mirror packages=${missing[*]} (180 s bound)"
    # An expired install may leave dpkg half-configured. Repair it before the
    # next apt call, inside the same bound; never retry tests or extend the bound.
    if sudo timeout --kill-after=10s 180 bash -c '
        set -euo pipefail
        attempt=$1; source_file=$2; shift 2
        if ((attempt > 1)); then
            echo "Repair interrupted package configuration before attempt $attempt."
            DEBIAN_FRONTEND=noninteractive dpkg --configure -a
        fi
        options=(-o Acquire::Retries=0 -o Acquire::http::Timeout=30 -o Acquire::https::Timeout=30 -o APT::Update::Error-Mode=any)
        if [[ -n "$source_file" ]]; then
            options+=(-o "Dir::Etc::sourcelist=$source_file" -o "Dir::Etc::sourceparts=$(dirname "$source_file")/orbite-apt-sourceparts")
        fi
        apt-get "${options[@]}" update
        DEBIAN_FRONTEND=noninteractive apt-get "${options[@]}" install -y --no-install-recommends "$@"
    ' _ "$attempt" "$source_file" "${missing[@]}"; then
        echo "Native dependencies succeeded on attempt $attempt/3 ($mirror)."
        exit 0
    else
        code=$?
        echo "Native dependencies attempt $attempt/3 failed (status $code)." >&2
    fi
done
echo 'Native dependencies exhausted all 3 bounded attempts.' >&2
exit 1
