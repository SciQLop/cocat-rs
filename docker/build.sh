#!/usr/bin/env bash
# Builds the cocat-server image from this checkout, and pushes it with --push.
# usage: docker/build.sh <image>[:tag] [--push]
set -euo pipefail

usage() {
    echo "usage: $0 <image>[:tag] [--push]" >&2
    exit 1
}

[[ $# -ge 1 && $# -le 2 ]] || usage
IMAGE=$1
PUSH=${2:-}
[[ -z $PUSH || $PUSH == --push ]] || usage
# docker push without a tag would not mean :latest everywhere, so name it.
[[ ${IMAGE##*/} == *:* ]] || IMAGE+=:latest

if command -v podman >/dev/null; then
    ENGINE=(podman)
    # Inside a toolbox or distrobox container, build with the host's podman service.
    [[ -z ${container:-} ]] || ENGINE+=(--remote)
elif command -v docker >/dev/null; then
    ENGINE=(docker)
else
    echo "neither podman nor docker is installed" >&2
    exit 1
fi

ROOT=$(cd "$(dirname "$0")/.." && pwd)
"${ENGINE[@]}" build -t "$IMAGE" -f "$ROOT/docker/Dockerfile" "$ROOT"
if [[ -n $PUSH ]]; then
    "${ENGINE[@]}" push "$IMAGE"
fi
