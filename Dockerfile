# The release image: the static (musl) binary on Alpine. Built by
# .github/workflows/release.yml from the release archive, pushed to GHCR
# and signed by digest.
FROM docker.io/library/alpine:3.20
RUN apk add --no-cache ca-certificates && adduser -D -u 10001 mcpgw
COPY mcpgw /usr/local/bin/mcpgw
USER mcpgw
ENV MCPGW_CONFIG=/config/config.toml
# mcpgw only listens on loopback, so the image serves one client on stdio:
#   docker run -i --rm -v ~/.config/mcpgw:/config ghcr.io/abindran/mcpgw:1.0.0
ENTRYPOINT ["mcpgw"]
CMD ["serve", "--stdio"]
