# M3-04 — File and tmpfs secret injection for containers

**Story:** US2 · **Estimate:** 1 day · **Depends on:** M3-01
**Done when:** a service-account JSON is mounted in tmpfs and gone after stop.
**Status:** ✅ Done (verified with Docker 27.4; Linux CI runs the test)

## Using it

```toml
[[server]]
name = "bigquery"
transport = "container"
image = "example/bigquery-mcp:1"
files = { "/run/secrets/sa.json" = "gcp_service_account" }      # path → vault secret
env = { GOOGLE_APPLICATION_CREDENTIALS = "/run/secrets/sa.json" }
```

```sh
mcpgw secret set gcp_service_account < sa.json
mcpgw add bigquery --image example/bigquery-mcp:1 \
  --secret-file /run/secrets/sa.json=gcp_service_account \
  --env GOOGLE_APPLICATION_CREDENTIALS=/run/secrets/sa.json
```

Paths must be absolute file paths: no `..`, commas or `=`, and nothing
under `/proc`, `/sys` or `/dev`. Secret names may use `{account}` like other
bindings. `files` is for container servers only.

## How it works

The file has to be in tmpfs on the **container host**. That is the Linux
machine itself, or the VM that runs the containers under Docker Desktop,
colima or a Podman machine. The host's tmpfs is its `/dev/shm`. On each
start:

1. A helper container (the same pinned `nginx:1.27-alpine` image used for
   egress, `--network=none`, 32 MiB) mounts the host's `/dev/shm`. It
   writes each value **from stdin** to
   `/dev/shm/mcpgw-secrets/<instance>/<container>/f<n>`, with directories
   0700 and files 0444. The value is never on a command line or in an
   environment.
2. The server container gets `--mount
   type=bind,source=…/f<n>,target=<path>,readonly`. Inside, the file is a
   read-only tmpfs mount, readable by the strict profile's `nobody` user.
3. When the server stops (idle, eviction, shutdown, failed start) the
   container is removed, then the egress proxy and network, then the secret
   directory. The startup sweep also removes the directories of containers
   that no longer exist.

What the server reads back is redacted like any injected secret.
`docker inspect` shows only the bind source path, never the contents.

## Tests

`crates/mcpgw-runtime/tests/files.rs` uses a service-account JSON, built at
runtime so the canary scan never sees a literal, under the strict profile:

- **Done-when:** the server runs as uid 65534 and reads the file at
  `GOOGLE_APPLICATION_CREDENTIALS`. Its contents come back as
  `[REDACTED:gcp_sa]`, which proves they match. `/proc/mounts` shows a
  `tmpfs` mount with `ro`, and writing gives `EROFS`. `docker inspect` has
  no key material. The file exists on the host while the server runs, and
  after the idle stop both the container and the file are gone. A fresh
  start writes a fresh file, which shutdown removes again.
- `crates/mcpgw/tests/container.rs`: `mcpgw add … --secret-file
  /run/secrets/cfg.json=cfg_json`, and a call through the daemon reads it
  back redacted.
- Unit tests: path validation, `files` counted in `secret_refs` (so a missing
  secret fails the start with a hint), and refusing `files` on non-container
  servers. `ContainerTemplate`'s `Debug` shows env and file names only.
