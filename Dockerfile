# imbo-server image. Built in CI from prebuilt binaries:
#   dist/server/linux-amd64/imbo-server, dist/server/linux-arm64/imbo-server
# Without DATABASE_URL it runs its bundled PostgreSQL under /data.
FROM debian:trixie-slim
ARG TARGETARCH
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates \
 && rm -rf /var/lib/apt/lists/* \
 && useradd --system --uid 10001 --home-dir /data --shell /usr/sbin/nologin imbo \
 && mkdir -p /data && chown imbo:imbo /data
COPY dist/server/linux-${TARGETARCH}/imbo-server /usr/local/bin/imbo-server
USER imbo
WORKDIR /data
ENV IMBO_DATA_DIR=/data
VOLUME /data
EXPOSE 8080 443
ENTRYPOINT ["/usr/local/bin/imbo-server"]
