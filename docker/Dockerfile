FROM fedora:43 AS build

RUN dnf install -y cargo rust rust-std-static-wasm32-unknown-unknown gcc gcc-c++ make \
        ca-certificates \
    && dnf clean all

# trunk fetches the wasm-bindgen that Cargo.lock pins, and wasm-opt, on its first build.
RUN cargo install trunk --version 0.22.1 --locked --root /usr/local

WORKDIR /build
COPY Cargo.toml Cargo.lock rust-toolchain.toml ./
COPY crates ./crates
RUN cargo build --release -p cocat-server --locked
RUN cd crates/cocat-web && trunk build --release --locked

FROM fedora:43

LABEL authors="jeandet"

ARG PORT=6570

ENV STORAGE_PATH=/storage \
    DB_PATH=/db \
    LOG_PATH=/log \
    PORT=$PORT \
    COCAT_PROXY_PREFIX="" \
    COCAT_TRUSTED_PROXIES=1 \
    COCAT_WEB_DIR=/usr/share/cocat/web

RUN dnf install -y ca-certificates \
    && useradd cocat \
    && mkdir -p $STORAGE_PATH $DB_PATH $LOG_PATH/cocat \
    && chown -R cocat $STORAGE_PATH $DB_PATH $LOG_PATH \
    && dnf clean all

COPY --from=build /build/target/release/cocat-server /usr/local/bin/cocat-server
COPY --from=build /build/crates/cocat-web/dist /usr/share/cocat/web
COPY docker/entry_point.sh /home/cocat/entry_point.sh
COPY docker/create-user.sh /usr/local/bin/create-user
COPY docker/set-superuser.sh /usr/local/bin/set-superuser
COPY docker/set-curator.sh /usr/local/bin/set-curator
COPY docker/add-user-to-room.sh /usr/local/bin/add-user-to-room
COPY docker/remove-user-from-room.sh /usr/local/bin/remove-user-from-room
RUN chmod 0755 /home/cocat/entry_point.sh /usr/local/bin/create-user \
    /usr/local/bin/set-superuser /usr/local/bin/set-curator /usr/local/bin/add-user-to-room \
    /usr/local/bin/remove-user-from-room

USER cocat
WORKDIR /home/cocat

VOLUME $STORAGE_PATH
VOLUME $LOG_PATH
VOLUME $DB_PATH
EXPOSE $PORT/tcp

ENTRYPOINT ["/home/cocat/entry_point.sh"]
