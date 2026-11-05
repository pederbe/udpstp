# The published image contains the verified Linux release binaries.
# The context has binary/amd64/udpstp, binary/arm64/udpstp and server.toml.
# COPY needs no target execution or QEMU.
FROM gcr.io/distroless/cc-debian12:nonroot@sha256:9dac0a79194e45a7da0158a9c6da57b217585af0786db3845d1f0ec1a0dd182f
ARG TARGETARCH
COPY --chmod=755 binary/${TARGETARCH}/udpstp /usr/local/bin/udpstp
COPY binary/${TARGETARCH}/LICENSE-MIT binary/${TARGETARCH}/LICENSE-APACHE binary/${TARGETARCH}/THIRD-PARTY-LICENSES.md /usr/share/licenses/udpstp/
COPY server.toml /etc/udpstp/server.toml
EXPOSE 24601/udp
ENTRYPOINT ["/usr/local/bin/udpstp", "server", "--config", "/etc/udpstp/server.toml", "--key-file", "/run/secrets/keys.toml"]
