# M0-03 — Supply-chain gates

**Story:** X · **Estimate:** 1 day · **Depends on:** M0-02
**Done when:** CI fails on a known-vulnerable or disallowed-licence crate.
**Status:** ✅ Done

## What runs (`supply-chain` CI job)

| Step | Tool | Fails on |
|------|------|----------|
| `cargo deny --locked check` | cargo-deny | RustSec advisories (vulnerable, unmaintained, unsound, yanked), licences outside the allowlist, wildcard deps, unknown registries or git sources |
| `cargo audit --deny warnings` | cargo-audit | any RustSec advisory or warning against `Cargo.lock` |
| `scripts/supply-chain-selftest.sh` | cargo-deny | the gate *not* rejecting the fixtures below |
| `cargo cyclonedx --format json --all` | cargo-cyclonedx | — produces a CycloneDX SBOM per crate, uploaded as the `sbom-cyclonedx` artifact |

## Policy (`deny.toml`)

- **Licence allowlist:** MIT, Apache-2.0 (incl. LLVM exception), BSD-2/3-Clause,
  ISC, Zlib, Unicode-3.0, CDLA-Permissive-2.0, MPL-2.0. Copyleft licences
  (GPL, AGPL, LGPL, …) are rejected.
- **Advisories:** every advisory fails unless added to `ignore` with a reason.
- **Sources:** crates.io only.
- **Duplicates:** warned, not denied.
- Internal path dependencies now carry `version = "0.1.0"` so they are not
  treated as wildcards (and can be published).

## Proof the gate bites

`testdata/supply-chain/` holds two standalone fixture projects with committed
lockfiles:

- `vulnerable/` depends on `smallvec = "=1.6.0"` (RUSTSEC-2021-0003).
- `gpl/` depends on a local `GPL-3.0-only` crate.

The self-test requires `cargo deny check advisories` to fail on the first with
`error[vulnerability]` and `cargo deny check licenses` to fail on the second
with `error[rejected]`. Checking for the specific diagnostic matters: an
earlier draft "passed" because cargo-deny rejected a misplaced flag.

## Also in this commit

The MSRV moves from the plan's 1.88 to **1.98**, the toolchain installed on
the development machine. The CI MSRV job had failed because `uuid` 1.28 needs
Rust 1.89.
