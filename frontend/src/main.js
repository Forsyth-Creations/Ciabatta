import "./style.css";

// Injected at build time by vite.config.js (from the release tag or
// package.json), so the version label and download links track the real release.
const VERSION = __APP_VERSION__;
const REPO = "https://github.com/forsyth-creations/ciabatta";

const releaseBase = `${REPO}/releases/download/v${VERSION}`;

const PLATFORMS = [
  { os: "🐧", label: "Linux · x86_64", file: "ciabatta-linux-x86_64.tar.gz", hint: "static (musl) — runs on any distro" },
  { os: "🐧", label: "Linux · ARM64", file: "ciabatta-linux-aarch64.tar.gz", hint: "static (musl)" },
  { os: "🍎", label: "macOS · Apple Silicon", file: "ciabatta-macos-aarch64.tar.gz", hint: "M-series" },
  { os: "🍎", label: "macOS · Intel", file: "ciabatta-macos-x86_64.tar.gz", hint: "x86_64" },
  { os: "🪟", label: "Windows · x86_64", file: "ciabatta-windows-x86_64.zip", hint: "unzip, add to PATH" },
];

const INSTALL_TABS = [
  { id: "cargo", label: "cargo", cmd: "cargo install ciabatta", note: "From crates.io — needs a Rust toolchain." },
  { id: "linux", label: "Linux", cmd: "tar xzf ciabatta-linux-x86_64.tar.gz && sudo mv ciabatta /usr/local/bin/", note: "Prebuilt static binary — no glibc requirement." },
  { id: "macos", label: "macOS", cmd: "tar xzf ciabatta-macos-aarch64.tar.gz && sudo mv ciabatta /usr/local/bin/", note: "Swap in the x86_64 archive on Intel Macs." },
  { id: "source", label: "source", cmd: `cargo install --git ${REPO}`, note: "Build straight from the main branch." },
];

const REGISTRIES = [
  { name: "Nexus — raw", how: "HTTP PUT / GET", pull: true, tag: "" },
  { name: "Nexus — npm", how: "npm publish", pull: false, tag: "new" },
  { name: "Nexus — PyPI", how: "twine upload", pull: false, tag: "new" },
  { name: "Artifactory", how: "HTTP PUT / GET", pull: true, tag: "" },
  { name: "Amazon S3", how: "aws s3 cp", pull: true, tag: "" },
  { name: "Docker registry", how: "docker / podman push", pull: true, tag: "" },
  { name: "Amazon ECR", how: "auto-login, then push", pull: true, tag: "" },
];

const STAGES = [
  { k: "login", d: "Authenticate — login script, or credentials for the registry type" },
  { k: "pre", d: "Anything before the transfer: bundle, compile, sign" },
  { k: "push / pull", d: "The built-in registry action — or your own command" },
  { k: "post", d: "Notify, tag, clean up after a successful transfer" },
];

const COMMANDS = [
  ["ciabatta push [RECIPE…]", "Push one or more recipes in parallel — all of them if you name none."],
  ["ciabatta pull [RECIPE…]", "Fetch artifacts back down; finds the best commit on your branch if the exact one is missing."],
  ["ciabatta list", "List every recipe defined in the config."],
  ["ciabatta init --ci github", "Scaffold a .ciabatta/ directory with a starter config."],
  ["ciabatta configure", "Add a registry (and optionally a recipe) interactively — no hand-editing TOML."],
  ["ciabatta configure auto", "Inspect the repo and pick recipes from a checklist."],
  ["ciabatta tui", "Open the browser: inspect registries, explore remote paths, push on demand."],
  ["ciabatta analyze", "Map the dependency graph and serve an interactive view."],
  ["ciabatta config reference", "Show the full config-format reference."],
];

