# M2-12 — Minimal web UI

**Story:** US3 · **Estimate:** 4 days · **Depends on:** M2-11
**Done when:** enable, disable and approve work from the browser.
**Status:** ✅ Done (verified by clicking through it in a browser)

## Use

```sh
mcpgw ui            # opens http://127.0.0.1:7777/ui#token=… (starts the daemon if needed)
mcpgw ui --print    # just print the address
```

One page, refreshed every 2 s:

- **Header**: version · N of M servers running · K waiting for approval
- **Waiting for approval**: each pending call with tool, endpoint, client,
  id and arguments, plus **Approve** / **Deny**
- **Servers**: name, state badge, pinned tool count, source, last error, and
  **Enable/Disable** and **Restart**. Config-file servers have the toggle
  disabled, with a tooltip saying to change them in the file.
- **Recent calls**: time, tool, endpoint, outcome (`ok`, `tool error`,
  `error`), ms

Light and dark follow the OS. No external resources.

## Security

- The three files (`/ui`, `/ui/app.js`, `/ui/app.css`) contain no secrets
  and are served **without** the token, but still only to loopback `Host`s.
- The token travels in the **URL fragment**, which browsers never send to
  the server. The page moves it into `sessionStorage` for that tab and strips
  it from the address bar; every API call sends it as `Authorization:
  Bearer`. Without it the page asks you to use `mcpgw ui`.
- Headers: `Content-Security-Policy: default-src 'none'; script-src 'self';
  style-src 'self'; connect-src 'self'; … frame-ancestors 'none'`,
  `X-Frame-Options: DENY`, `Referrer-Policy: no-referrer`, `nosniff`,
  `no-store`. No inline script or style, which a unit test checks. Clicks are
  same-origin `fetch`es, which pass the edge's `Origin` check.

## New API and fixes

- `GET /api/v1/calls?limit=`: recent tool calls from a 200-entry
  `CallLog` the router fills: names, endpoint, outcome, duration and client,
  **never arguments or results**. The approval question round of MRTR isn't
  counted. It's in the OpenAPI document and the API test.
- Fixed while testing in the browser: `BackendManager::set_enabled` didn't
  update the manager's copy of the spec, so the API reported `enabled: true`
  for a disabled server and the button never flipped. Also, the `disabled`
  reason is no longer shown as a red error.

## Verification

- **In a browser** (built-in browser pane, demo daemon with a store server
  `fx`, a config server `cfg` and `approve = ["fx__echo"]`):
  1. **Disable** `fx` → badge `disabled`, button becomes **Enable**;
     `mcpgw status` shows `fx disabled`, `mcpgw list` shows `[disabled]`.
  2. **Enable** → `cold` again.
  3. A client without elicitation calls `fx__echo` through `mcpgw connect`;
     the card shows `fx__echo · the default endpoint · demo-client` and the
     arguments. **Approve** → the waiting client receives the result after
     13 s, `fx` starts on demand, and Recent calls shows `fx__echo ok`.
  4. No console errors (the CSP holds).
- Automated: `crates/mcpgw/tests/api.rs::web_ui_is_served_with_a_strict_csp`
  (content types, CSP and frame headers, token not in the page, 403 for a
  foreign Host, `mcpgw ui --print` URL); `ui.rs` unit test (the page only
  calls documented endpoints, no inline code); `set_enabled_updates_the_spec`;
  the call log in the API test.
