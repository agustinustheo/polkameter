# syntax=docker/dockerfile:1
# The build context holds the Linux release binaries in one directory per architecture:
# amd64/ and arm64/, each with polkameter and polkameter-substrate-plugin. They link
# against glibc 2.35 or older; bookworm has 2.36.
FROM debian:bookworm-slim@sha256:734a72b3d6142a71993b4bc580ca01d8ca687f77a2c422d96a961d3376b179bc
ARG TARGETARCH
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates tini \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --create-home --uid 10001 polkameter
COPY ${TARGETARCH}/polkameter ${TARGETARCH}/polkameter-substrate-plugin /usr/local/bin/
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
