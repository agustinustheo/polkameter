# syntax=docker/dockerfile:1
FROM rust:1.93.0-bookworm AS builder
WORKDIR /build
COPY . .
ENV CARGO_PROFILE_RELEASE_STRIP=symbols CARGO_PROFILE_RELEASE_DEBUG=0
RUN cargo +1.93.0 build --locked --release -p polkameter --bin polkameter

FROM debian:bookworm-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --create-home --uid 10001 polkameter
COPY --from=builder /build/target/release/polkameter /usr/local/bin/polkameter
USER 10001:10001
WORKDIR /home/polkameter
ENTRYPOINT ["polkameter"]
CMD ["--help"]
