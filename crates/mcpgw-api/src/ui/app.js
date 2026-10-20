// mcpgw web UI: polls /api/v1 and acts on it with the loopback token.
// The token arrives in the URL fragment (never sent to the server), is kept
// in sessionStorage for this tab, and removed from the address bar.
(() => {
  const KEY = "mcpgw-token";
  const m = location.hash.match(/token=([0-9a-f]+)/);
  if (m) {
    sessionStorage.setItem(KEY, m[1]);
    history.replaceState(null, "", location.pathname);
  }
  const token = sessionStorage.getItem(KEY);
  const $ = (id) => document.getElementById(id);

  function notice(text) {
    const n = $("notice");
    n.hidden = !text;
    n.textContent = text || "";
  }

  if (!token) {
    document.querySelector("main").replaceChildren($("login").content.cloneNode(true));
    $("runtime").textContent = "not signed in";
    return;
  }

  async function api(method, path) {
    const r = await fetch("/api/v1" + path, {
      method,
      headers: { Authorization: "Bearer " + token },
    });
    if (r.status === 401) throw new Error("The token was rejected; open the page again with `mcpgw ui`.");
    const body = r.status === 204 ? null : await r.json().catch(() => null);
    if (!r.ok) throw new Error((body && body.error) || r.statusText);
    return body;
  }

  function el(tag, attrs, ...kids) {
    const e = document.createElement(tag);
    for (const [k, v] of Object.entries(attrs || {})) {
      if (k === "class") e.className = v;
      else if (k.startsWith("on")) e.addEventListener(k.slice(2), v);
      else if (v !== false && v != null) e.setAttribute(k, v === true ? "" : v);
    }
    for (const k of kids) if (k != null) e.append(k);
    return e;
  }

  async function act(button, method, path) {
    button.disabled = true;
    try {
      await api(method, path);
      notice("");
      await refresh();
    } catch (e) {
      notice(e.message);
      button.disabled = false;
    }
  }

  function servers(list) {
    $("servers").replaceChildren(...list.map((s) => {
      const state = s.state || "cold";
      const fromConfig = s.source === "config";
      const toggle = el("button", {
        "data-action": s.enabled ? "disable" : "enable",
        "data-server": s.name,
        disabled: fromConfig,
        title: fromConfig ? "Defined in the config file; change it there" : null,
        onclick: (ev) => act(ev.currentTarget, "POST", `/servers/${encodeURIComponent(s.name)}/${s.enabled ? "disable" : "enable"}`),
      }, s.enabled ? "Disable" : "Enable");
      const restart = el("button", {
        "data-action": "restart",
        "data-server": s.name,
        onclick: (ev) => act(ev.currentTarget, "POST", `/servers/${encodeURIComponent(s.name)}/restart`),
      }, "Restart");
      return el("tr", { "data-server": s.name },
        el("td", {}, el("strong", {}, s.name),
          s.last_error && state !== "disabled" ? el("span", { class: "err" }, s.last_error) : null),
        el("td", {}, el("span", { class: `badge s-${state}`, "data-state": state }, state)),
        el("td", { class: "num" }, String(s.tools)),
        el("td", {}, s.source),
        el("td", { class: "actions" }, toggle, restart),
      );
    }));
  }

  function approvals(list) {
    const pending = list.filter((a) => a.status === "pending");
    $("approvals-empty").hidden = pending.length > 0;
    $("approvals").replaceChildren(...pending.map((a) => el("li", { "data-approval": a.id },
      el("div", { class: "tool" }, a.tool),
      el("div", { class: "meta" }, `${a.endpoint}${a.client ? " · " + a.client : ""} · id ${a.id}`),
      el("pre", {}, a.arguments),
      el("button", {
        class: "primary", "data-action": "approve",
        onclick: (ev) => act(ev.currentTarget, "POST", `/approvals/${a.id}/approve`),
      }, "Approve"),
      " ",
      el("button", {
        "data-action": "deny",
        onclick: (ev) => act(ev.currentTarget, "POST", `/approvals/${a.id}/deny`),
      }, "Deny"),
    )));
  }

  function calls(list) {
    $("calls-empty").hidden = list.length > 0;
    $("calls-table").hidden = list.length === 0;
    $("calls").replaceChildren(...list.map((c) => el("tr", {},
      el("td", {}, new Date(c.at_ms).toLocaleTimeString()),
      el("td", {}, el("code", {}, c.tool)),
      el("td", {}, c.endpoint),
      el("td", {}, el("span", { class: `badge o-${c.outcome}` }, c.outcome.replace("_", " "))),
      el("td", { class: "num" }, String(c.duration_ms)),
    )));
  }

  async function refresh() {
    try {
      const [rt, s, a, c] = await Promise.all([
        api("GET", "/runtime"), api("GET", "/servers"), api("GET", "/approvals"), api("GET", "/calls?limit=20"),
      ]);
      $("runtime").textContent =
        `v${rt.version} · ${rt.servers.running} of ${rt.servers.total} servers running · ` +
        `${rt.approvals_pending} waiting for approval`;
      servers(s.servers);
      approvals(a.approvals);
      calls(c.calls);
    } catch (e) {
      notice(e.message);
    }
  }

  refresh();
  setInterval(refresh, 2000);
})();
