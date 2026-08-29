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
  { id: "quick", label: "Linux / macOS", cmd: "curl -fsSL https://forsyth-creations.github.io/Ciabatta/install.sh | sh", note: "Detects your OS and CPU, then installs the matching prebuilt binary. No Rust toolchain needed. Pin a version with <code>| sh -s -- --version 0.3.0</code>, or list them with <code>--list</code>." },
  { id: "windows", label: "Windows", cmd: "irm https://forsyth-creations.github.io/Ciabatta/install.ps1 | iex", note: "Run in PowerShell — downloads the binary and adds it to your PATH. To pin a version: <code>&amp; ([scriptblock]::Create((irm …install.ps1))) -Version 0.3.0</code>." },
  { id: "cargo", label: "cargo", cmd: "cargo install ciabatta", note: "From crates.io — needs a Rust toolchain." },
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
  { k: "needs", d: "A step runs once the steps it names have succeeded" },
  { k: "kind: push", d: "The built-in registry transfer — or your own command" },
  { k: "kind: pull", d: "The same artifact, the other way; `from` names the push" },
  { k: "on_error", d: "Route a failure to a recovery node offering a choice of fixes" },
];

const COMMANDS = [
  ["ciabatta &lt;WORKFLOW&gt;", "Run that workflow across every package that defines one, in dependency order. Add --gui for a live browser view."],
  ["ciabatta build test", "Fold several workflows into one graph, so a shared dependency runs once instead of twice."],
  ["ciabatta release --filter kind:push", "Run only part of a graph — by kind, tag, package, owner or step name."],
  ["ciabatta &lt;WORKFLOW&gt; --graph", "Draw the resolved graph and run nothing."],
  ["ciabatta list", "Every workflow in the monorepo, its owner, and what it needs."],
  ["ciabatta init --lib", "Opt a package in: a workspace identity plus a starter workflow."],
  ["ciabatta convert --script PATH", "Turn an existing script into a workflow, tools and variables included."],
  ["ciabatta configure auto", "Inspect the repo and pick publishing workflows from a checklist."],
  ["ciabatta tui", "Open the registry browser: inspect registries and explore remote paths."],
  ["ciabatta analyze", "Map the dependency graph and serve an interactive view."],
  ["ciabatta config reference", "Show the full config-format reference."],
];

const CI_MATRIX = [
  ["CIABATTA_BRANCH", "CI_COMMIT_BRANCH", "GITHUB_REF_NAME", "GIT_BRANCH"],
  ["CIABATTA_COMMIT", "CI_COMMIT_SHA", "GITHUB_SHA", "GIT_COMMIT"],
  ["CIABATTA_TAG", "CI_COMMIT_TAG", "GITHUB_REF_NAME", "TAG_NAME"],
  ["CIABATTA_BUILD_NUMBER", "CI_PIPELINE_IID", "GITHUB_RUN_NUMBER", "BUILD_NUMBER"],
];

// Annotated config example (spans classed for lightweight YAML highlighting).
const CONFIG_HTML = `<span class="c"># One YAML file describes where things go.</span>
<span class="s">system:</span>
  <span class="k">ci</span>: <span class="v">github</span>
  <span class="k">containers</span>: <span class="v">docker</span>

<span class="c"># Raw files: choose the repo and where they land inside it.</span>
<span class="s">registries:</span>
  <span class="s">nexus:</span>
    <span class="k">type</span>: <span class="v">nexus</span>
    <span class="k">url</span>: <span class="v">https://nexus.example.com</span>   <span class="c"># bare host</span>
    <span class="new">repository</span>: <span class="v">raw-hosted</span>            <span class="c"># which repo</span>
    <span class="new">format</span>: <span class="v">raw</span>                       <span class="c"># raw | npm | pypi</span>
    <span class="new">base_path</span>: <span class="v">builds</span>                 <span class="c"># prefix for raw uploads</span>

<span class="c"># Publishing is a step on a graph, not a separate schema.</span>
<span class="c"># packages/ui/.ciabatta/workflows/release.yaml</span>
<span class="s">steps:</span>
  - <span class="k">name</span>: <span class="v">build</span>
    <span class="k">run</span>: <span class="v">npm run build</span>

  - <span class="k">name</span>: <span class="v">publish</span>
    <span class="new">kind</span>: <span class="v">push</span>                       <span class="c"># the built-in transfer</span>
    <span class="new">needs</span>: <span class="v">[build]</span>                    <span class="c"># can't run before the artifact exists</span>
    <span class="k">registry</span>: <span class="v">nexus</span>
    <span class="new">artifact</span>: <span class="v">dist</span>
    <span class="k">publish_path</span>: <span class="v">"ui/{CIABATTA_BRANCH}/{CIABATTA_COMMIT}/dist.tar.gz"</span>`;

