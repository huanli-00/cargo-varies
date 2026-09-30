#!/usr/bin/env bash
set -euo pipefail

if [[ "${IN_CARGO_VARIES_CONTAINER:-0}" == "1" ]]; then
  exec "$@"
fi

IMAGE="${IMAGE:-cargo-varies-experiments:latest}"
HOST_CPUS="$(nproc)"
DOCKER_CPUS="${DOCKER_CPUS:-$(( HOST_CPUS / 2 ))}"
if [[ "$DOCKER_CPUS" -lt 1 ]]; then
  DOCKER_CPUS=1
fi
RESOURCE_ARGS=()
if [[ -n "${DOCKER_MEMORY:-}" ]]; then
  RESOURCE_ARGS+=(--memory "$DOCKER_MEMORY")
fi
if [[ -n "${DOCKER_MEMORY_SWAP:-}" ]]; then
  RESOURCE_ARGS+=(--memory-swap "$DOCKER_MEMORY_SWAP")
fi
REPO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
WORKDIR="${CONTAINER_WORKDIR:-$REPO_DIR}"
POPULAR_CRATES_HOST_DIR="${POPULAR_CRATES_HOST_DIR:-}"
: "${VARIES_KANI_HOST_DIR:?Set VARIES_KANI_HOST_DIR to a built varies-kani checkout}"
RUSTUP_HOST_DIR="${RUSTUP_HOST_DIR:-${RUSTUP_HOME:-$(rustup show home)}}"
CARGO_HOST_DIR="${CARGO_HOST_DIR:-${CARGO_HOME:-$(dirname "$(dirname "$(command -v cargo)")")}}"
CBMC_HOST_BIN="${CBMC_HOST_BIN:-$(command -v cbmc)}"
GOTO_CC_HOST_BIN="${GOTO_CC_HOST_BIN:-$(command -v goto-cc)}"
GOTO_INSTRUMENT_HOST_BIN="${GOTO_INSTRUMENT_HOST_BIN:-$(command -v goto-instrument)}"
CONTAINER_HOME="${CONTAINER_HOME:-$REPO_DIR/tmp/experiment-container-home}"

mkdir -p "$CONTAINER_HOME/.cache" "$CONTAINER_HOME/.local/share"

POPULAR_CRATES_MOUNT=()
if [[ -d "$POPULAR_CRATES_HOST_DIR" ]]; then
  POPULAR_CRATES_MOUNT=(-v "$POPULAR_CRATES_HOST_DIR:$POPULAR_CRATES_HOST_DIR")
fi

docker run --rm --init \
  --cpus "$DOCKER_CPUS" \
  "${RESOURCE_ARGS[@]}" \
  --shm-size "${DOCKER_SHM_SIZE:-4g}" \
  --user "$(id -u):$(id -g)" \
  -e IN_CARGO_VARIES_CONTAINER=1 \
  -e HOME="$CONTAINER_HOME" \
  -e XDG_CACHE_HOME="$CONTAINER_HOME/.cache" \
  -e XDG_DATA_HOME="$CONTAINER_HOME/.local/share" \
  -e RUSTUP_HOME="$RUSTUP_HOST_DIR" \
  -e CARGO_HOME="$CARGO_HOST_DIR" \
  -e PATH="/opt/varies-kani/scripts:$CARGO_HOST_DIR/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin" \
  -e VARIES_KANI_DIR=/opt/varies-kani \
  -e RUST_BACKTRACE="${RUST_BACKTRACE:-1}" \
  -v "$VARIES_KANI_HOST_DIR:/opt/varies-kani" \
  -v "$RUSTUP_HOST_DIR:$RUSTUP_HOST_DIR" \
  -v "$CARGO_HOST_DIR:$CARGO_HOST_DIR" \
  -v "$CBMC_HOST_BIN:/usr/local/bin/cbmc:ro" \
  -v "$GOTO_CC_HOST_BIN:/usr/local/bin/goto-cc:ro" \
  -v "$GOTO_INSTRUMENT_HOST_BIN:/usr/local/bin/goto-instrument:ro" \
  -v "$REPO_DIR:$REPO_DIR" \
  "${POPULAR_CRATES_MOUNT[@]}" \
  -w "$WORKDIR" \
  "$IMAGE" \
  "$@"
