<div align="center" style="margin: 24px 0;">
  <a href="https://forsyth-creations.github.io/Ciabatta/" style="display: inline-block; padding: 12px 28px; background: linear-gradient(135deg, #d97742, #b5562b); color: #fff; font-family: 'Segoe UI', sans-serif; font-size: 18px; font-weight: 600; text-decoration: none; border-radius: 8px; box-shadow: 0 4px 10px rgba(0,0,0,0.15);">
    🍞 Ciabatta
  </a>
</div>

**Artifact publishing made easy.**

Ciabatta is a fast, cross-platform CLI for publishing and pulling build
artifacts to and from common registries — Nexus, S3, Artifactory, Docker, and
ECR — driven by a single declarative TOML file. It picks up branch / commit /
tag / build-number metadata from whatever CI system you run on, runs multiple
publish jobs in parallel, and shows progress in a friendly terminal UI.

```
   _____ _       _           _   _
  / ____(_)     | |         | | | |
 | |     _  __ _| |__   __ _| |_| |_ __ _
 | |    | |/ _` | '_ \ / _` | __| __/ _` |
 | |____| | (_| | |_) | (_| | |_| || (_| |
  \_____|_|\__,_|_.__/ \__,_|\__|\__\__,_|
```

## Why Ciabatta

- **One config, many registries.** Describe your registries and publish
  "recipes" once in `.ciabatta/ciabatta.toml`; run any combination of them with
  a single command.
- **CI-aware.** Automatically resolves `CIABATTA_BRANCH`, `CIABATTA_COMMIT`,
  `CIABATTA_TAG`, and `CIABATTA_BUILD_NUMBER` from GitLab, GitHub Actions,
  Jenkins, CircleCI, Azure DevOps, or Bitbucket — and lets you template them
  into publish paths.
- **Parallel with live progress.** Run several recipes at once and watch each
  one in a `ratatui`-powered TUI (or `--no-tui` for plain CI logs).
- **Push *and* pull.** Because Ciabatta knows where things live, it can fetch
  artifacts back down, not just upload them.
- **Bring your own auth.** Login is handled by your own scripts — Ciabatta just
  makes the resolved variables available to them as environment variables.
- **Truly drop-in.** Linux builds are statically linked (musl), so there is no
  glibc version requirement: download, extract, run, on any distro.

## Installation

### From crates.io

```bash
cargo install ciabatta
```

### Pre-built binaries

Download the archive for your platform from the
[latest release](https://github.com/forsyth-creations/ciabatta/releases/latest)
and move the binary onto your `PATH`:

```bash
tar xzf ciabatta-linux-x86_64.tar.gz && sudo mv ciabatta /usr/local/bin/
```

Builds are published for Linux (x86_64 / aarch64, static), macOS
(x86_64 / aarch64), and Windows (x86_64).

## Quick start

```bash
# 1. Scaffold a .ciabatta/ directory with a starter config
ciabatta init --ci github

# 2. See what recipes are available
ciabatta list

# 3. Dry-run to see exactly what would happen
ciabatta run release_frontend --dry-run

# 4. Publish for real (runs multiple recipes in parallel)
ciabatta run release_frontend release_backend

# 5. Pull an artifact back down
ciabatta pull release_frontend
```

Ciabatta discovers your project by walking up to find the `.ciabatta/`
directory; the directory **above** it is treated as the project root that
artifacts are published from.

## Commands

| Command | What it does |
| --- | --- |
| `ciabatta run [RECIPE...]` | Push one or more recipes in parallel (all if none named). |
| `ciabatta pull [RECIPE...]` | Download artifacts for one or more recipes. |
| `ciabatta list` | List all recipes defined in the config. |
| `ciabatta init [--ci SYSTEM]` | Create a `.ciabatta/` directory with a starter `ciabatta.toml`. |
| `ciabatta configure` | Interactively add a registry (and optionally a recipe) — no hand-editing TOML. |
| `ciabatta configure auto` | Analyze the project and pick recipes from an interactive checklist (Docker → ECR/Nexus, Rust binaries → crates.io / S3 / Nexus). |
| `ciabatta tui` (alias `browse`) | Interactive browser — inspect registries, check paths, push on demand. |
| `ciabatta analyze` | Build the project's dependency graph and serve an interactive view. |
| `ciabatta config show` | Print the resolved configuration. |
| `ciabatta config reference` | Show documentation on the config format and options. |

Useful flags on `run` / `pull`:

- `-e, --env KEY=VALUE` — set a variable. **Command-line values always override
  CI-derived ones.** Repeatable.
- `--dry-run` — show what would happen without publishing or fetching.
- `--no-tui` — disable the TUI and stream plain progress to stdout (ideal for CI).
- `-c, --config PATH` — use a specific config file instead of discovery.

Global flags (any command):

- `--debug` — enable debug logging to stderr. You can also set `CIABATTA_DEBUG=1`,
  or `CIABATTA_LOG=ciabatta=trace` for finer control.

When a recipe's `local_artifact_path` is a **directory**, Ciabatta uploads each
file in it individually, recreating the folder structure under the recipe's
`publish_path` (creating sub-folders in the registry as needed) — so
`local_artifact_path = "frontend/dist"` publishes the whole `dist` tree.

In the `ciabatta tui` browser, press `e` on a registry to **explore** its remote
contents — navigate folders and see which artifacts already exist, which is handy
when deciding on a `publish_path`.

## Configuration

Ciabatta reads `.ciabatta/ciabatta.toml`. Registries describe *where* things go;
recipes describe *what* to publish and *how*.

The fastest way to start is `ciabatta configure` (add a registry interactively)
or `ciabatta configure auto` (let Ciabatta inspect the repo and suggest recipes).
You can also edit the file by hand:

```toml
[system]
ci = "github"          # gitlab | github | jenkins | circleci | azure | bitbucket
containers = "docker"  # docker | podman — when omitted, Ciabatta auto-detects what
                       # is installed (prefers podman, then docker; asks you to
                       # choose if both are present).

[registries.nexus]
# url and login_script expand environment variables with bash-style defaults,
# so one config can target different environments.
url = "https://${NEXUS_HOST:-nexus.example.com}/repository/maven-repository/"
tls_verify = true
needs_auth = true
login_script = "./nexus_login.sh"

[registries.s3]
url = "https://s3.example.com/"
tls_verify = true

# A simple recipe: copy a local artifact to a templated publish path.
[recipies.release_frontend]
registry = "nexus"
local_artifact_path = "frontend/dist"
publish_path = "frontend/{CIABATTA_BRANCH}/{CIABATTA_COMMIT}/frontend"

# A scripted recipe: run your own push/pull scripts with the variables injected.
[recipies.release_backend.push]
bash_script = "scripts/release_backend.sh"

[recipies.release_backend.pull]
bash_script = "scripts/pull_backend.sh"
```

A few rules worth knowing:

- If a `publish_path` references a variable that isn't set, Ciabatta **errors
  immediately** rather than publishing to a half-resolved path.
- Stage commands, login scripts, and bash recipes all receive every resolved
  `CIABATTA_*` variable (plus anything you pass with `-e`) in their environment.

Run `ciabatta config reference` for the full, always-up-to-date field listing.

## Stages

Every direction runs as a four-stage state machine, and the TUI shows progress
through each stage live:

```
Push:  login → pre-push → push → post-push
Pull:  login → pre-pull → pull → post-pull
```

Override any stage with an arbitrary command — bash, python, a compiled binary,
anything runnable. Unset stages fall back to their defaults (login authenticates,
`pre`/`post` do nothing, `main` runs the built-in registry action). Each command
runs via `sh -c` from the project root with all `CIABATTA_*` vars in its
environment.

```toml
[recipies.frontend]
registry = "nexus"
local_artifact_path = "frontend/dist"
publish_path = "front/{CIABATTA_COMMIT}/dist"

  # Overrides for the push direction only:
  [recipies.frontend.push]
  pre  = "python scripts/bundle.py"     # pre-push
  post = "./scripts/notify.sh deployed" # post-push
  # login + push (main) use their defaults

  [recipies.frontend.pull]
  post = "echo pulled $CIABATTA_COMMIT"
```

| Stage | Override key | Default |
| --- | --- | --- |
| login | `login` | registry `login_script`, or `CIABATTA_<REGISTRY>_USER`/`_PASS` credentials |
| pre-push / pre-pull | `pre` | nothing |
| push / pull | `main` | built-in registry action (or legacy `bash_script`) |
| post-push / post-pull | `post` | nothing |

## Credentials

When a registry has **no** `login_script` and no `login` override, Ciabatta reads
per-registry credentials from the environment:

```
CIABATTA_<REGISTRY>_USER    CIABATTA_<REGISTRY>_PASS
```

`<REGISTRY>` is the registry's section name, uppercased — so `[registries.nexus]`
uses `CIABATTA_NEXUS_USER` / `CIABATTA_NEXUS_PASS`. They're applied per type:

- **Nexus / Artifactory** — sent as HTTP basic auth on the upload/download.
- **Docker** — `docker login <host> -u $USER --password-stdin`.
- **ECR** — auto-login via `aws ecr get-login-password` (no credentials needed).
- **S3** — uses the standard AWS credential chain (`AWS_ACCESS_KEY_ID`, …).

## Analyze

`ciabatta analyze` maps how your repository is wired together and serves an
interactive dependency graph laid out in columns:

```
[requirements] →  dependencies   →   internal packages   →   publish points
 (optional)       (crates.io,         (your crates,            (crates.io, plus
                   npm, pip,           npm/python packages,      ciabatta-managed
                   dockerhub)          workspaces, modules)      registries)
```

It scans `Cargo.toml`, `package.json`, `requirements.txt` / `pyproject.toml`,
`Dockerfile`s, and `.gitlab-ci.yml` (its `image:` / `services:` container
images) for external dependencies, identifies the internal packages in the repo,
and derives publish points from your ciabatta recipes (and a publishable crate →
crates.io). The result is written as JSON and served at `http://127.0.0.1:8080`,
where you can click any node for details.

**Publish scripts.** Developers often publish from shell scripts, so `analyze`
also reads `.sh` files (anywhere in the tree, plus `.ciabatta/` and any script
referenced by your config) and turns registry-push commands into publish points:
`docker`/`podman push`, `aws s3 cp`/`sync`, `cargo publish`, `npm`/`yarn
publish`, `twine upload`, `helm push`, and `curl` uploads (`-T` / `--upload-file`
/ `PUT`). Each is wired to the package that owns the script, and — unlike a
ciabatta recipe — is **not** flagged as ciabatta-managed.

```bash
ciabatta analyze                 # write JSON + open the live view on :8080
ciabatta analyze --port 9000     # use a different port
ciabatta analyze --no-serve      # just write ciabatta-analyze.json
ciabatta analyze --check-vulns   # also query OSV for known vulnerabilities
ciabatta analyze --requirements reqs.txt --trace trace.csv   # requirements column
```

**Workspaces.** A `Cargo.toml` `[workspace]`, a `package.json` `workspaces`
field, or a `pyproject.toml` `[tool.uv.workspace]` is detected as a workspace:
its members are linked to the root and tagged so you can filter by workspace.

**File data.** Every scanned file is tracked with its kind, ecosystem, size,
content hash, owning package, and any declared workspace members — browse them
with the **Files** button in the view.

**Filtering.** The view has live filters for name search, category, ecosystem,
and workspace, so you can focus on one corner of a large graph.

**Managed publish points.** Publish points that come from a ciabatta recipe are
flagged **🍞 managed by ciabatta**, distinguishing them from inferred ones like
crates.io.

**Requirements & traceability.** Point `analyze` at a *requirements file* (one
requirement per line, `id` or `id, description`) to add a leftmost
**Requirements** column. A *trace file* — a CSV of `requirement,file`
connections — wires each requirement to the internal package that owns the
traced file(s), threading requirements through to the rest of the graph. Both
can be set on the command line or in config:

```toml
[analyze]
requirements = "docs/requirements.txt"
trace = "docs/trace.csv"
```

The web view is fully self-contained (no external assets or network needed,
unless you pass `--check-vulns`).

Scanned files are content-hashed into `.ciabatta/.cache/analyze.json`, so
re-running `analyze` only re-parses the manifests that actually changed (it
reports e.g. `cache: 4 reused, 1 parsed`).

## CI variables

On a supported CI system Ciabatta resolves these and prints them at startup:

| Variable | Meaning |
| --- | --- |
| `CIABATTA_BRANCH` | Current branch |
| `CIABATTA_COMMIT` | Commit SHA |
| `CIABATTA_TAG` | Tag, if the build is tagged |
| `CIABATTA_BUILD_NUMBER` | CI build/run number |

Pass any of them explicitly with `-e CIABATTA_BRANCH=main` to override what was
detected — handy for local runs.

## Web frontend

Ciabatta ships a small Vite-built site (hosted on GitHub Pages) with download
links and usage instructions. See the
[project site](https://forsyth-creations.github.io/Ciabatta/).

## License

Licensed under the [MIT License](LICENSE).