const CI_MATRIX = [
  ["CIABATTA_BRANCH", "CI_COMMIT_BRANCH", "GITHUB_REF_NAME", "GIT_BRANCH"],
  ["CIABATTA_COMMIT", "CI_COMMIT_SHA", "GITHUB_SHA", "GIT_COMMIT"],
  ["CIABATTA_TAG", "CI_COMMIT_TAG", "GITHUB_REF_NAME", "TAG_NAME"],
  ["CIABATTA_BUILD_NUMBER", "CI_PIPELINE_IID", "GITHUB_RUN_NUMBER", "BUILD_NUMBER"],
];

// Annotated config example (spans classed for lightweight TOML highlighting).
const CONFIG_HTML = `<span class="c"># One TOML file describes where things go and what to publish.</span>
<span class="s">[system]</span>
<span class="k">ci</span>         = <span class="v">"github"</span>
<span class="k">containers</span> = <span class="v">"docker"</span>

<span class="c"># Raw files: choose the repo and where they land inside it.</span>
<span class="s">[registries.nexus]</span>
<span class="k">type</span>       = <span class="v">"nexus"</span>
<span class="k">url</span>        = <span class="v">"https://nexus.example.com"</span>   <span class="c"># bare host</span>
<span class="new">repository</span> = <span class="v">"raw-hosted"</span>            <span class="c"># which repo</span>
<span class="new">format</span>     = <span class="v">"raw"</span>                   <span class="c"># raw | npm | pypi</span>
<span class="new">base_path</span>  = <span class="v">"builds"</span>                <span class="c"># prefix for raw uploads</span>

<span class="c"># Publish an npm package straight to a Nexus npm repo.</span>
<span class="s">[registries.sdk]</span>
<span class="k">type</span>       = <span class="v">"nexus"</span>
<span class="k">url</span>        = <span class="v">"https://nexus.example.com"</span>
<span class="new">repository</span> = <span class="v">"npm-hosted"</span>
<span class="new">format</span>     = <span class="v">"npm"</span>

<span class="c"># A recipe = one artifact + how to ship it.</span>
<span class="s">[recipies.frontend]</span>
<span class="k">registry</span>            = <span class="v">"nexus"</span>
<span class="k">local_artifact_path</span> = <span class="v">"frontend/dist"</span>
<span class="k">publish_path</span>        = <span class="v">"ui/{CIABATTA_BRANCH}/{CIABATTA_COMMIT}/dist.tar.gz"</span>

<span class="s">[recipies.sdk]</span>
<span class="k">registry</span>            = <span class="v">"sdk"</span>
<span class="k">local_artifact_path</span> = <span class="v">"packages/sdk"</span>   <span class="c"># tarball or package dir</span>`;

// Annotated S3 config example.
const S3_CONFIG_HTML = `<span class="c"># Point the registry at a bucket with the s3:// scheme.</span>
<span class="s">[registries.s3]</span>
<span class="k">type</span> = <span class="v">"s3"</span>                        <span class="c"># inferred when the name contains "s3"</span>
<span class="k">url</span>  = <span class="v">"s3://my-artifacts-bucket"</span>

<span class="c"># publish_path becomes the key inside the bucket.</span>
<span class="s">[recipies.release]</span>
<span class="k">registry</span>            = <span class="v">"s3"</span>
<span class="k">local_artifact_path</span> = <span class="v">"target/release/app"</span>
<span class="k">publish_path</span>        = <span class="v">"app/{CIABATTA_BRANCH}/{CIABATTA_COMMIT}/app"</span>
<span class="c">#  → s3://my-artifacts-bucket/app/&lt;branch&gt;/&lt;commit&gt;/app</span>`;

function platformCard(p) {
  return `
    <div class="pcard">
      <div class="pcard__os">${p.os}</div>
      <div class="pcard__label">${p.label}</div>
      <a class="pcard__dl" href="${releaseBase}/${p.file}" download>${p.file} ↓</a>
      <div class="pcard__hint">${p.hint}</div>
    </div>`;
}

