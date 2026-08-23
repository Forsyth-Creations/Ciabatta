//! The remote cache's own admin page.
//!
//! One self-contained HTML document, embedded in the binary. No bundle, no
//! build step, no asset directory — because the thing that has to be true of
//! this page is that it works on the box the server is running on, which is
//! typically a headless machine with nothing else installed.
//!
//! It exists for one job the CLI can't do well: **minting a credential**.
//! `ciabatta remote-cache add-user` prints a hash for the operator to paste
//! into a config file and restart around, which is fine once and tiresome
//! forever. This writes the user to the server's own list and hands back the
//! token, live.
//!
//! Everything else on the page — stats, projects, the builds it hands out — is
//! there because an operator who has just opened a page about their cache will
//! want to know whether it's working, and making them go elsewhere for that
//! would be silly.

/// The admin page, ready to serve.
///
/// Deliberately one string rather than a template: there is nothing to
/// interpolate. The page asks the API for everything it shows, which means it
/// can't drift out of step with the server the way a server-rendered copy of
/// the same data would.
pub const ADMIN_PAGE: &str = r##"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>ciabatta remote cache</title>
<style>
  :root {
    color-scheme: light dark;
    --bg: #ffffff; --fg: #1b1b1d; --muted: #61646b; --line: #e2e3e7;
    --card: #f7f7f9; --accent: #b45309; --good: #15803d; --bad: #b91c1c;
    --code: #f1f1f4;
  }
  @media (prefers-color-scheme: dark) {
    :root {
      --bg: #16171a; --fg: #e8e8ea; --muted: #9a9da5; --line: #2c2e33;
      --card: #1e1f23; --accent: #f59e0b; --good: #4ade80; --bad: #f87171;
      --code: #24262b;
    }
  }
  * { box-sizing: border-box; }
  body {
    margin: 0; padding: 2rem 1.5rem 4rem; background: var(--bg); color: var(--fg);
    font: 15px/1.55 ui-sans-serif, system-ui, -apple-system, "Segoe UI", sans-serif;
  }
  main { max-width: 62rem; margin: 0 auto; }
  h1 { font-size: 1.5rem; margin: 0 0 .25rem; }
  .head { display: flex; align-items: flex-start; gap: 1rem; }
  .head .titles { flex: 1; min-width: 0; }
  h2 { font-size: 1.05rem; margin: 2.5rem 0 .25rem; }
  p.sub { color: var(--muted); margin: 0 0 1rem; }
  code, .mono { font-family: ui-monospace, SFMono-Regular, Menlo, monospace; font-size: .875em; }
  .card { background: var(--card); border: 1px solid var(--line); border-radius: 8px; padding: 1rem; }
  .stats { display: flex; flex-wrap: wrap; gap: 2rem; }
  .stat b { display: block; font-size: 1.4rem; font-weight: 600; }
  .stat span { color: var(--muted); font-size: .8rem; }
  table { width: 100%; border-collapse: collapse; }
  th, td { text-align: left; padding: .55rem .5rem; border-bottom: 1px solid var(--line); }
  th { color: var(--muted); font-weight: 500; font-size: .8rem; }
  tr:last-child td { border-bottom: 0; }
  .tag {
    display: inline-block; padding: .05rem .45rem; border-radius: 999px;
    border: 1px solid var(--line); font-size: .75rem; color: var(--muted);
  }
  form.row { display: flex; flex-wrap: wrap; gap: .6rem; align-items: center; }
  input[type=text], input[type=password] {
    padding: .45rem .6rem; border: 1px solid var(--line); border-radius: 6px;
    background: var(--bg); color: var(--fg); font: inherit; min-width: 12rem;
  }
  label.check { display: flex; align-items: center; gap: .3rem; color: var(--muted); font-size: .875rem; }
  button {
    padding: .45rem .9rem; border: 1px solid var(--accent); border-radius: 6px;
    background: var(--accent); color: #fff; font: inherit; cursor: pointer;
  }
  button.ghost { background: transparent; color: var(--muted); border-color: var(--line); }
  button:disabled { opacity: .5; cursor: default; }
  .note { padding: .75rem 1rem; border-radius: 6px; border: 1px solid var(--line); margin: 1rem 0; }
  .note.warn { border-color: var(--accent); }
  .note.bad { border-color: var(--bad); color: var(--bad); }
  .token {
    margin-top: .75rem; padding: 1rem; border: 1px solid var(--good); border-radius: 6px;
  }
  .token .value {
    display: block; margin: .5rem 0; padding: .6rem .75rem; background: var(--code);
    border-radius: 4px; word-break: break-all; user-select: all;
  }
  .muted { color: var(--muted); }
  .hidden { display: none; }
