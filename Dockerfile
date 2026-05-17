FROM rust:1.82-slim-bookworm AS builder

WORKDIR /app
COPY Cargo.toml Cargo.lock* ./
COPY groundtruth-validator/ groundtruth-validator/
COPY server/ server/

RUN cargo build --release -p groundtruth

FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /app/target/release/groundtruth /usr/local/bin/groundtruth

ENV RUST_LOG=groundtruth=info
ENV MQTT_BROKER_HOST=mosquitto
ENV MQTT_BROKER_PORT=1883
ENV DB_PATH=/data/groundtruth.db

VOLUME ["/data"]

EXPOSE 3001

CMD ["groundtruth"]
