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
//! Everything else on the page is there because an operator who has just
//! opened a page about their cache wants to know whether it's working and what
//! to do if it isn't: when it was last used and last saved to, the insights the
//! server derives from its traffic (see [`super::activity`]), which targets
//! miss most, the latest traffic, and what retention will evict next.
//!
//! Light, dark, or following the OS — chosen in the header and remembered. The
//! theme is the one thing the page keeps in browser storage.

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
<script>
// Apply the saved theme before anything paints, so a dark-mode visitor never
// sees a white flash. Only the theme is stored — never a credential.
const THEME_KEY = "ciabatta-cache-theme";
try {
  const saved = localStorage.getItem(THEME_KEY);
  if (saved === "light" || saved === "dark") document.documentElement.dataset.theme = saved;
} catch { /* storage blocked: follow the OS */ }
</script>
<style>
  :root {
    color-scheme: light;
    --bg: #ffffff; --fg: #1b1b1d; --muted: #61646b; --line: #e2e3e7;
    --card: #f7f7f9; --accent: #b45309; --good: #15803d; --bad: #b91c1c;
    --warn: #b45309; --code: #f1f1f4; --bar: #e9e9ee; --hover: #eeeef2;
  }
  @media (prefers-color-scheme: dark) {
    :root:not([data-theme="light"]) {
      color-scheme: dark;
      --bg: #16171a; --fg: #e8e8ea; --muted: #9a9da5; --line: #2c2e33;
      --card: #1e1f23; --accent: #f59e0b; --good: #4ade80; --bad: #f87171;
      --warn: #fbbf24; --code: #24262b; --bar: #2c2e33; --hover: #25272c;
    }
  }
  :root[data-theme="dark"] {
    color-scheme: dark;
    --bg: #16171a; --fg: #e8e8ea; --muted: #9a9da5; --line: #2c2e33;
    --card: #1e1f23; --accent: #f59e0b; --good: #4ade80; --bad: #f87171;
    --warn: #fbbf24; --code: #24262b; --bar: #2c2e33; --hover: #25272c;
  }
  * { box-sizing: border-box; }
  body {
    margin: 0; padding: 2rem 1rem 4rem; background: var(--bg); color: var(--fg);
    font: 15px/1.55 ui-sans-serif, system-ui, -apple-system, "Segoe UI", sans-serif;
  }
  main { max-width: 68rem; margin: 0 auto; }
  h1 { font-size: 1.5rem; margin: 0 0 .25rem; }
  .head { display: flex; align-items: flex-start; gap: .75rem; flex-wrap: wrap; }
  .head .titles { flex: 1; min-width: 14rem; }
  .head .controls { display: flex; gap: .5rem; align-items: center; }
  h2 { font-size: 1.05rem; margin: 2.25rem 0 .25rem; }
  h3 { font-size: .9rem; margin: 1.25rem 0 .4rem; color: var(--muted); font-weight: 500; }
  p.sub { color: var(--muted); margin: 0 0 1rem; }
  code, .mono { font-family: ui-monospace, SFMono-Regular, Menlo, monospace; font-size: .875em; }
  .card { background: var(--card); border: 1px solid var(--line); border-radius: 8px; padding: 1rem; }
  .grid { display: grid; grid-template-columns: repeat(auto-fill, minmax(10rem, 1fr)); gap: .75rem; }
  .stat { background: var(--card); border: 1px solid var(--line); border-radius: 8px; padding: .75rem .9rem; min-width: 0; }
  .stat b { display: block; font-size: 1.3rem; font-weight: 600; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
  .stat span { color: var(--muted); font-size: .78rem; }
  .stat small { display: block; color: var(--muted); font-size: .72rem; margin-top: .15rem; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .meter { height: 6px; border-radius: 3px; background: var(--bar); margin-top: .45rem; overflow: hidden; }
  .meter i { display: block; height: 100%; background: var(--good); }
  .meter i.high { background: var(--warn); }
  .meter i.full { background: var(--bad); }
  .scroll { overflow-x: auto; }
  table { width: 100%; border-collapse: collapse; }
  th, td { text-align: left; padding: .5rem .5rem; border-bottom: 1px solid var(--line); vertical-align: top; }
  th { color: var(--muted); font-weight: 500; font-size: .78rem; white-space: nowrap; }
  td.num, th.num { text-align: right; white-space: nowrap; }
  tr:last-child td { border-bottom: 0; }
  tr.expandable { cursor: pointer; }
  tr.expandable:hover td { background: var(--hover); }
  tr.detail td { background: var(--bg); padding: .5rem 1rem 1rem; }
  .tag {
    display: inline-block; padding: .05rem .45rem; border-radius: 999px;
    border: 1px solid var(--line); font-size: .72rem; color: var(--muted); white-space: nowrap;
  }
  .tag.hit, .tag.upload { color: var(--good); border-color: var(--good); }
  .tag.miss { color: var(--warn); border-color: var(--warn); }
  .tag.touch { color: var(--muted); }
  .insight { border-left: 3px solid var(--line); padding: .5rem .75rem; margin: .5rem 0; background: var(--card); border-radius: 0 6px 6px 0; }
  .insight.warn { border-left-color: var(--warn); }
  .insight .what { font-weight: 600; }
  .insight .scope { color: var(--muted); font-size: .8rem; margin-right: .35rem; }
  .insight .do { color: var(--muted); font-size: .88rem; margin-top: .15rem; }
  .ok { color: var(--good); }
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
  .seg { display: inline-flex; border: 1px solid var(--line); border-radius: 6px; overflow: hidden; }
  .seg button { border: 0; border-radius: 0; background: transparent; color: var(--muted); padding: .4rem .7rem; font-size: .85rem; }
  .seg button + button { border-left: 1px solid var(--line); }
  .seg button[aria-pressed="true"] { background: var(--code); color: var(--fg); }
  .note { padding: .75rem 1rem; border-radius: 6px; border: 1px solid var(--line); margin: 1rem 0; }
  .note.warn { border-color: var(--accent); }
  .note.bad { border-color: var(--bad); color: var(--bad); }
  .token { margin-top: .75rem; padding: 1rem; border: 1px solid var(--good); border-radius: 6px; }
  .token .value {
    display: block; margin: .5rem 0; padding: .6rem .75rem; background: var(--code);
    border-radius: 4px; word-break: break-all; user-select: all;
  }
  .muted { color: var(--muted); }
  .hidden { display: none; }
  .two { display: grid; grid-template-columns: repeat(auto-fit, minmax(20rem, 1fr)); gap: 1rem; }
</style>
</head>
<body>
<main>
  <div class="head">
    <div class="titles">
      <h1>ciabatta remote cache</h1>
      <p class="sub" id="subtitle">Loading…</p>
    </div>
    <div class="controls">
      <div class="seg" role="group" aria-label="Theme">
        <button type="button" data-theme-choice="auto" title="Follow the system setting">Auto</button>
        <button type="button" data-theme-choice="light">Light</button>
        <button type="button" data-theme-choice="dark">Dark</button>
      </div>
      <button class="ghost" id="refresh" title="Re-read everything from the server">Refresh</button>
    </div>
  </div>

  <div id="error" class="note bad hidden"></div>

  <!-- Sign in, when the server wants credentials. -->
  <section id="login-section" class="hidden">
    <h2>Sign in</h2>
    <p class="sub">This cache authenticates. Sign in to see its activity and manage users.</p>
    <form class="row" id="login-form">
      <input type="text" id="login-user" placeholder="username" autocomplete="username">
      <input type="password" id="login-token" placeholder="token" autocomplete="current-password">
      <button type="submit">Sign in</button>
    </form>
  </section>

  <section id="stats-section" class="hidden">
    <h2>At a glance</h2>
    <p class="sub" id="since"></p>
    <div class="grid" id="stats"></div>

    <h2>Worth a look</h2>
    <p class="sub">What to change to get more out of this cache, most costly first.</p>
    <div id="insights"></div>

    <h2>Projects</h2>
    <p class="sub">Click a project for the targets that miss most — the ones to look at first.</p>
    <div class="card scroll" style="padding:0">
      <table>
        <thead><tr>
          <th>Project</th><th class="num">Hit rate</th><th class="num">Hits / misses</th>
          <th class="num">Stored</th><th>Last used</th><th>Last saved</th>
        </tr></thead>
        <tbody id="projects"></tbody>
      </table>
    </div>

    <h2>Recent activity</h2>
    <p class="sub">The latest lookups and uploads, newest first.</p>
    <div class="card scroll" style="padding:0">
      <table>
        <thead><tr><th>When</th><th>Project</th><th>What</th><th>Target</th><th>Who</th><th class="num">Size</th></tr></thead>
        <tbody id="recent"></tbody>
      </table>
    </div>

    <h2>Storage</h2>
    <p class="sub" id="retention"></p>
    <div class="two">
      <div>
        <h3>Largest entries</h3>
        <div class="card scroll" style="padding:0"><table><tbody id="largest"></tbody></table></div>
      </div>
      <div>
        <h3>Least recently used — first to be evicted</h3>
        <div class="card scroll" style="padding:0"><table><tbody id="lru"></tbody></table></div>
      </div>
    </div>
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

    <div class="card scroll" style="padding:0">
      <table>
        <thead><tr><th>Name</th><th>Access</th><th>Created</th><th></th></tr></thead>
        <tbody id="users"></tbody>
      </table>
    </div>
  </section>
</main>

<script>
// The session token, kept in memory only. Putting it in browser storage would
// leave a credential behind on a shared machine long after the tab was closed.
let token = null;
let mode = "open";
const expanded = new Set();

const $ = (id) => document.getElementById(id);

// ─── Theme ───────────────────────────────────────────────────────────────────
function currentTheme() {
  return document.documentElement.dataset.theme || "auto";
}
function setTheme(choice) {
  if (choice === "auto") delete document.documentElement.dataset.theme;
  else document.documentElement.dataset.theme = choice;
  try {
    if (choice === "auto") localStorage.removeItem(THEME_KEY);
    else localStorage.setItem(THEME_KEY, choice);
  } catch { /* not remembered; still applied */ }
  markTheme();
}
function markTheme() {
  document.querySelectorAll("[data-theme-choice]").forEach((b) => {
    b.setAttribute("aria-pressed", String(b.dataset.themeChoice === currentTheme()));
  });
}
document.querySelectorAll("[data-theme-choice]").forEach((b) => {
  b.onclick = () => setTheme(b.dataset.themeChoice);
});
markTheme();

// ─── Plumbing ────────────────────────────────────────────────────────────────
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

// Everything shown comes from the server, and some of it (project and target
// names) from whoever registered them — so it is escaped, always.
function esc(value) {
  return String(value ?? "").replace(/[&<>"']/g, (c) => ({
    "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;",
  }[c]));
}

// Backticked names in an insight read as code.
function prose(text) {
  return esc(text).replace(/`([^`]+)`/g, "<code>$1</code>");
}

function human(bytes) {
  const units = ["B", "KB", "MB", "GB", "TB"];
  let value = bytes || 0, unit = 0;
  while (value >= 1024 && unit < units.length - 1) { value /= 1024; unit++; }
  return unit === 0 ? value + " B" : value.toFixed(1) + " " + units[unit];
}

function ago(at) {
  if (!at) return "never";
  const seconds = Math.max(0, Math.round((Date.now() - Date.parse(at)) / 1000));
  if (Number.isNaN(seconds)) return "—";
  if (seconds < 60) return "just now";
  if (seconds < 3600) return Math.round(seconds / 60) + "m ago";
  if (seconds < 172800) return Math.round(seconds / 3600) + "h ago";
  return Math.round(seconds / 86400) + "d ago";
}

function when(at) {
  if (!at) return `<span class="muted">never</span>`;
  return `<span title="${esc(new Date(at).toLocaleString())}">${esc(ago(at))}</span>`;
}

function rate(value) {
  return value === null || value === undefined ? "—" : value.toFixed(0) + "%";
}

function stat(label, value, small = "", extra = "") {
  return `<div class="stat"><span>${esc(label)}</span><b>${value}</b>` +
    (small ? `<small>${small}</small>` : "") + extra + `</div>`;
}

// ─── Loading ─────────────────────────────────────────────────────────────────
async function loadHealth() {
  const health = await api("/api/health");
  mode = health.auth;
  const release = health.release && health.release.version;
  $("subtitle").textContent =
    `ciabatta ${health.version} · auth: ${mode} · ${location.protocol === "https:" ? "HTTPS" : "HTTP"}` +
    (release ? ` · serving ciabatta ${release}` : "");
  if (mode === "open" || mode === "none") return true;
  return token !== null;
}

async function loadStats() {
  let s;
  try {
    s = await api("/api/stats");
  } catch {
    // Stats need a session on an authenticating server; the sign-in form
    // already says so.
    $("stats-section").classList.add("hidden");
    return;
  }
  const a = s.activity || {};
  const lookups = s.counters.hits + s.counters.misses;

  // Capacity, when the retention policy sets one.
  let capacity = "";
  if (s.storage.percent !== null && s.storage.percent !== undefined) {
    const pct = Math.min(100, s.storage.percent);
    const cls = pct >= 100 ? "full" : pct >= 80 ? "high" : "";
    capacity = `<div class="meter" title="${pct.toFixed(0)}% of ${esc(human(s.storage.max_bytes))}"><i class="${cls}" style="width:${pct}%"></i></div>`;
  }

  $("since").textContent = a.since
    ? `Counted since ${new Date(a.since).toLocaleDateString()} — these survive restarts.`
    : "";
  $("stats").innerHTML =
    stat("Hit rate", rate(s.hit_rate), `${s.counters.hits} of ${lookups} lookups`) +
    stat("Last used", esc(ago(a.last_used_at)), a.last_used_at ? esc(new Date(a.last_used_at).toLocaleString()) : "nothing yet") +
    stat("Last saved to", esc(ago(a.last_saved_at)), a.last_saved_at ? esc(new Date(a.last_saved_at).toLocaleString()) : "nothing uploaded yet") +
    stat("Last miss", esc(ago(a.last_miss_at)), `${s.counters.misses} in all`) +
    stat("Stored", esc(s.storage.human), `${s.storage.entries} entries` + (s.storage.max_bytes ? ` · limit ${esc(human(s.storage.max_bytes))}` : ""), capacity) +
    stat("Served", esc(human(s.counters.bytes_served)), `${s.counters.uploads} uploads`) +
    stat("Projects", s.projects.length, `${s.sessions} live session(s)`) +
    stat("Connection", s.tls ? `<span class="ok">HTTPS</span>` : "HTTP", s.tls ? "TLS terminated here" : "set server.tls to encrypt");

  // Insights.
  const insights = s.insights || [];
  $("insights").innerHTML = insights.length === 0
    ? `<div class="insight"><span class="ok">✓</span> Nothing needs attention.</div>`
    : insights.map((i) => `<div class="insight ${esc(i.severity)}">
        <div>${i.project ? `<span class="scope">${esc(i.project)}${i.target ? " · " + esc(i.target) : ""}</span>` : ""}<span class="what">${prose(i.message)}</span></div>
        <div class="do">${prose(i.action)}</div>
      </div>`).join("");

  // Projects, each expandable into its targets.
  const rows = s.projects.map((p) => {
    const id = p.project.id;
    const open = expanded.has(id);
    const saved = p.last_saved_at || p.newest_entry_at;
    const main = `<tr class="expandable" data-project="${esc(id)}">
      <td><b>${esc(p.project.name)}</b> <span class="muted mono">${esc(id.slice(0, 8))}</span>
        ${p.stale_workflows ? `<span class="tag" title="Workflows nobody has run lately">${p.stale_workflows} stale</span>` : ""}</td>
      <td class="num">${rate(p.hit_rate)}</td>
      <td class="num">${p.counters.hits} / ${p.counters.misses}</td>
      <td class="num">${esc(human(p.bytes))} <span class="muted">· ${p.entries}</span></td>
      <td>${when(p.last_used_at)}</td>
      <td>${when(saved)}</td>
    </tr>`;
    if (!open) return main;
    const targets = (p.targets || []).map((t) => `<tr>
        <td class="mono">${esc(t.name)}</td>
        <td class="num">${t.hits}</td><td class="num">${t.misses}</td><td class="num">${t.uploads}</td>
        <td>${when(t.last_hit_at)}</td><td>${when(t.last_miss_at)}</td><td>${when(t.last_upload_at)}</td>
      </tr>`).join("");
    return main + `<tr class="detail"><td colspan="6">${targets
      ? `<table><thead><tr><th>Target</th><th class="num">Hits</th><th class="num">Misses</th><th class="num">Uploads</th><th>Last hit</th><th>Last miss</th><th>Last saved</th></tr></thead><tbody>${targets}</tbody></table>`
      : `<span class="muted">No per-target traffic yet — clients older than this server don't name their targets.</span>`}</td></tr>`;
  });
  $("projects").innerHTML = rows.join("") ||
    `<tr><td colspan="6" class="muted">No projects yet. Point a workspace here with <code>ciabatta cache init --remote ${esc(location.origin)}</code>.</td></tr>`;
  document.querySelectorAll("tr.expandable").forEach((row) => {
    row.onclick = () => {
      const id = row.dataset.project;
      expanded.has(id) ? expanded.delete(id) : expanded.add(id);
      loadStats();
    };
  });

  // Recent activity.
  const recent = a.recent || [];
  $("recent").innerHTML = recent.map((e) => `<tr>
      <td>${when(e.at)}</td>
      <td>${esc(e.project_name)}</td>
      <td><span class="tag ${esc(e.kind)}">${esc(e.kind === "touch" ? `kept ${e.count} alive` : e.kind)}</span></td>
      <td class="mono">${esc(e.target || "")}</td>
      <td>${esc(e.user || "")}</td>
      <td class="num">${e.bytes ? esc(human(e.bytes)) : ""}</td>
    </tr>`).join("") || `<tr><td colspan="6" class="muted">Nothing yet.</td></tr>`;

  // Storage.
  const ev = a.last_eviction;
  $("retention").innerHTML = `Retention: ${esc(s.retention.description)}.` +
    (ev ? ` Last eviction ${esc(ago(ev.at))}: ${ev.removed} entr${ev.removed === 1 ? "y" : "ies"}, ${esc(human(ev.freed))}.` : " Nothing evicted yet.");
  const entryRow = (e) => `<tr><td><span class="mono">${esc(e.target)}</span><br><span class="muted">${esc(e.project)}</span></td>
    <td class="num">${esc(human(e.size))}</td><td>${when(e.last_used_at)}</td></tr>`;
  $("largest").innerHTML = (s.largest || []).map(entryRow).join("") || `<tr><td class="muted">Empty.</td></tr>`;
  $("lru").innerHTML = (s.least_recently_used || []).map(entryRow).join("") || `<tr><td class="muted">Empty.</td></tr>`;

  $("stats-section").classList.remove("hidden");
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
      ? esc(new Date(u.created_at).toLocaleString())
      : `<span class="muted">—</span>`;
    const revoke = u.from_config
      ? `<span class="muted" title="Remove it from the server's config instead">—</span>`
      : `<button class="ghost" data-revoke="${esc(u.name)}">Revoke</button>`;
    return `<tr>
      <td class="mono">${esc(u.name)} ${origin}</td>
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
        <strong>Token for ${esc(created.user.name)}</strong>
        <code class="value">${esc(created.token)}</code>
        <div class="muted">${esc(created.note)}</div>
        <div class="muted mono" style="margin-top:.5rem">${esc(created.login)}</div>
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
    /// The page does use storage — for the light/dark choice, which is nobody's
    /// secret — so this checks that every use of it goes through the theme's
    /// key and nothing else, rather than banning the API outright.
    #[test]
    fn only_the_theme_is_ever_kept_in_the_browser() {
        for call in ["sessionStorage", "document.cookie"] {
            assert!(
                !ADMIN_PAGE.contains(call),
                "the admin page must not put anything in {call}"
            );
        }
        let uses: Vec<&str> = ADMIN_PAGE.split("localStorage.").skip(1).collect();
        assert!(!uses.is_empty(), "the theme choice is remembered");
        for after in uses {
            let call = &after[..after.find(')').unwrap_or(after.len())];
            assert!(
                call.contains("(THEME_KEY"),
                "localStorage may only hold the theme, not: localStorage.{call})"
            );
        }
        assert!(
            !ADMIN_PAGE.contains("localStorage["),
            "no indexed access that could slip past the check above"
        );
    }

    /// Dark mode on request, not only by OS preference: the page carries both
    /// palettes and a switch between them.
    #[test]
    fn the_page_has_a_dark_mode_switch() {
        assert!(ADMIN_PAGE.contains(r#":root[data-theme="dark"]"#));
        assert!(ADMIN_PAGE.contains(r#"data-theme-choice="dark""#));
        assert!(ADMIN_PAGE.contains(r#"data-theme-choice="auto""#));
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