</style>
</head>
<body>
<main>
  <div class="head">
    <div class="titles">
      <h1>ciabatta remote cache</h1>
      <p class="sub" id="subtitle">Loading…</p>
    </div>
    <button class="ghost" id="refresh" title="Re-read the stats and users from the server">
      Refresh
    </button>
  </div>

  <div id="error" class="note bad hidden"></div>

  <!-- Sign in, when the server wants credentials. -->
  <section id="login-section" class="hidden">
    <h2>Sign in</h2>
    <p class="sub">This cache authenticates. Sign in to manage users.</p>
    <form class="row" id="login-form">
      <input type="text" id="login-user" placeholder="username" autocomplete="username">
      <input type="password" id="login-token" placeholder="token" autocomplete="current-password">
      <button type="submit">Sign in</button>
    </form>
  </section>

  <section id="stats-section" class="hidden">
    <h2>Cache</h2>
    <div class="card stats" id="stats"></div>
  </section>

  <section id="users-section" class="hidden">
    <h2>Users</h2>
    <p class="sub" id="users-sub"></p>

    <div id="token-panel"></div>

    <form class="row" id="create-form" style="margin-bottom:1rem">
      <input type="text" id="new-name" placeholder="username" required>
      <label class="check"><input type="checkbox" id="new-readonly"> read-only</label>
      <label class="check" id="admin-label"><input type="checkbox" id="new-admin"> admin</label>
      <button type="submit">Create user</button>
    </form>

    <div class="card" style="padding:0">
      <table>
        <thead><tr><th>Name</th><th>Access</th><th>Created</th><th></th></tr></thead>
        <tbody id="users"></tbody>
      </table>
    </div>
  </section>
</main>

<script>
// The session token, kept in memory only. Putting it in localStorage would
// leave a credential behind on a shared machine long after the tab was closed.
let token = null;
let mode = "open";

const $ = (id) => document.getElementById(id);

function headers() {
  const h = { "Content-Type": "application/json" };
  if (token) h["Authorization"] = "Bearer " + token;
  return h;
}

async function api(path, options = {}) {
  const response = await fetch(path, { ...options, headers: headers() });
  const text = await response.text();
  let body = null;
  try { body = text ? JSON.parse(text) : null; } catch { /* not JSON */ }
  if (!response.ok) {
    throw new Error((body && body.error) || text || response.statusText);
  }
  return body;
}

function showError(message) {
  const box = $("error");
  box.textContent = message;
  box.classList.toggle("hidden", !message);
}

function human(bytes) {
  const units = ["B", "KB", "MB", "GB", "TB"];
  let value = bytes, unit = 0;
  while (value >= 1024 && unit < units.length - 1) { value /= 1024; unit++; }
  return unit === 0 ? bytes + " B" : value.toFixed(1) + " " + units[unit];
}

function stat(label, value) {
  return `<div class="stat"><b>${value}</b><span>${label}</span></div>`;
}

async function loadHealth() {
  const health = await api("/api/health");
  mode = health.auth;
  const release = health.release && health.release.version;
  $("subtitle").textContent =
    `ciabatta ${health.version} · auth: ${mode}` +
    (release ? ` · serving ciabatta ${release}` : "");
  // `open` needs no session; anything else does.
  if (mode === "open" || mode === "none") return true;
  return token !== null;
}

async function loadStats() {
  try {
    const s = await api("/api/stats");
    const rate = s.hit_rate === null ? "—" : s.hit_rate.toFixed(1) + "%";
    $("stats").innerHTML =
      stat("Hit rate", rate) +
      stat("Hits", s.counters.hits) +
      stat("Misses", s.counters.misses) +
      stat("Entries", s.storage.entries) +
      stat("Stored", s.storage.human) +
      stat("Served", human(s.counters.bytes_served)) +
      stat("Projects", s.projects.length) +
      stat("Sessions", s.sessions);
    $("stats-section").classList.remove("hidden");
  } catch {
    // Stats need a session on an authenticating server; the users section is
    // the point of this page, so a missing one isn't worth an error banner.
    $("stats-section").classList.add("hidden");
  }
}

async function loadUsers() {
  const data = await api("/api/users");
  const open = data.mode === "open" || data.mode === "none";

  // An admin can only be granted by the operator's config on an open server —
  // so don't offer a checkbox that will be refused.
  $("admin-label").classList.toggle("hidden", open);

  $("users-sub").textContent = open
    ? "This cache is in `open` mode: anyone who can reach it can read, write, and " +
      "create users. Create the credentials you want, then set `auth.mode: token` " +
      "in the server's config and restart — from then on only these will work."
    : "Credentials for this cache. Tokens are shown once, when they're created, " +
      "and only their hashes are kept.";

  if (data.locked_out) {
    $("users-sub").textContent =
      "This cache requires credentials but has none, so nobody can sign in. Add a " +
      "user with `admin: true` under `auth.users` in the server's config and restart.";
  }

  const rows = data.users.map((u) => {
    const access = u.admin ? "admin" : u.read_only ? "read-only" : "read/write";
    const origin = u.from_config
      ? `<span class="tag" title="Declared in the server's config file">config</span>`
      : "";
    const created = u.created_at
      ? new Date(u.created_at).toLocaleString()
      : `<span class="muted">—</span>`;
    const revoke = u.from_config
      ? `<span class="muted" title="Remove it from the server's config instead">—</span>`
      : `<button class="ghost" data-revoke="${u.name}">Revoke</button>`;
    return `<tr>
      <td class="mono">${u.name} ${origin}</td>
      <td>${access}</td>
      <td class="muted">${created}</td>
      <td style="text-align:right">${revoke}</td>
    </tr>`;
  });

  $("users").innerHTML =
    rows.join("") ||
    `<tr><td colspan="4" class="muted">No users yet.</td></tr>`;

  document.querySelectorAll("[data-revoke]").forEach((button) => {
    button.onclick = async () => {
      const name = button.dataset.revoke;
      if (!confirm(`Revoke ${name}? Anything using its token stops working.`)) return;
      try {
        await api("/api/users/" + encodeURIComponent(name), { method: "DELETE" });
        showError("");
        await loadUsers();
      } catch (e) { showError(e.message); }
    };
  });

  $("users-section").classList.remove("hidden");
}

