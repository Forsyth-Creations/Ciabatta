const VERSION = "0.1.0";

const PLATFORMS = [
  {
    label: "Linux x86_64",
    file: `ciabatta-linux-x86_64.tar.gz`,
    icon: "🐧",
    install: "tar xzf ciabatta-linux-x86_64.tar.gz && sudo mv ciabatta /usr/local/bin/",
  },
  {
    label: "Linux ARM64",
    file: `ciabatta-linux-aarch64.tar.gz`,
    icon: "🐧",
    install: "tar xzf ciabatta-linux-aarch64.tar.gz && sudo mv ciabatta /usr/local/bin/",
  },
  {
    label: "macOS x86_64",
    file: `ciabatta-macos-x86_64.tar.gz`,
    icon: "🍎",
    install: "tar xzf ciabatta-macos-x86_64.tar.gz && sudo mv ciabatta /usr/local/bin/",
  },
  {
    label: "macOS ARM64 (Apple Silicon)",
    file: `ciabatta-macos-aarch64.tar.gz`,
    icon: "🍎",
    install: "tar xzf ciabatta-macos-aarch64.tar.gz && sudo mv ciabatta /usr/local/bin/",
  },
  {
    label: "Windows x86_64",
    file: `ciabatta-windows-x86_64.zip`,
    icon: "🪟",
    install: "Unzip and place ciabatta.exe on your PATH.",
  },
];

const EXAMPLE_CONFIG = `[system]
ci         = "github"
containers = "docker"

[registries.nexus]
url          = "https://nexus.example.com/repository/releases/"
tls_verify   = true
needs_auth   = true
login_script = ".ciabatta/nexus_login.sh"

[registries.ecr]
url        = "123456789.dkr.ecr.us-east-1.amazonaws.com"
needs_auth = false

[recipies.frontend]
registry            = "nexus"
local_artifact_path = "frontend/dist"
publish_path        = "ui/{CIABATTA_BRANCH}/{CIABATTA_COMMIT}/dist.tar.gz"

[recipies.api.push]
bash_script = "scripts/build_push.sh"
[recipies.api.pull]
bash_script = "scripts/pull.sh"`;

function getReleaseBase() {
  // In a real deploy, point at GitHub releases.
  return `https://github.com/YOUR_USERNAME/ciabatta/releases/download/v${VERSION}`;
}

