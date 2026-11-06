#!/usr/bin/env bash
set -e
BASEDIR=$(dirname "$0")
IMAGE=129.104.6.172:32219/sciqlop/cocat-server-rs

BUILD_TYPE=${1:-"testing"}
if [ "$BUILD_TYPE" = "release" ]; then
    TAG=stable
elif [ "$BUILD_TYPE" = "testing" ]; then
    TAG=devel
else
    echo "usage: $0 [testing|release]" >&2
    exit 1
fi

if [ -z "${container}" ]; then
    REMOTE=""
else
    REMOTE="--remote"
fi

podman $REMOTE build -t "$IMAGE:$TAG" -f docker/Dockerfile "$BASEDIR/.."
podman $REMOTE push "$IMAGE:$TAG"