function render() {
  const app = document.getElementById("app");
  app.innerHTML = `
    <header class="topbar">
      <div class="topbar__inner">
        <span class="brand"><span class="brand__loaf">🍞</span> ciabatta <span class="brand__ver">v${VERSION}</span></span>
        <span class="topbar__spacer"></span>
        <a class="topbar__link" href="#install">Install</a>
        <a class="topbar__link" href="#config">Config</a>
        <a class="topbar__link" href="#s3">S3</a>
        <a class="topbar__link" href="#commands">Commands</a>
        <a class="topbar__cta" href="${REPO}">GitHub ↗</a>
      </div>
    </header>

    <div class="wrap">
      <section class="hero">
        <div>
          <span class="hero__eyebrow">artifact publishing, made easy</span>
          <h1>Ship every artifact from <em>one TOML file.</em></h1>
          <p class="hero__lede">
            Ciabatta publishes and pulls build artifacts across Nexus, Artifactory,
            S3, Docker, and ECR — now with first-class npm and PyPI publishing
            through Nexus. Declarative recipes, parallel runs, a live terminal UI.
          </p>
          <div class="hero__actions">
            <a class="btn btn--primary" href="#install">Get Ciabatta</a>
            <a class="btn btn--ghost" href="${REPO}">View source</a>
          </div>
          <p class="hero__hint"><b>cargo install ciabatta</b> · or grab a static binary</p>
        </div>
        ${terminalHTML()}
      </section>
    </div>

    <div class="strip">
      <div class="strip__inner">
        <span class="strip__label">Publishes to</span>
        <span class="chip"><span class="dot"></span>Nexus</span>
        <span class="chip"><span class="dot"></span>Artifactory</span>
        <span class="chip"><span class="dot"></span>S3</span>
        <span class="chip"><span class="dot"></span>Docker</span>
        <span class="chip"><span class="dot"></span>ECR</span>
        <span class="chip chip--new"><span class="dot"></span>npm · PyPI (new)</span>
      </div>
    </div>

    <div class="wrap">
      <section class="section reveal" id="install">
        <div class="section__head">
          <div class="eyebrow">Install</div>
          <h2>One binary. No runtime.</h2>
          <p class="section__sub">Pick a package manager or grab a prebuilt binary. Linux builds are statically linked, so there's no glibc version to worry about.</p>
        </div>
        <div class="tabs" role="tablist" aria-label="Install method">
          ${INSTALL_TABS.map((t, i) => `<button class="tab" role="tab" id="tab-${t.id}" aria-controls="panel-install" aria-selected="${i === 0}">${t.label}</button>`).join("")}
        </div>
        <div class="cmdline" id="panel-install" role="tabpanel">
          <span class="cmdline__sigil">$</span>
          <span class="cmdline__text" id="install-cmd">${INSTALL_TABS[0].cmd}</span>
          <button class="copy" data-copy-target="install-cmd">copy</button>
        </div>
        <p class="cmdline__note" id="install-note">${INSTALL_TABS[0].note}</p>
        <div class="platforms">${PLATFORMS.map(platformCard).join("")}</div>
      </section>

      <section class="section reveal">
        <div class="section__head">
          <div class="eyebrow">The idea</div>
          <h2>Recipes, run in stages.</h2>
          <p class="section__sub">A recipe names one artifact and how to ship it. Every push and pull runs the same four-stage pipeline — override any stage with your own command, or lean on the defaults.</p>
        </div>
        <div class="pipeline">
          ${STAGES.map((s) => `
            <div class="stagechip">
              <div class="stagechip__k">${s.k}</div>
              <div class="stagechip__d">${s.d}</div>
              <span class="stagechip__arrow">→</span>
            </div>`).join("")}
        </div>
        <div class="grid">
          <div class="fcard"><div class="fcard__icon">🧩</div><h3>Parallel by default</h3><p>Name several recipes and Ciabatta runs them at once, each tracked live in the TUI.</p></div>
          <div class="fcard"><div class="fcard__icon">↩</div><h3>Push and pull</h3><p>Ciabatta knows where artifacts live, so it fetches them back too — and on a miss, walks your branch history for the newest published commit.</p></div>
          <div class="fcard"><div class="fcard__icon">🔎</div><h3>Dry run anything</h3><p><code>--dry-run</code> prints the exact URLs and commands before a single byte moves.</p></div>
        </div>
      </section>

      <section class="section reveal" id="config">
        <div class="section__head">
          <div class="eyebrow">Configuration</div>
          <h2>Point it at a Nexus repo — raw, npm, or PyPI.</h2>
          <p class="section__sub">Set the bare Nexus host once, then choose the <code>repository</code> and <code>format</code> per registry. Raw files upload over HTTP; npm and PyPI publish with their native tools.</p>
        </div>
        <div class="split">
          <pre class="code">${CONFIG_HTML}</pre>
          <div class="tablecard">
            <table>
              <thead><tr><th>Registry</th><th>Push</th><th>Pull</th><th>How</th></tr></thead>
              <tbody>
                ${REGISTRIES.map((r) => `
                  <tr>
                    <td>${r.name}${r.tag === "new" ? '<span class="badge-new">NEW</span>' : ""}</td>
                    <td><span class="yes">✓</span></td>
                    <td>${r.pull ? '<span class="yes">✓</span>' : "—"}</td>
                    <td><code>${r.how}</code></td>
                  </tr>`).join("")}
              </tbody>
            </table>
          </div>
        </div>
        <p class="cmdline__note">
          Auth for every format reads <code>CIABATTA_&lt;NAME&gt;_USER</code> / <code>_PASS</code> from the
          environment (npm also accepts a <code>CIABATTA_&lt;NAME&gt;_TOKEN</code>). npm needs <code>npm</code>
          on PATH; PyPI needs <code>twine</code>.
        </p>
      </section>

      <section class="section reveal" id="s3">
        <div class="section__head">
          <div class="eyebrow">Configuration · S3</div>
          <h2>Publish to an S3 bucket.</h2>
          <p class="section__sub">Ciabatta drives the AWS CLI, so an S3 registry is just a bucket URL. Set the <code>url</code> to <code>s3://&lt;bucket&gt;</code> and each recipe's <code>publish_path</code> becomes the object key.</p>
        </div>
        <div class="split">
          <pre class="code">${S3_CONFIG_HTML}</pre>
          <div class="grid" style="grid-template-columns: 1fr;">
            <div class="fcard"><div class="fcard__icon">🪣</div><h3>Bucket in, key out</h3><p>Use <code>url = "s3://bucket"</code>. Ciabatta joins it with <code>publish_path</code> and runs <code>aws s3 cp</code> — push uploads, <code>ciabatta pull</code> downloads.</p></div>
            <div class="fcard"><div class="fcard__icon">🔑</div><h3>Standard AWS auth</h3><p>No login script needed. Credentials come from the usual chain: <code>AWS_ACCESS_KEY_ID</code> / <code>AWS_SECRET_ACCESS_KEY</code>, <code>AWS_PROFILE</code>, or an instance role.</p></div>
            <div class="fcard"><div class="fcard__icon">⚙</div><h3>Needs the AWS CLI</h3><p>Install and configure the <code>aws</code> CLI on the machine or runner. Set <code>AWS_REGION</code> if your bucket isn't in the CLI's default region.</p></div>
          </div>
        </div>
      </section>

      <section class="section reveal" id="commands">
        <div class="section__head">
          <div class="eyebrow">Reference</div>
          <h2>Commands</h2>
        </div>
        <div class="tablecard">
          <table>
            <thead><tr><th>Command</th><th>What it does</th></tr></thead>
            <tbody>
              ${COMMANDS.map(([c, d]) => `<tr><td><code>${c}</code></td><td>${d}</td></tr>`).join("")}
            </tbody>
          </table>
        </div>
        <p class="cmdline__note">
          On <code>push</code> / <code>pull</code>: <code>-e KEY=VALUE</code> overrides a variable,
          <code>--dry-run</code> previews, <code>--no-tui</code> streams plain logs for CI. Set
          <code>CIABATTA_ENV=local</code> to resolve branch and commit from local git on a dev machine.
        </p>
      </section>

      <section class="section reveal">
        <div class="section__head">
          <div class="eyebrow">CI-aware</div>
          <h2>Metadata, resolved for you.</h2>
          <p class="section__sub">Set <code>ci</code> and Ciabatta reads branch, commit, tag, and build number straight from your CI — then lets you template them into publish paths.</p>
        </div>
        <div class="tablecard">
          <table>
            <thead><tr><th>Ciabatta variable</th><th>GitLab CI</th><th>GitHub Actions</th><th>Jenkins</th></tr></thead>
            <tbody>
              ${CI_MATRIX.map((row) => `<tr>${row.map((c, i) => `<td>${i === 0 ? `<code>${c}</code>` : `<code>${c}</code>`}</td>`).join("")}</tr>`).join("")}
            </tbody>
          </table>
        </div>
        <p class="cmdline__note">Also supported: CircleCI, Travis CI, Azure DevOps, and Bitbucket Pipelines.</p>
      </section>

      <section class="section reveal">
        <div class="section__head">
          <div class="eyebrow">Bonus</div>
          <h2>See how your repo is wired.</h2>
          <p class="section__sub"><code>ciabatta analyze</code> maps requirements, dependencies, internal packages, and publish points into an interactive, self-contained graph on <code>localhost:8080</code>.</p>
        </div>
        <div class="grid">
          <div class="fcard"><div class="fcard__icon">🕸</div><h3>Four columns, left to right</h3><p>Requirements → dependencies (crates.io, npm, pip, Docker images) → your internal packages → publish points.</p></div>
          <div class="fcard"><div class="fcard__icon">🍞</div><h3>Managed vs. inferred</h3><p>Publish points from a Ciabatta recipe are flagged 🍞, apart from ones inferred from your <code>.sh</code> scripts.</p></div>
          <div class="fcard"><div class="fcard__icon">🛡</div><h3>Vulnerability check</h3><p><code>--check-vulns</code> annotates dependencies with known OSV advisories. Filter the graph by name, ecosystem, or workspace.</p></div>
        </div>
      </section>
    </div>

    <footer class="footer">
      <div class="footer__loaf">🍞</div>
      <div class="footer__links">
        <a href="${REPO}">GitHub</a>
        <a href="${REPO}/releases/latest">Releases</a>
        <a href="${REPO}/blob/main/README.md">Docs</a>
      </div>
      <p class="footer__fine">Ciabatta v${VERSION} · MIT License · Artifact publishing made easy</p>
    </footer>
  `;
}