function render() {
  const app = document.getElementById("app");
  const releaseBase = getReleaseBase();

  app.innerHTML = `
    <header>
      <div class="logo">🍞</div>
      <h1>Ciabatta</h1>
      <p class="tagline">Artifact Publishing Made Easy</p>
      <p class="version">v${VERSION}</p>
    </header>

    <section class="hero">
      <p>
        Ciabatta is a fast, cross-platform CLI for publishing and pulling artifacts
        to Nexus, S3, Artifactory, Docker registries, and ECR — with a beautiful
        real-time TUI, parallel recipe execution, and first-class CI/CD integration.
      </p>
    </section>

    <section>
      <h2>⬇ Download</h2>
      <div class="platforms">
        ${PLATFORMS.map(
          (p) => `
          <div class="platform-card">
            <div class="platform-icon">${p.icon}</div>
            <div class="platform-label">${p.label}</div>
            <a class="btn" href="${releaseBase}/${p.file}" download>${p.file}</a>
            <code class="install-cmd">${p.install}</code>
          </div>
        `
        ).join("")}
      </div>
      <p style="margin-top:1rem;color:#888;">
        Or install from source:
        <code>cargo install --git https://github.com/YOUR_USERNAME/ciabatta</code>
      </p>
    </section>

    <section>
      <h2>🚀 Quick Start</h2>
      <ol>
        <li>Create a <code>.ciabatta/</code> directory in your project root.</li>
        <li>Add a <code>.ciabatta/ciabatta.toml</code> config file (see below).</li>
        <li>Run <code>ciabatta run frontend</code> to publish your artifacts.</li>
      </ol>
      <pre>${EXAMPLE_CONFIG}</pre>
    </section>

    <section>
      <h2>📖 Commands</h2>
      <table>
        <thead><tr><th>Command</th><th>Description</th></tr></thead>
        <tbody>
          <tr><td><code>ciabatta run [RECIPES]</code></td><td>Push artifacts (all recipes if none specified)</td></tr>
          <tr><td><code>ciabatta pull [RECIPES]</code></td><td>Pull/download artifacts</td></tr>
          <tr><td><code>ciabatta list</code></td><td>List available recipes</td></tr>
          <tr><td><code>ciabatta config show</code></td><td>Show resolved configuration</td></tr>
          <tr><td><code>ciabatta config help</code></td><td>Show config file reference</td></tr>
        </tbody>
      </table>
      <h3>Common flags</h3>
      <table>
        <thead><tr><th>Flag</th><th>Description</th></tr></thead>
        <tbody>
          <tr><td><code>-e KEY=VALUE</code></td><td>Set / override an environment variable</td></tr>
          <tr><td><code>--dry-run</code></td><td>Show what would happen without doing it</td></tr>
          <tr><td><code>--no-tui</code></td><td>Plain text output instead of the TUI</td></tr>
        </tbody>
      </table>
    </section>

    <section>
      <h2>🔌 Supported Registries</h2>
      <table>
        <thead><tr><th>Type</th><th>Push</th><th>Pull</th><th>Notes</th></tr></thead>
        <tbody>
          <tr><td>Nexus</td><td>✅</td><td>✅</td><td>HTTP PUT / GET</td></tr>
          <tr><td>Artifactory</td><td>✅</td><td>✅</td><td>HTTP PUT / GET</td></tr>
          <tr><td>S3</td><td>✅</td><td>✅</td><td>Requires <code>aws</code> CLI</td></tr>
          <tr><td>Docker</td><td>✅</td><td>✅</td><td>Requires <code>docker</code> or <code>podman</code></td></tr>
          <tr><td>ECR</td><td>✅</td><td>✅</td><td>Auto-fetches ECR login token</td></tr>
        </tbody>
      </table>
    </section>

    <section>
      <h2>🤖 CI/CD Integration</h2>
      <p>
        Set <code>ci = "..."</code> in <code>[system]</code> and Ciabatta will
        automatically resolve the following variables from your CI environment:
      </p>
      <table>
        <thead><tr><th>Variable</th><th>GitLab CI</th><th>GitHub Actions</th><th>Jenkins</th></tr></thead>
        <tbody>
          <tr>
            <td><code>CIABATTA_BRANCH</code></td>
            <td><code>CI_COMMIT_BRANCH</code></td>
            <td><code>GITHUB_REF_NAME</code></td>
            <td><code>GIT_BRANCH</code></td>
          </tr>
          <tr>
            <td><code>CIABATTA_COMMIT</code></td>
            <td><code>CI_COMMIT_SHA</code></td>
            <td><code>GITHUB_SHA</code></td>
            <td><code>GIT_COMMIT</code></td>
          </tr>
          <tr>
            <td><code>CIABATTA_TAG</code></td>
            <td><code>CI_COMMIT_TAG</code></td>
            <td><code>GITHUB_REF_NAME</code></td>
            <td><code>TAG_NAME</code></td>
          </tr>
          <tr>
            <td><code>CIABATTA_BUILD_NUMBER</code></td>
            <td><code>CI_PIPELINE_IID</code></td>
            <td><code>GITHUB_RUN_NUMBER</code></td>
            <td><code>BUILD_NUMBER</code></td>
          </tr>
        </tbody>
      </table>
      <p>Also supported: CircleCI, Travis CI, Azure DevOps, Bitbucket Pipelines.</p>
    </section>

    <footer>
      <p>
        <a href="https://github.com/YOUR_USERNAME/ciabatta">GitHub</a> ·
        MIT License
      </p>
    </footer>
  `;
}