// Annotated S3 config example.
const S3_CONFIG_HTML = `<span class="c"># Point the registry at a bucket with the s3:// scheme.</span>
<span class="s">registries:</span>
  <span class="s">s3:</span>
    <span class="k">type</span>: <span class="v">s3</span>                      <span class="c"># inferred when the name contains "s3"</span>
    <span class="k">url</span>: <span class="v">s3://my-artifacts-bucket</span>

<span class="c"># publish_path becomes the key inside the bucket.</span>
<span class="s">steps:</span>
  - <span class="k">name</span>: <span class="v">publish</span>
    <span class="new">kind</span>: <span class="v">push</span>
    <span class="k">registry</span>: <span class="v">s3</span>
    <span class="new">artifact</span>: <span class="v">target/release/app</span>
    <span class="k">publish_path</span>: <span class="v">"app/{CIABATTA_BRANCH}/{CIABATTA_COMMIT}/app"</span>
<span class="c">#  → s3://my-artifacts-bucket/app/&lt;branch&gt;/&lt;commit&gt;/app</span>`;

// Annotated container (Docker / ECR) config example.
const DOCKER_CONFIG_HTML = `<span class="c"># An "ecr" registry auto-logs in via aws — no credentials in config.</span>
<span class="s">registries:</span>
  <span class="s">myecr:</span>
    <span class="k">type</span>: <span class="v">ecr</span>                     <span class="c"># inferred when the name contains "ecr"</span>
    <span class="k">url</span>: <span class="v">123456789.dkr.ecr.us-east-1.amazonaws.com</span>

  <span class="c"># A plain Docker registry logs in with CIABATTA_&lt;NAME&gt;_USER / _PASS.</span>
  <span class="s">hub:</span>
    <span class="k">type</span>: <span class="v">docker</span>
    <span class="k">url</span>: <span class="v">docker.io/myorg</span>

<span class="c"># Point a push step at a locally-built image; ciabatta retags + pushes it.</span>
<span class="s">steps:</span>
  <span class="c"># Build the image first — an ordinary step, and an edge to it.</span>
  - <span class="k">name</span>: <span class="v">image</span>
    <span class="k">run</span>: <span class="v">docker build -t app:latest .</span>

  - <span class="k">name</span>: <span class="v">publish</span>
    <span class="new">kind</span>: <span class="v">push</span>
    <span class="new">needs</span>: <span class="v">[image]</span>
    <span class="k">registry</span>: <span class="v">myecr</span>
    <span class="new">local_image</span>: <span class="v">app:latest</span>              <span class="c"># a local image (name or name:tag)</span>
    <span class="k">publish_path</span>: <span class="v">"app:{CIABATTA_COMMIT}"</span>  <span class="c"># remote ref (repo[:tag])</span>`;