// ── The signature terminal: a replay of `ciabatta push` ──────────────────────
function terminalHTML() {
  const recipes = [
    { name: "frontend", stages: ["login", "pre", "push", "post"] },
    { name: "sdk", stages: ["login", "pre", "publish", "post"] },
    { name: "api", stages: ["login", "pre", "push", "post"] },
  ];
  return `
    <div class="term" id="term" aria-label="Terminal replay of ciabatta push">
      <div class="term__bar">
        <span class="term__dot term__dot--r"></span>
        <span class="term__dot term__dot--y"></span>
        <span class="term__dot term__dot--g"></span>
        <span class="term__title">ciabatta push frontend sdk api</span>
      </div>
      <div class="term__body">
        <div class="term__prompt"><span class="sigil">$</span> <span class="cmd">ciabatta push frontend sdk api</span></div>
        <div class="term__caption"><span class="loaf">🍞</span> pushing 3 recipes in parallel</div>
        ${recipes.map((r, ri) => `
          <div class="trecipe" data-recipe="${ri}">
            <span class="trecipe__name">${r.name}</span>
            <span class="trecipe__line">
              <span class="tstatus" data-s="pending">○</span>
              <span class="tstages">
                ${r.stages.map((s) => `<span class="tstage" data-s="pending">${s}</span>`).join("")}
              </span>
              <span class="tbar"><span class="tbar__fill"></span></span>
            </span>
          </div>`).join("")}
        <div class="term__done" id="term-done">✓ done · 3 recipes in 2.4s</div>
      </div>
    </div>`;
}