const styles = `
  * { box-sizing: border-box; margin: 0; padding: 0; }

  body {
    font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, sans-serif;
    background: #0d1117;
    color: #e6edf3;
    line-height: 1.6;
  }

  header {
    text-align: center;
    padding: 4rem 1rem 2rem;
    background: linear-gradient(180deg, #1a2236 0%, #0d1117 100%);
    border-bottom: 1px solid #21262d;
  }

  .logo { font-size: 4rem; margin-bottom: 0.5rem; }

  h1 {
    font-size: 3rem;
    font-weight: 800;
    background: linear-gradient(135deg, #f0b429, #e67e22);
    -webkit-background-clip: text;
    -webkit-text-fill-color: transparent;
  }

  .tagline { color: #8b949e; font-size: 1.2rem; margin-top: 0.5rem; }
  .version { color: #484f58; font-size: 0.9rem; margin-top: 0.25rem; }

  .hero {
    max-width: 700px;
    margin: 2rem auto;
    padding: 0 1.5rem;
    font-size: 1.1rem;
    color: #8b949e;
    text-align: center;
  }

  section {
    max-width: 900px;
    margin: 3rem auto;
    padding: 0 1.5rem;
  }

  h2 {
    font-size: 1.6rem;
    margin-bottom: 1.25rem;
    color: #f0b429;
    border-bottom: 1px solid #21262d;
    padding-bottom: 0.5rem;
  }

  h3 { font-size: 1.1rem; margin: 1.5rem 0 0.75rem; color: #8b949e; }

  ol { padding-left: 1.5rem; }
  ol li { margin-bottom: 0.5rem; }

  pre {
    background: #161b22;
    border: 1px solid #30363d;
    border-radius: 8px;
    padding: 1.25rem;
    overflow-x: auto;
    font-size: 0.875rem;
    color: #e6edf3;
    line-height: 1.5;
    white-space: pre;
  }

  code {
    font-family: "SFMono-Regular", Consolas, "Liberation Mono", Menlo, monospace;
    font-size: 0.875em;
    background: #161b22;
    border: 1px solid #30363d;
    border-radius: 4px;
    padding: 0.15em 0.4em;
    color: #e6edf3;
  }

  pre code { background: none; border: none; padding: 0; }

  table {
    width: 100%;
    border-collapse: collapse;
    margin-top: 0.5rem;
    font-size: 0.9rem;
  }

  th, td {
    text-align: left;
    padding: 0.6rem 0.75rem;
    border-bottom: 1px solid #21262d;
  }

  th { color: #8b949e; font-weight: 600; background: #161b22; }
  tr:hover td { background: #161b22; }

  .platforms {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(220px, 1fr));
    gap: 1rem;
  }

  .platform-card {
    background: #161b22;
    border: 1px solid #30363d;
    border-radius: 8px;
    padding: 1.25rem;
    display: flex;
    flex-direction: column;
    gap: 0.6rem;
  }

  .platform-icon { font-size: 1.75rem; }
  .platform-label { font-weight: 600; color: #e6edf3; }

  .btn {
    display: inline-block;
    background: #f0b429;
    color: #0d1117;
    font-weight: 700;
    padding: 0.5rem 0.75rem;
    border-radius: 6px;
    text-decoration: none;
    font-size: 0.8rem;
    word-break: break-all;
    text-align: center;
  }

  .btn:hover { background: #e67e22; }

  .install-cmd {
    font-size: 0.75rem;
    color: #8b949e;
    word-break: break-all;
    background: none;
    border: none;
    padding: 0;
  }

  footer {
    text-align: center;
    padding: 2rem;
    border-top: 1px solid #21262d;
    color: #484f58;
    font-size: 0.9rem;
    margin-top: 4rem;
  }

  footer a { color: #8b949e; text-decoration: none; }
  footer a:hover { color: #e6edf3; }

  a { color: #f0b429; }
`;

const styleEl = document.createElement("style");
styleEl.textContent = styles;
document.head.appendChild(styleEl);

render();
