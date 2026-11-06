#!/bin/bash
# Deploys the Rust dev server on the sciqlop host, as the sciqlop user, the way
# update-cocat-server deploys the Python one. Install as /usr/bin/update-cocat-rs-dev.
set -e
if [[ $EUID -ne 1001 ]]; then
    echo "This script must be run as sciqlop user"
    exit 1
fi
export XDG_RUNTIME_DIR=/run/user/"$(id -u)"

IMAGE=registry.lpp.polytechnique.fr/sciqlop/cocat-server-rs:devel
NAME=cocat-rs-dev
DATA=/DATA/cocat_rs_dev
# The whole mail configuration (COCAT_SMTP_*, COCAT_MAIL_FROM), password included, lives in
# this file, readable by sciqlop only; without it mails are written as files to $DATA/mail.
MAIL_ENV=~/.config/cocat-rs-dev/mail.env
if [[ -f $MAIL_ENV ]]; then
    MAIL=(--env-file "$MAIL_ENV")
else
    MAIL=(-e COCAT_MAIL_DIR=/mail -e COCAT_MAIL_FROM=cocat-dev@sciqlop.lpp.polytechnique.fr)
fi

podman pull "$IMAGE"
podman rm --ignore "${NAME}_old"
if podman container exists "$NAME"; then
    podman stop "$NAME"
    podman rename "$NAME" "${NAME}_old"
fi
# systemd owns restarts (Restart=on-failure in the generated unit), as for cocat-server.
podman run --ulimit nofile=262144:262144 --network=host -d \
    -e PORT=6575 \
    -e COCAT_PROXY_PREFIX=cocat-dev \
    -e COCAT_PUBLIC_URL=https://sciqlop.lpp.polytechnique.fr \
    -e COCAT_METRICS_BIND=127.0.0.1:9187 \
    "${MAIL[@]}" \
    -v "$DATA/storage:/storage:Z" -v "$DATA/db:/db:Z" -v "$DATA/mail:/mail:Z" \
    --stop-timeout 30 \
    --name "$NAME" "$IMAGE"
podman generate systemd --name "$NAME" > ~/.config/systemd/user/"$NAME".service
systemctl --user daemon-reload
# --now: the unit adopts the container that runs now, so a crash is restarted.
systemctl --user enable --now "$NAME"
