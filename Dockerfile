# syntax=docker/dockerfile:1
FROM rust:1.93.0-bookworm@sha256:d0a4aa3ca2e1088ac0c81690914a0d810f2eee188197034edf366ed010a2b382 AS builder
WORKDIR /build
COPY . .
ENV CARGO_PROFILE_RELEASE_STRIP=symbols CARGO_PROFILE_RELEASE_DEBUG=0
RUN cargo +1.93.0 build --locked --release -p polkameter -p polkameter-substrate-plugin --bin polkameter --bin polkameter-substrate-plugin

FROM debian:bookworm-slim@sha256:734a72b3d6142a71993b4bc580ca01d8ca687f77a2c422d96a961d3376b179bc
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates tini \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --create-home --uid 10001 polkameter
COPY --from=builder /build/target/release/polkameter /usr/local/bin/polkameter
COPY --from=builder /build/target/release/polkameter-substrate-plugin /usr/local/bin/polkameter-substrate-plugin
USER 10001:10001
WORKDIR /home/polkameter
# tini runs as PID 1: it forwards signals to polkameter and reaps exited plugin processes.
# STOPSIGNAL makes docker stop send SIGINT, the signal polkameter treats as Ctrl-C: it
# stops the run, runs teardown and writes the results. Teardown can outlast docker's
# default 10-second stop timeout, so stop with `docker stop --timeout 45` or run with
# `--stop-timeout 45`. Runtimes that ignore STOPSIGNAL, such as Kubernetes, send SIGTERM.
STOPSIGNAL SIGINT
ENTRYPOINT ["/usr/bin/tini", "--", "polkameter"]
CMD ["--help"]
