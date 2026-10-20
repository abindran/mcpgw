# M4 exit check

**Exit criteria (plan):** v1.0 release signed and published; Wasm and
OpenAPI paths each have a public example; upgrade from M3 builds tested.

## Wasm and OpenAPI public examples: met

| Example | What | Tested by |
|---|---|---|
| [`examples/wasm-text-tools`](../examples/wasm-text-tools) | a Wasm tool server built with the `mcpgw-tool` SDK: `count_words` and `slugify`, no grants | `crates/mcpgw/tests/examples.rs`: builds it for wasm32, `mcpgw add --wasm` (2 tools, pinned), calls both through the daemon, checks a tool error |
| [`examples/openapi-petstore.toml`](../examples/openapi-petstore.toml) | the public Swagger Petstore as tools: its own spec URL, `api_key` bound to the vault, a read-only profile | `examples.rs` checks the config. The spec and calls themselves are covered against the Petstore fixture (M4-05, M4-06) |

The [Wasm](../docs/guides/wasm.md) and [OpenAPI](../docs/guides/openapi.md)
guides walk through both, and `sdk/template` remains the starting point
for a new tool (M4-04).

## Upgrade from M3 builds tested: met

`crates/mcpgw/tests/fixtures/m3/` was written by the M3 binary (c8eee8c,
`mcpgw 0.1.0`). `scripts/make-m3-fixture.sh` records how:

- a vault with one secret (passphrase key);
- a stored stdio server bound to that secret, with its 7 tools pinned;
- a profile with a deny policy;
- the audit entries those commands wrote.

`crates/mcpgw/tests/upgrade.rs` runs v1.0 (1.0.0) on a copy:

- **Listed:** the server, the secret and the profile.
- **Audit:** `mcpgw audit verify` finds the chain intact.
- **Doctor:** `mcpgw doctor --json` reports no failures.
- **Pin:** after `mcpgw up` the server starts against its M3 pin, and is
  not quarantined.
- **Secret:** a call reads the injected secret and gets
  `[REDACTED:m3_secret]`.
- **Profile:** the profile's endpoint hides the denied tool.

The store schema is unchanged since M3, and the config only gained keys.
[docs/upgrading.md](../docs/upgrading.md) lists the behaviour changes and
migration notes.

## v1.0 release signed and published: prepared, waiting for the tag

Everything the release needs is in place.

- **Version.** The workspace version is 1.0.0, so `mcpgw --version` prints
  `mcpgw 1.0.0`.
- **The workflow.** `.github/workflows/release.yml`, on a `v*` tag:
  - **builds** the five CI targets (macOS arm64 and x86_64, Linux glibc and
    musl, Windows) with `--locked`, and packages each as a tarball or zip
    with the README, CHANGELOG and licences;
  - **SBOM:** CycloneDX (`mcpgw.cdx.json`);
  - **checksums** (`SHA256SUMS`, which also covers the published
    `install.sh`);
  - **keyless Sigstore signatures** for every file (`cosign sign-blob`,
    `*.sigstore.json` bundles);
  - **GitHub build attestations** (`actions/attest-build-provenance`);
  - **a Homebrew formula** (`scripts/homebrew-formula.sh`, `mcpgw.rb`);
  - **the GitHub release**, with notes taken from the CHANGELOG section;
  - **a container image** (`Dockerfile`, the musl binary on Alpine, run as
    a non-root user, serving stdio), pushed to GHCR and signed by digest.
- **Dry runs.** `workflow_dispatch` builds and packages everything, but
  signs and publishes nothing: keyless signing writes to the public Rekor
  log.
- **Installers:** `cargo install`, `scripts/install.sh` (checks the
  SHA-256 against `SHA256SUMS`) and the Homebrew formula. A Windows MSI
  isn't built: Windows gets the zip.
- **Licences:** `LICENSE-MIT` and `LICENSE-APACHE` (the crates are
  `MIT OR Apache-2.0`).

**Publishing is the maintainer's step.** Pushing the tag publishes the
release, its signatures and the image:

```sh
git tag -s v1.0.0 -m "mcpgw 1.0.0" && git push origin v1.0.0
```

Publishing the `mcpgw-tool` crate to crates.io is also left to the
maintainer (`cargo publish -p mcpgw-tool`; the dry run passes).

## Found on the way (fixed)

- **Health checks of 2026-07-28 servers** (M1-05). `ping` isn't part of
  2026-07-28, so every health check got -32601 and restarted the server,
  even under a running call. Found by M4-09's 10-minute run. Such servers
  are now probed with `server/discover`, and any answer counts as alive.
- **Two tests that passed for the wrong reason.** The hanging-ping test only
  passed because of that -32601. The SDK template test removed its
  directory before stopping its daemon, so every run left a daemon behind.
- **`unsafe-libyaml` is unmaintained** (cargo deny): YAML specs are now read
  with serde-saphyr.
- **JSON key order** depended on which crates a build included
  (cedar-policy enables `serde_json/preserve_order`). The workspace now
  enables it everywhere.
