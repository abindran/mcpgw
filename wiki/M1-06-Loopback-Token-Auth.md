# M1-06 — Static loopback-token auth

**Crate:** `mcpgw-core` (`src/auth.rs`) · **Status:** ✅ Done

## Design
- `Token` is 32 random bytes from the OS RNG (`getrandom`), hex-encoded to 64 characters.
- It is stored at `[gateway] token_path` (default `~/.local/share/mcpgw/token`). The file is created with `create_new` and mode **0600**.
- `Token::load` **refuses** a token file that group or other users can access (`chmod 600` fixes it), and rejects malformed contents.
- `verify` compares in constant time. `verify_bearer` checks an `Authorization: Bearer <token>` value, with a case-insensitive scheme.
- `rotate` writes a new token to a temp file and renames it over the old one atomically.
- `Debug` prints `Token(***)`, so the secret never ends up in logs.

## Where it applies
| Front end | Auth |
|-----------|------|
| stdio (M1) | None needed: only the parent process holds the pipe. The token is still created on `serve` so it is ready for clients. |
| Streamable HTTP on loopback (M2) | Every request needs `Authorization: Bearer <token>`. |

The CLI (M1-07) creates the token on first `serve` and offers `mcpgw token [--rotate]` to print or rotate it.

## Tests
3 unit tests: generation and uniqueness, verify and bearer parsing with the redacted `Debug`; create, reload and rotate with the 0600 mode check; malformed and insecure files rejected.
