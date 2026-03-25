FROM rust:slim-bookworm AS builder
WORKDIR /app
RUN apt-get update && apt-get install -y pkg-config libssl-dev && rm -rf /var/lib/apt/lists/*
COPY . .
RUN cargo build --release

FROM gcr.io/distroless/cc-debian12:nonroot
WORKDIR /app
COPY --from=builder --chown=nonroot:nonroot /app/target/release/rustjack /app/rustjack
USER 65532:65532
EXPOSE 8443
ENTRYPOINT ["/app/rustjack"]