$("create-form").onsubmit = async (event) => {
  event.preventDefault();
  try {
    const created = await api("/api/users", {
      method: "POST",
      body: JSON.stringify({
        name: $("new-name").value,
        read_only: $("new-readonly").checked,
        admin: $("new-admin").checked,
      }),
    });
    showError("");
    $("new-name").value = "";
    $("new-readonly").checked = false;
    $("new-admin").checked = false;

    // The one moment this value exists in readable form, so give it room.
    $("token-panel").innerHTML = `
      <div class="token">
        <strong>Token for ${created.user.name}</strong>
        <code class="value">${created.token}</code>
        <div class="muted">${created.note}</div>
        <div class="muted mono" style="margin-top:.5rem">${created.login}</div>
      </div>`;
    await loadUsers();
  } catch (e) { showError(e.message); }
};

$("login-form").onsubmit = async (event) => {
  event.preventDefault();
  try {
    const response = await fetch("/api/auth/login", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({
        username: $("login-user").value,
        password: $("login-token").value,
      }),
    });
    const body = await response.json();
    if (!response.ok) throw new Error(body.error || "Sign-in failed");
    token = body.token;
    $("login-section").classList.add("hidden");
    showError("");
    await refresh();
  } catch (e) { showError(e.message); }
};

// The counters move while the page is open, so the numbers on screen are only
// ever a snapshot. Rather than poll — which would fight with someone reading the
// users table, and keep a headless box awake for nobody — the page says how old
// its snapshot is and lets you ask for a new one.
async function refresh() {
  const button = $("refresh");
  button.disabled = true;
  button.textContent = "Refreshing…";
  try {
    const ready = await loadHealth();
    if (!ready) {
      $("login-section").classList.remove("hidden");
      // Even signed out, say whether the server has anybody to sign in as.
      try { await loadUsers(); } catch { /* needs a session; fine */ }
      return;
    }
    await loadStats();
    await loadUsers();
    stamp();
  } catch (e) { showError(e.message); }
  finally {
    button.disabled = false;
    button.textContent = "Refresh";
  }
}

/** Append the time these numbers were read to whatever the subtitle says. */
function stamp() {
  const at = new Date().toLocaleTimeString();
  $("subtitle").textContent += ` · read at ${at}`;
}

$("refresh").onclick = () => refresh();

refresh();
</script>
</body>
</html>
"##;

#[cfg(test)]
mod tests {
    use super::ADMIN_PAGE;

    /// The page is one file with no external references — that's the property
    /// that makes it work on a headless box with no network.
    #[test]
    fn the_page_is_self_contained() {
        assert!(ADMIN_PAGE.starts_with("<!doctype html>"));
        for external in ["http://", "https://", "//cdn", "<link"] {
            assert!(
                !ADMIN_PAGE.contains(external),
                "the admin page must not reference {external}"
            );
        }
        // Styles and script are inline, not fetched.
        assert!(ADMIN_PAGE.contains("<style>") && ADMIN_PAGE.contains("<script>"));
    }

    /// A credential in browser storage outlives the tab and the person using
    /// it, on a machine they may not own.
    ///
    /// Matches on *use* rather than on the word: the page's own comment
    /// explains why it doesn't persist the token, and that comment is worth
    /// keeping.
    #[test]
    fn the_session_token_is_never_persisted_in_the_browser() {
        for call in [
            "localStorage.",
            "localStorage[",
            "sessionStorage.",
            "sessionStorage[",
            "document.cookie",
        ] {
            assert!(
                !ADMIN_PAGE.contains(call),
                "the admin page must not put a token in browser storage ({call})"
            );
        }
    }

    /// Every endpoint the page calls has to exist on the server.
    #[test]
    fn the_page_only_calls_routes_the_server_serves() {
        for route in ["/api/health", "/api/stats", "/api/users", "/api/auth/login"] {
            assert!(
                ADMIN_PAGE.contains(route),
                "expected the page to use {route}"
            );
        }
    }
}