function playTerminal() {
  const term = document.getElementById("term");
  if (!term) return;
  const reduce = window.matchMedia("(prefers-reduced-motion: reduce)").matches;
  const rows = [...term.querySelectorAll(".trecipe")];

  const finish = (row) => {
    row.dataset.done = "1";
    row.querySelector(".tstatus").dataset.s = "done";
    row.querySelector(".tstatus").textContent = "✓";
    row.querySelectorAll(".tstage").forEach((s) => (s.dataset.s = "done"));
    row.querySelector(".tbar__fill").style.width = "100%";
  };

  if (reduce) {
    rows.forEach(finish);
    document.getElementById("term-done").classList.add("show");
    return;
  }

  // Each recipe advances stage by stage; the bar fills as stages complete.
  rows.forEach((row, ri) => {
    const status = row.querySelector(".tstatus");
    const stages = [...row.querySelectorAll(".tstage")];
    const fill = row.querySelector(".tbar__fill");
    const startDelay = 200 + ri * 260;
    const stepMs = 300;

    setTimeout(() => {
      status.dataset.s = "running";
      status.textContent = "◑";
    }, startDelay);

    stages.forEach((stage, si) => {
      setTimeout(() => {
        if (si > 0) stages[si - 1].dataset.s = "done";
        stage.dataset.s = "running";
        fill.style.width = `${((si + 1) / (stages.length + 1)) * 100}%`;
      }, startDelay + si * stepMs);
    });

    setTimeout(() => finish(row), startDelay + stages.length * stepMs);
  });

  const total = 200 + (rows.length - 1) * 260 + 4 * 300 + 200;
  setTimeout(() => document.getElementById("term-done").classList.add("show"), total);
}

