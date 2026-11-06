FROM fedora:43 AS build

RUN dnf install -y cargo rust gcc gcc-c++ make ca-certificates \
    && dnf clean all

WORKDIR /build
COPY Cargo.toml Cargo.lock rust-toolchain.toml ./
COPY crates ./crates
RUN cargo build --release -p cocat-server --locked

FROM fedora:43

LABEL authors="jeandet"

ARG PORT=6570

ENV STORAGE_PATH=/storage \
    DB_PATH=/db \
    LOG_PATH=/log \
    PORT=$PORT \
    COCAT_PROXY_PREFIX="" \
    COCAT_TRUSTED_PROXIES=1

RUN dnf install -y ca-certificates \
    && useradd cocat \
    && mkdir -p $STORAGE_PATH $DB_PATH $LOG_PATH/cocat \
    && chown -R cocat $STORAGE_PATH $DB_PATH $LOG_PATH \
    && dnf clean all

COPY --from=build /build/target/release/cocat-server /usr/local/bin/cocat-server
COPY docker/entry_point.sh /home/cocat/entry_point.sh
COPY docker/create-user.sh /usr/local/bin/create-user
COPY docker/add-user-to-room.sh /usr/local/bin/add-user-to-room
COPY docker/remove-user-from-room.sh /usr/local/bin/remove-user-from-room
RUN chmod 0755 /home/cocat/entry_point.sh /usr/local/bin/create-user \
    /usr/local/bin/add-user-to-room /usr/local/bin/remove-user-from-room

USER cocat
WORKDIR /home/cocat

VOLUME $STORAGE_PATH
VOLUME $LOG_PATH
VOLUME $DB_PATH
EXPOSE $PORT/tcp

ENTRYPOINT ["/home/cocat/entry_point.sh"]