// Annotated deploy flowchart example.
const DEPLOY_CONFIG_HTML = `<span class="c"># packages/web/.ciabatta/workflows/deploy.yaml</span>
<span class="c"># The filename is the workflow name. Run it with: ciabatta deploy</span>
<span class="k">description</span>: <span class="v">Migrate and release the web app</span>
<span class="new">REQUIRED_ENV</span>: <span class="v">[RUN_TOKEN]</span>            <span class="c"># refuse to start without it</span>

<span class="s">steps:</span>
  - <span class="k">name</span>: <span class="v">build</span>
    <span class="k">script</span>: <span class="v">scripts/build.sh</span>

  - <span class="k">name</span>: <span class="v">migrate</span>
    <span class="k">script</span>: <span class="v">scripts/migrate.sh</span>
    <span class="new">needs</span>: <span class="v">[build]</span>                  <span class="c"># DAG edge</span>
    <span class="new">on_error</span>: <span class="v">fix_migrate</span>          <span class="c"># if it fails, go recover</span>

  - <span class="k">name</span>: <span class="v">fix_migrate</span>              <span class="c"># a recovery node</span>
    <span class="new">recover</span>: <span class="v">true</span>
    <span class="k">retry</span>: <span class="v">migrate</span>                <span class="c"># re-run after a fix</span>
    <span class="new">options</span>:
      - <span class="k">label</span>: <span class="v">Roll back</span>
        <span class="k">script</span>: <span class="v">scripts/rollback.sh</span>
      - <span class="k">label</span>: <span class="v">Force unlock</span>
        <span class="k">run</span>: <span class="v">make unlock</span>
        <span class="k">default</span>: <span class="v">true</span>

  - <span class="k">name</span>: <span class="v">release</span>
    <span class="k">script</span>: <span class="v">scripts/release.sh</span>
    <span class="new">needs</span>: <span class="v">[migrate]</span>`;

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
        <a class="topbar__link" href="#docker">Containers</a>
        <a class="topbar__link" href="#deploy">Deploy</a>
        <a class="topbar__link" href="#commands">Commands</a>
        <a class="topbar__cta" href="${REPO}">GitHub ↗</a>
      </div>
    </header>

    <div class="wrap">
      <section class="hero">
        <div>
          <span class="hero__eyebrow">artifact publishing, made easy</span>
          <h1>One pattern for <em>everything you run.</em></h1>
          <p class="hero__lede">
            Ciabatta publishes and pulls build artifacts across Nexus, Artifactory,
            S3, Docker, and ECR — now with first-class npm and PyPI publishing
            through Nexus. One declarative graph, run in dependency order, with a live terminal UI.
          </p>
          <div class="hero__actions">
            <a class="btn btn--primary" href="#install">Get Ciabatta</a>
            <a class="btn btn--ghost" href="${REPO}">View source</a>
          </div>
          <p class="hero__hint"><b>curl -fsSL https://forsyth-creations.github.io/Ciabatta/install.sh | sh</b> · any OS, any CPU</p>
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
          <h2>Publishing is a step.</h2>
          <p class="section__sub">A step with <code>kind: push</code> moves an artifact to a registry. It sits on the graph like any other node — so it declares what it <code>needs</code>, it shows up in the dependency report, and it cannot run before the thing it publishes exists.</p>
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
          <div class="fcard"><div class="fcard__icon">🧩</div><h3>Parallel by default</h3><p>Independent branches of the graph run at once, scheduled against their real dependencies rather than a list you wrote by hand.</p></div>
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
          <p class="section__sub">Ciabatta drives the AWS CLI, so an S3 registry is just a bucket URL. Set the <code>url</code> to <code>s3://&lt;bucket&gt;</code> and a push step's <code>publish_path</code> becomes the object key.</p>
        </div>
        <div class="split">
          <pre class="code">${S3_CONFIG_HTML}</pre>
          <div class="grid" style="grid-template-columns: 1fr;">
            <div class="fcard"><div class="fcard__icon">🪣</div><h3>Bucket in, key out</h3><p>Use <code>url: s3://bucket</code>. Ciabatta joins it with <code>publish_path</code> and runs <code>aws s3 cp</code> — a <code>kind: push</code> step uploads, a <code>kind: pull</code> step downloads.</p></div>
            <div class="fcard"><div class="fcard__icon">🔑</div><h3>Standard AWS auth</h3><p>No login script needed. Credentials come from the usual chain: <code>AWS_ACCESS_KEY_ID</code> / <code>AWS_SECRET_ACCESS_KEY</code>, <code>AWS_PROFILE</code>, or an instance role.</p></div>
            <div class="fcard"><div class="fcard__icon">⚙</div><h3>Needs the AWS CLI</h3><p>Install and configure the <code>aws</code> CLI on the machine or runner. Set <code>AWS_REGION</code> if your bucket isn't in the CLI's default region.</p></div>
          </div>
        </div>
      </section>

      <section class="section reveal" id="docker">
        <div class="section__head">
          <div class="eyebrow">Configuration · Containers</div>
          <h2>Push Docker &amp; ECR images.</h2>
          <p class="section__sub">Point a push step at a <b>locally-built image</b> with <code>local_image</code>. Ciabatta retags it to the registry's target reference and pushes it — so the registry URL never has to be baked into your <code>docker build</code>.</p>
        </div>
        <div class="split">
          <pre class="code">${DOCKER_CONFIG_HTML}</pre>
          <div class="grid" style="grid-template-columns: 1fr;">
            <div class="fcard"><div class="fcard__icon">🐳</div><h3>Retag, don't rebuild</h3><p>On push, Ciabatta runs <code>docker tag</code> then <code>docker push</code> to the registry's ref. Omit <code>publish_path</code> to reuse <code>local_image</code> verbatim.</p></div>
            <div class="fcard"><div class="fcard__icon">🔐</div><h3>ECR logs in for you</h3><p>An <code>ecr</code> registry authenticates automatically with <code>aws ecr get-login-password</code> — no credentials in your config. Plain Docker registries use <code>CIABATTA_&lt;NAME&gt;_USER</code> / <code>_PASS</code>.</p></div>
            <div class="fcard"><div class="fcard__icon">🐋</div><h3>Docker or podman</h3><p>Set <code>containers</code> under <code>system:</code>, or let Ciabatta auto-detect what's installed. The same step works with either engine.</p></div>
            <div class="fcard"><div class="fcard__icon">↩</div><h3>Pull retags back</h3><p>A <code>kind: pull</code> step pulls the remote reference and retags it back to <code>local_image</code>, so the image lands locally under the name you started with.</p></div>
          </div>
        </div>
      </section>

      <section class="section reveal" id="deploy">
        <div class="section__head">
          <div class="eyebrow">Configuration · Deploy</div>
          <h2>Deploys: a flowchart of scripts that heals itself.</h2>
          <p class="section__sub">A workflow is a DAG of dependent steps, one file per name in <code>.ciabatta/workflows/</code>. Every package that declares that name joins in, so <code>ciabatta deploy</code> compiles the whole monorepo's deploy into a single graph and runs it in order.</p>
        </div>
        <div class="split">
          <pre class="code">${DEPLOY_CONFIG_HTML}</pre>
          <div class="grid" style="grid-template-columns: 1fr;">
            <div class="fcard"><div class="fcard__icon">🔀</div><h3>Dependent steps, in order</h3><p>Declare <code>needs = ["build"]</code> and Ciabatta runs steps once their dependencies succeed. The graph is validated up front — missing edges and cycles fail before anything runs.</p></div>
            <div class="fcard"><div class="fcard__icon">🩹</div><h3>“If error” recovery branches</h3><p>A step's <code>on_error</code> jumps to a recovery node that offers a choice of fix scripts. Pick one, and <code>retry</code> re-runs the failed step. In CI, the <code>default</code> option self-heals unattended.</p></div>
            <div class="fcard"><div class="fcard__icon">🖥</div><h3>Debug it in the browser</h3><p><code>ciabatta deploy web --gui</code> opens a live view: the flowchart lights up per step, logs stream in, and recovery nodes show fix-it buttons you click to resolve.</p></div>
            <div class="fcard"><div class="fcard__icon">🧰</div><h3>Run part of a graph</h3><p><code>--filter kind:push</code>, <code>--filter tag:fast</code>, <code>--filter workspace:api</code>. Filtering prunes the compiled graph, so what survives is a real subgraph — the fast debug loop.</p></div>
          </div>
        </div>
        <figure class="shot" style="margin-top:26px;">
          <img src="deploy-gui-recovery.png" alt="The ciabatta deploy --gui live view: a failed migrate step routed to a recovery node with fix-it buttons." loading="lazy" />
          <figcaption><code>ciabatta deploy web --gui</code> — a failed step routes to a recovery node, and the browser shows the fix-it buttons you click to resolve it live.</figcaption>
        </figure>
        <div style="margin-top:26px;">
          <div class="deepdive">
            <a class="deepdive__card" href="deploy-env-files.html">
              <h4>📄 Sourcing .env files</h4>
              <p>Load <code>KEY=VALUE</code> files into a deploy with <code>env_file</code> — one file or a list, with clear precedence.</p>
              <span class="go">Read the guide →</span>
            </a>
            <a class="deepdive__card" href="deploy-env-select.html">
              <h4>🔀 Per-environment selection</h4>
              <p>Pick the env file at run time with a <code>{VAR}</code> placeholder: <code>.env.{DEPLOY_ENV}</code> → dev or prod.</p>
              <span class="go">Read the guide →</span>
            </a>
            <a class="deepdive__card" href="deploy-conditional-steps.html">
              <h4>⛔ Conditional steps</h4>
              <p>Skip steps by condition with <code>when</code> / <code>skip_if</code> — single or multiple criteria.</p>
              <span class="go">Read the guide →</span>
            </a>
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
          <div class="fcard"><div class="fcard__icon">🍞</div><h3>Managed vs. inferred</h3><p>Publish points from a Ciabatta push step are flagged 🍞, apart from ones inferred from your <code>.sh</code> scripts.</p></div>
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

// ── The signature terminal: a replay of `ciabatta release` ───────────────────
function terminalHTML() {
  const recipes = [
    { name: "proto", stages: ["generate", "build", "test", "publish"] },
    { name: "api", stages: ["build", "test", "package", "publish"] },
    { name: "web", stages: ["build", "test", "bundle", "publish"] },
  ];
  return `
    <div class="term" id="term" aria-label="Terminal replay of ciabatta release">
      <div class="term__bar">
        <span class="term__dot term__dot--r"></span>
        <span class="term__dot term__dot--y"></span>
        <span class="term__dot term__dot--g"></span>
        <span class="term__title">ciabatta release</span>
      </div>
      <div class="term__body">
        <div class="term__prompt"><span class="sigil">$</span> <span class="cmd">ciabatta release</span></div>
        <div class="term__caption"><span class="loaf">🍞</span> 3 packages, in dependency order</div>
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
        <div class="term__done" id="term-done">✓ done · 3 packages in 2.4s</div>
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

  // Each package advances step by step; the bar fills as steps complete.
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