// ── Interactions ─────────────────────────────────────────────────────────────
function wireInstallTabs() {
  const tabs = [...document.querySelectorAll(".tab")];
  const cmdEl = document.getElementById("install-cmd");
  const noteEl = document.getElementById("install-note");
  tabs.forEach((tab) => {
    tab.addEventListener("click", () => {
      const id = tab.id.replace("tab-", "");
      const entry = INSTALL_TABS.find((t) => t.id === id);
      if (!entry) return;
      tabs.forEach((t) => t.setAttribute("aria-selected", t === tab ? "true" : "false"));
      cmdEl.textContent = entry.cmd;
      noteEl.textContent = entry.note;
    });
  });
}

function wireCopyButtons() {
  document.querySelectorAll("[data-copy-target]").forEach((btn) => {
    btn.addEventListener("click", async () => {
      const target = document.getElementById(btn.dataset.copyTarget);
      if (!target) return;
      try {
        await navigator.clipboard.writeText(target.textContent.trim());
        const prev = btn.textContent;
        btn.textContent = "copied ✓";
        btn.classList.add("copied");
        setTimeout(() => {
          btn.textContent = prev;
          btn.classList.remove("copied");
        }, 1400);
      } catch {
        btn.textContent = "select & copy";
      }
    });
  });
}

function wireReveal() {
  const reduce = window.matchMedia("(prefers-reduced-motion: reduce)").matches;
  const els = [...document.querySelectorAll(".reveal")];
  if (reduce || !("IntersectionObserver" in window)) {
    els.forEach((el) => el.classList.add("in"));
    return;
  }
  const io = new IntersectionObserver(
    (entries) => {
      entries.forEach((e) => {
        if (e.isIntersecting) {
          e.target.classList.add("in");
          io.unobserve(e.target);
        }
      });
    },
    { rootMargin: "0px 0px -10% 0px" },
  );
  els.forEach((el) => io.observe(el));
}

render();
wireInstallTabs();
wireCopyButtons();
wireReveal();
playTerminal();
