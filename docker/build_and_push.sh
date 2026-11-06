#!/usr/bin/env bash
set -e
BASEDIR=$(dirname "$0")
# Pushes go to port 5443 and need a login (podman login registry.lpp.polytechnique.fr:5443);
# the host pulls anonymously from registry.lpp.polytechnique.fr.
IMAGE=registry.lpp.polytechnique.fr:5443/sciqlop/cocat-server-rs

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
