<div align="center" style="margin: 24px 0;">
  <a href="https://forsyth-creations.github.io/Ciabatta/" style="display: inline-block; padding: 12px 28px; background: linear-gradient(135deg, #d97742, #b5562b); color: #fff; font-family: 'Segoe UI', sans-serif; font-size: 18px; font-weight: 600; text-decoration: none; border-radius: 8px; box-shadow: 0 4px 10px rgba(0,0,0,0.15);">
    🍞 Ciabatta
  </a>
</div>

**Monorepo orchestration and artifact publishing.**

Ciabatta is a fast, cross-platform CLI that does two jobs, from one declarative
TOML file.

It **orchestrates a monorepo**: every package opts in with `ciabatta init --lib`
and declares who owns it, what its workflows do, and which other packages they
need. `ciabatta build` then resolves *one* graph across the whole repo and shows
you exactly what will run, in what order, and where each node came from.

And it **publishes artifacts** to common registries — Nexus (raw, **npm**, and
**PyPI**), S3, Artifactory, Docker, and ECR. It picks up branch / commit / tag /
build-number metadata from whatever CI system you run on, runs multiple publish
jobs in parallel, and shows progress in a friendly terminal UI. Publishing is
also just a step on the graph, so a build that ends in a push is one command.

```
   _____ _       _           _   _
  / ____(_)     | |         | | | |
 | |     _  __ _| |__   __ _| |_| |_ __ _
 | |    | |/ _` | '_ \ / _` | __| __/ _` |
 | |____| | (_| | |_) | (_| | |_| || (_| |
  \_____|_|\__,_|_.__/ \__,_|\__|\__\__,_|
```

## Why Ciabatta

- **Every script is discoverable, described, and owned.** `ciabatta list` shows
  every workflow in the monorepo, what it does, and who to ask — so nobody has
  to open six packages to find out what `build.sh` is for.
- **Cross-package dependencies are declared, not remembered.** A package that
  needs another's generated protobufs says so once; ciabatta runs them in the
  right order, every time, and *draws you the graph* before it starts.
- **Missing toolchains are named up front.** Declare what a step needs, and a
  missing `protoc` is reported before the build starts, with the install command
  your repo wrote down — not as "command not found" ten minutes in.
- **Nothing hangs the graph.** Long-running steps get a `timeout`; genuinely
  persistent ones (dev servers, watchers) are handed to the daemon and stepped
  over, so they keep running after the build that started them is done.
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
[latest release](https://github.com/forsyth-creations/ciabatta/releases/latest).
Each archive includes an install script that puts the binary on your `PATH`.

**Linux / macOS**
```bash
tar xzf ciabatta-linux-x86_64.tar.gz
./install.sh                  # installs to /usr/local/bin (uses sudo if needed)
./install.sh ~/.local/bin     # or pick your own directory
```

**Windows** (PowerShell)
```powershell
Expand-Archive ciabatta-windows-x86_64.zip
.\install.ps1                 # installs to %LOCALAPPDATA%\Programs\ciabatta and adds it to your PATH
.\install.ps1 -InstallDir C:\tools\ciabatta   # or pick your own directory
```

Builds are published for Linux (x86_64 / aarch64, static), macOS
(x86_64 / aarch64), and Windows (x86_64).

## Quick start

**A monorepo:**

```bash
# 1. In each package, opt in
cd packages/proto && ciabatta init --lib --owner "Grace" \
    --description "Shared protobuf definitions" --workflow generate
cd ../api      && ciabatta init --lib --owner "Ada" \
    --description "Public REST API" --depends-on proto:generate

# 2. See everything the repo can do, and who owns it
ciabatta list -v
ciabatta list -s proto        # ...or search

# 3. See the graph before you run it
ciabatta build --graph

# 4. Walk every step without side effects
ciabatta build --dry-run

# 5. Run it, from anywhere in the repo
ciabatta build
```

**A single project publishing artifacts:**

```bash
# 1. Scaffold a .ciabatta/ directory with a starter config
ciabatta init --ci github

# 2. See what recipes are available
ciabatta list

# 3. Dry-run to see exactly what would happen
ciabatta push release_frontend --dry-run

# 4. Publish for real (pushes multiple recipes in parallel)
ciabatta push release_frontend release_backend

# 5. Pull an artifact back down
ciabatta pull release_frontend
```

Ciabatta discovers your project by walking up to find the `.ciabatta/`
directory; the directory **above** it is treated as the project root that
artifacts are published from. For workflows it goes one level further out: the
**monorepo root** is your git root, and every directory beneath it with a
`.ciabatta/ciabatta.toml` is a sub-workspace.

## Commands

| Command | What it does |
| --- | --- |
| `ciabatta <WORKFLOW>` | Run that workflow across every sub-workspace that defines one, in dependency order. `ciabatta build`, `ciabatta test`, … |
| `ciabatta workflow <NAME>` | The same thing, spelled out — for a workflow whose name collides with a command below. |
| `ciabatta push [RECIPE...]` | Push one or more recipes in parallel (all if none named). |
| `ciabatta pull [RECIPE...]` | Download artifacts for one or more recipes. |
| `ciabatta run [RECIPE...]` | Execute a single project's run: a DAG of dependent script steps with error-recovery branches. `--gui` for a live browser view, `--build` for a visual flowchart editor. |
| `ciabatta list` | Every workflow in the monorepo — with descriptions, owners and dependencies — then this project's recipes. `-s TERM` to search, `-v` for steps. |
| `ciabatta init --lib` | Opt this package in as a sub-workspace: a `[workspace]` identity plus a starter workflow. |
| `ciabatta init [--ci SYSTEM]` | Create a `.ciabatta/` directory with a starter publishing `ciabatta.toml`. |
| `ciabatta configure` | Interactively add a registry (and optionally a recipe) — no hand-editing TOML. |
| `ciabatta configure auto` | Analyze the project and pick recipes from an interactive checklist (Docker → ECR/Nexus, Rust binaries → crates.io / S3 / Nexus). |
| `ciabatta tui` (alias `browse`) | Interactive browser — inspect registries, check paths, push on demand. |
| `ciabatta analyze` | Build the project's dependency graph and open an interactive view. |
| `ciabatta watch <command>` | Run a command and stream its logs into a live, searchable web view with bookmarks and notification triggers. `--list` to see running sessions, `--attach <ID>` to tail one, `--stop <ID>` to end it. |
| `ciabatta todo [TASK]` | Personal task list. With a TASK, adds it and exits; without, opens the todo page. |
| `ciabatta ai` | AI assistant — chat TUI plus a live architecture mind map. |
| `ciabatta daemon <status\|stop\|restart\|logs>` | Inspect or control the background daemon. |
| `ciabatta config show` | Print the resolved configuration. |
| `ciabatta config reference` | Show documentation on the config format and options. |

Useful flags on `push` / `pull`:

- `-e, --env KEY=VALUE` — set a variable. **Command-line values always override
  CI-derived ones.** Repeatable.
- `--dry-run` — show what would happen without publishing or fetching.
- `--no-tui` — disable the TUI and stream plain progress to stdout (ideal for CI).
- `-c, --config PATH` — use a specific config file instead of discovery.

Useful flags on a workflow (`ciabatta build`, …):

- `--graph` — print the graph and stop. Nothing runs.
- `--dry-run` — walk every step, executing none of them.
- `--only MEMBER` — start from one sub-workspace. Its dependencies still come
  along, so the result stays correct. Repeatable.
- `--isolated` — don't follow dependencies out of what you selected, for when
  everything upstream is already built.
- `-e, --env KEY=VALUE` — set a variable for every step. Repeatable.
- `--gui` — watch the graph run live in a browser instead of the terminal.

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

## Monorepos

A monorepo accumulates scripts nobody owns, publishing to places nobody
remembers, quietly depending on each other in ways nobody wrote down. Ciabatta's
answer is that each package declares three things — **who owns it**, **what its
workflows do**, and **which other packages they need** — and everything else
follows from that.

### Opting a package in

```bash
cd packages/api
ciabatta init --lib --owner "Ada" --description "Public REST API" \
              --depends-on proto:generate
```

That writes a `[workspace]` identity into `packages/api/.ciabatta/ciabatta.toml`:

```toml
[workspace]
name        = "api"
description = "Public REST API"
owner       = "Ada"
depends_on  = ["proto:generate"]   # what this package needs first
tags        = ["backend"]
requires    = ["cargo"]            # tools every workflow here needs on PATH
```

`description` and `owner` aren't decoration — they're what `ciabatta list` shows
everyone else, so `init --lib` defaults the owner to your git user and nags you
if either is blank.

### Workflows

One file per workflow, in `.ciabatta/workflows/`. The **filename is the
workflow name**, and every package that defines a workflow of that name joins
the same graph.

```toml
# packages/api/.ciabatta/workflows/build.toml
description  = "Compile the API service"
owner        = "Ada"
needs        = ["proto:generate"]     # cross-package deps for this workflow
REQUIRED_ENV = ["API_TOKEN"]          # refuse to start unless set
env_file     = ".env"                 # relative to this package

[[steps]]
name        = "compile"
description = "Build the release binary"
run         = "cargo build --release"
requires    = ["cargo", "protoc"]     # tools this step needs
timeout     = "10m"
retries     = 1

[[steps]]
name        = "publish"
description = "Publish the binary to Nexus"
kind        = "push"                  # a special, identifiable phase
recipe      = "binary"                # a [recipies] entry in this package
needs       = ["compile"]
```

Steps run **from their own package's directory**, with that package's
`[workspace.env]` variables in scope, so scripts work exactly as they do when
you run them by hand.

### One graph, drawn before it runs

```console
$ ciabatta build --graph
Workflow 'build' — 4 step(s) across 3 sub-workspace(s), in 4 wave(s)
Root: /home/ada/monorepo

  wave 1 — runs in parallel
  └─ proto:codegen
        Emit Rust + TS stubs into gen/
        from proto (packages/proto), owner Grace

  wave 2 — runs in parallel
  └─ api:compile  [1 retries]
        Build the release binary
        from api (packages/api), owner Ada
        after proto:codegen
        needs tools cargo, protoc
  ...
```

Every node says which package it came from and who owns it. `--dry-run` walks
the same graph and prints each command without executing it; `--gui` streams it
into a live browser view.

Dependencies are declared in two places, and both accept the same spelling:

| Spelling | Means |
| --- | --- |
| `depends_on` in `[workspace]` | Applies to **every** workflow in the package. |
| `needs` in a workflow file | Applies to that workflow only. |
| `"common"` | That package's workflow *of the same name*. Skipped if it has none. |
| `"proto:generate"` | Exactly that workflow. An error if it doesn't exist. |
| `"self:codegen"` | Another workflow in the same package. |

A cycle is refused with the loop spelled out, rather than deadlocking.

### Missing toolchains, answered

Declare install instructions once at the monorepo root:

```toml
# .ciabatta/ciabatta.toml at the repo root
[workspace]
umbrella = true            # the root is shared settings, not a package

[toolchain.protoc]
hint        = "brew install protobuf"
check       = "protoc --version"    # optional: smarter than a PATH lookup
description = "Protocol buffer compiler"
```

Every tool a graph requires is checked **before the first step runs**, and all
the missing ones are reported together:

```console
$ ciabatta build
Missing 1 build tool(s):
  • protoc — needed by web:bundle
    install it with: brew install protobuf
```

### Steps that would otherwise hang

| Setting | What it does |
| --- | --- |
| `timeout = "10m"` | Kills the step — and everything it spawned — past the limit, marks it failed, and **lets the rest of the graph carry on**. |
| `persistent = true` | A dev server or watcher that never exits: it's started, its dependents are released immediately, and the daemon takes ownership of it as a **watch session that outlives the run** — see below. |
| `retries = 2` | Extra attempts for a flaky step. A timeout isn't retried — it's stuck, not flaky. |
| `continue_on_error = true` | Its failure skips its dependents but doesn't stop the run. |

A run that tolerated failures still **fails**, and reports every one of them at
the end — tolerating a failure is not hiding it.

### Persistent steps outlive the run

A dev server that dies with the build that started it isn't persistent at all,
so ciabatta hands `persistent` steps to its daemon as watch sessions. The step
runs in its own package's directory with the run's environment, its output is
collected in full, and the run prints the id to reach it by:

```console
$ ciabatta dev
[dev]   [api:server] $ npm run dev   (persistent — the graph continues)
[dev]   [api:server] handed to the ciabatta daemon as watch session #4 — it outlives this run
[dev]   [api:server] follow it:  ciabatta watch --attach 4
[dev]   [api:server] stop it:    ciabatta watch --stop 4
[dev] ✓ completed

$ ciabatta watch --list
Watch sessions (1):
  #4    running         812 lines   api:server  (npm run dev)
```

`--attach` tails it in the terminal (Ctrl-C detaches; the session keeps going),
and it shows up on the **Watch** page named after the step that left it behind.
Sessions are owned by the daemon, so they end when you stop them or when the
daemon does — `ciabatta daemon stop` takes them with it.

If no daemon can be reached, the step falls back to running inside the run: it
still doesn't block the graph, but it stops when the run does, and the log says
so rather than leaving you to find out.

### Finding things

```console
$ ciabatta list -s proto
▪ proto  (packages/proto)
  Shared protobuf definitions
  owner: Grace

    generate         Generate language bindings from the .proto files
    owner: Grace     run with: ciabatta generate --only proto
```

`-s/--search` matches names, descriptions, owners, tags, and the commands steps
actually run. `-v` drills into the steps. The same data is on the daemon's HTTP
API (`GET /api/workspace`, `GET /api/workspace/graph?workflow=build`) for the
web app.

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
type       = "nexus"
url        = "https://${NEXUS_HOST:-nexus.example.com}"  # bare Nexus host
repository = "raw-hosted"   # which repo artifacts publish into
format     = "raw"          # raw | npm | pypi
tls_verify = true
needs_auth = true

[registries.s3]
type = "s3"
url  = "s3://my-artifacts-bucket"   # the bucket, with the s3:// scheme

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

### Nexus repositories: raw, npm, and PyPI

A Nexus registry picks its target repository and publish mechanism with three
fields:

- `repository` — the Nexus repo name (e.g. `raw-hosted`, `npm-hosted`). When set,
  `url` is the bare Nexus host and `/repository/<repository>` is appended for you.
  When omitted, `url` is used as the full repository URL (backwards compatible).
- `format` — `raw` (default), `npm`, or `pypi`, selecting how the push happens.
- `base_path` — *raw only*: an optional prefix prepended to every recipe's
  `publish_path`, so raw artifacts land under a common folder.

| `format` | How a push works | Requirements |
| --- | --- | --- |
| `raw` | HTTP `PUT` (pull is HTTP `GET`) | none |
| `npm` | `npm publish <artifact> --registry <repo>` | `npm` on `PATH` |
| `pypi` | `twine upload --repository-url <repo> <files>` | `twine` on `PATH` |

For `npm` / `pypi` recipes, `local_artifact_path` is the package tarball or the
`dist/` directory to publish, and `publish_path` is not used (the package name and
version determine where it lands). Both read credentials from
`CIABATTA_<NAME>_USER` / `_PASS`; npm also accepts a `CIABATTA_<NAME>_TOKEN`
bearer token. `ciabatta pull` supports only `raw` repositories — pull npm/PyPI
packages with their native clients.

```toml
# Publish an npm package straight to a Nexus npm repository.
[registries.sdk]
type       = "nexus"
url        = "https://nexus.example.com"
repository = "npm-hosted"
format     = "npm"

[recipies.sdk]
registry            = "sdk"
local_artifact_path = "packages/sdk"   # tarball or package directory
```

### S3

An S3 registry drives the AWS CLI, so it's just a bucket URL: set `url` to
`s3://<bucket>` and each recipe's `publish_path` becomes the object key.

```toml
[registries.s3]
type = "s3"                        # inferred when the name contains "s3"
url  = "s3://my-artifacts-bucket"

[recipies.release]
registry            = "s3"
local_artifact_path = "target/release/app"
publish_path        = "app/{CIABATTA_BRANCH}/{CIABATTA_COMMIT}/app"
# uploads to s3://my-artifacts-bucket/app/<branch>/<commit>/app
```

- **Auth** uses the standard AWS credential chain — `AWS_ACCESS_KEY_ID` /
  `AWS_SECRET_ACCESS_KEY`, `AWS_PROFILE`, or an instance/role profile — so no
  `login_script` is needed. Set `AWS_REGION` if your bucket isn't in the CLI's
  default region.
- The `aws` CLI must be installed and configured on the machine or CI runner;
  Ciabatta shells out to `aws s3 cp` for both push and pull.

### Docker / ECR images

For `docker`- and `ecr`-type registries, point a recipe at a **locally-built
image** with `local_image`. Ciabatta retags it to the registry's target
reference and pushes it — so you don't have to bake the registry URL into your
`docker build`:

```toml
[registries.myecr]
type = "ecr"                       # inferred when the name contains "ecr"
url  = "123456789.dkr.ecr.us-east-1.amazonaws.com"

[recipies.app]
registry     = "myecr"
local_image  = "app:latest"        # a local image (name or name:tag)
publish_path = "app:{CIABATTA_COMMIT}"   # remote image ref (repo[:tag])

  # Build the image locally; ciabatta handles the tag + push.
  [recipies.app.push]
  pre = "docker build -t app:latest ."
```

On push this runs `docker tag app:latest <url>/app:<commit>` then
`docker push <url>/app:<commit>`. On pull it pulls that remote reference and
retags it back to `app:latest`. Omit `publish_path` to reuse `local_image`
verbatim as the remote reference. ECR auto-logs in via
`aws ecr get-login-password`; plain Docker registries use
`CIABATTA_<REGISTRY>_USER` / `_PASS`.

## Stages

Every direction runs as a four-stage state machine, and the TUI shows progress
through each stage live:

```
Push:    login → pre-push   → push   → post-push
Pull:    login → pre-pull   → pull   → post-pull
Run:     login → pre-run    → run    → post-run
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

## Runs

A **run** is a third recipe direction, executed with `ciabatta run`. Instead of
a single registry transfer, its `run` phase executes a **DAG of dependent script
steps** — build → migrate → release, and so on. Runs are "just another
recipe": they live in `[recipies.<name>.run]`, show up in `ciabatta list`,
and work with menus (`--cookbook`).

The **step graph lives in its own flowchart file**, referenced from your config,
so a complex pipeline doesn't clutter `ciabatta.toml`:

```toml
# .ciabatta/ciabatta.toml
[recipies.web.run]
flowchart = ".ciabatta/runs.toml"      # the step DAG (a separate file)
pre  = "scripts/notify_start.sh"        # optional login/pre/post phase hooks
```

```toml
# .ciabatta/runs.toml — each top-level entry is a series of dependent steps.
[web]
  REQUIRED_ENV = ["RUN_TOKEN", "AWS_REGION"]     # gate the whole flowchart

  [[web.steps]]
  name = "build"
  script = "scripts/build.sh"           # a bash file… (or use run = "…" inline)

  [[web.steps]]
  name = "migrate"
  script  = "scripts/migrate.sh"
  needs   = ["build"]                    # runs once "build" succeeds (a DAG edge)
  on_error = "fix_migrate"               # on failure, jump to a recovery node

  [[web.steps]]                          # a recovery node: a choice of fixes
  name = "fix_migrate"
  recover = true
  message = "Migration failed — choose how to recover:"
  retry   = "migrate"                    # re-run this step after a fix succeeds
  options = [
    { label = "Roll back",    script = "scripts/rollback.sh" },
    { label = "Force unlock", run = "make unlock", default = true },
  ]

  [[web.steps]]
  name = "release"
  script = "scripts/release.sh"
  needs  = ["migrate"]
```

Steps whose `needs` are all satisfied become eligible to run; the graph is
validated up front (missing edges, non-recovery `on_error` targets, and cycles
are rejected before anything runs).

**`REQUIRED_ENV`** lists variables the flowchart needs. Before any phase runs,
each is checked; if one is empty or unset the run is aborted — the missing
names are printed to the console and shown in the `--gui` view, and no step runs.
(You can also set `REQUIRED_ENV` on the `[recipies.<name>.run]` table; the two
lists are merged.)

Started from the **web app**, a missing variable isn't a failure — the launcher
refuses to start the run and asks you for the values instead, then starts it with
what you typed. Ciabatta checks the daemon's own environment and any `env_file`
the recipe sources first, so it only prompts for what genuinely has nowhere else
to come from.

### Build variables are auto-sourced

Every `ciabatta run` **auto-sources the `CIABATTA_*` build variables from your
local git** (`CIABATTA_BRANCH` / `_COMMIT` / `_TAG` / `_BUILD_NUMBER`, plus the
derived `CIABATTA_PATH`) and makes them available to every step, `run` command,
and phase hook — the same set `ciabatta source` prints, so you don't need to
`eval "$(ciabatta source)"` first. This happens regardless of `--local` /
`CIABATTA_ENV`, so a run's script can reference `$CIABATTA_COMMIT` on a plain
dev-machine run:

```toml
[[web.steps]]
name = "release"
run  = "./scripts/release.sh --tag $CIABATTA_COMMIT"
```

Only *unset* variables are filled in: anything you provide explicitly — from a CI
system, the ambient environment, or `-e CIABATTA_BRANCH=…` — takes precedence. A
non-git directory is fine; the run just proceeds without the git-derived values.

### Error recovery ("if error")

When a step with `on_error` fails, control jumps to its **recovery node**, which
presents a list of fix `options`:

- With **`--gui`**, the browser shows fix-it buttons — click one to run that fix.
- Without a UI (plain / CI), the option marked **`default = true`** runs
  automatically (unattended self-heal); if none is marked, the run fails and
  prints the available remedies.
- After a fix succeeds, **`retry`** re-runs the named step. Retry loops are
  bounded so a persistently failing step can't spin forever.

### Watching and building runs in the browser

```bash
ciabatta run web --gui        # live view: flowchart + streaming logs + fix buttons
ciabatta run --build          # visual flowchart editor → copy the generated TOML
```

`--gui` hands the run to the daemon and opens a page at
`http://127.0.0.1:8099/run/<id>` showing each step lighting up as it runs,
per-step logs, and interactive recovery. The daemon owns the run, so it keeps
going if you close the terminal. `--build` opens a visual
builder that needs no project: lay out steps, edges, and recovery options, then
copy the emitted TOML into your flowchart file. Already have a pipeline? Paste
its TOML into the builder's import box to keep editing it visually.

| Phase | Override key | Default |
| --- | --- | --- |
| login | `login` | nothing (runs usually authenticate inside a step) |
| pre-run | `pre` | nothing |
| run | the `steps` DAG | executes the flowchart |
| post-run | `post` | nothing |

## Credentials

When a registry has **no** `login_script` and no `login` override, Ciabatta reads
per-registry credentials from the environment:

```
CIABATTA_<REGISTRY>_USER    CIABATTA_<REGISTRY>_PASS
```

`<REGISTRY>` is the registry's section name, uppercased — so `[registries.nexus]`
uses `CIABATTA_NEXUS_USER` / `CIABATTA_NEXUS_PASS`. They're applied per type:

- **Nexus (raw) / Artifactory** — sent as HTTP basic auth on the upload/download.
- **Nexus (npm)** — written to a throwaway npmrc for `npm publish`; prefers a
  `CIABATTA_<REGISTRY>_TOKEN`, otherwise basic auth from `_USER` / `_PASS`.
- **Nexus (PyPI)** — passed to `twine upload` as `-u` / `-p`.
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
crates.io). The result is written as JSON and shown at
`http://127.0.0.1:8099/analyze`, where you can sort, filter and click through the
graph. You can also rescan from that page.

**Accurate Rust versions.** A `Cargo.toml` dependency is only a *requirement*
(`serde = "1"`), so `analyze` reads the workspace `Cargo.lock` and shows the
concrete version cargo actually locked (`serde 1.0.228`), keeping the original
requirement alongside it. The lockfile is also how it tells your own crates
apart from crates.io: a workspace member or a `path`/`git` dependency is
classified as **internal** rather than being drawn as an external crates.io box,
so internal-crate edges no longer masquerade as third-party dependencies.

**Structure tab.** The view has a **Structure** tab showing the repository's
folder tree, pruned to just the folders that contain a dependency manifest
(`Cargo.toml`, `package.json`, `pyproject.toml`, …). Click a folder — or a
manifest within it — to see the package it defines, its resolved dependencies
(internal and external), and where it publishes; every entry links back into the
graph.

**Publish scripts.** Developers often publish from shell scripts, so `analyze`
also reads `.sh` files (anywhere in the tree, plus `.ciabatta/` and any script
referenced by your config) and turns registry-push commands into publish points:
`docker`/`podman push`, `aws s3 cp`/`sync`, `cargo publish`, `npm`/`yarn
publish`, `twine upload`, `helm push`, and `curl` uploads (`-T` / `--upload-file`
/ `PUT`). Each is wired to the package that owns the script, and — unlike a
ciabatta recipe — is **not** flagged as ciabatta-managed.

```bash
ciabatta analyze                 # write JSON + open the view at :8099/analyze
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

**Graph layout options.** The graph toolbar lets you retune the picture without
re-running: switch between a **columns →** and **rows ↓** layout, sort each
column by name / connections / version, pick **curved**, **straight**, or
**stepped** edges, **group internal packages by workspace**, **hide orphans**
(nodes with no visible edge), **size nodes by their number of connections**, and
zoom in / out / fit.

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

## Watch

`ciabatta watch <command>` runs a command, captures everything it writes to
stdout and stderr, and serves a **live, searchable web view** of the logs — handy
for dev servers, long builds, test runners, or any chatty process you'd rather
scan in a browser than in a scrollback buffer.

```bash
ciabatta watch "npm run dev"                 # stream a dev server's logs
ciabatta watch -t error -t "panic" "cargo test"   # notify on matching lines
ciabatta watch --list            # every session the daemon is running
ciabatta watch --attach 3        # tail session 3 (Ctrl-C only detaches)
ciabatta watch --stop 3          # stop session 3
```

Sessions also arrive here from workflows: a step marked `persistent = true` is
handed to the daemon as one, so it keeps running after its build finishes. Those
sessions are named after the graph node that left them behind — see
[Persistent steps outlive the run](#persistent-steps-outlive-the-run).

The command runs through your shell, so pipes, `&&`, and redirects work — quote
the whole command when you use them. The view opens in your browser at
`http://127.0.0.1:8099/watch/<id>` (suppress with `--no-open`), and
keeps serving after the command exits so the logs stay browsable. The status pill
shows whether it's running or how it exited.

- **Search** — type one or more terms (comma/space separated) and match **any**
  (OR) or **all** (AND) of them; filter by stdout/stderr, or switch on regex.
  Search runs server-side over the whole buffer, so history that has scrolled out
  of the live view is still findable. Matches are highlighted.
- **Live tail** — new lines stream in automatically; toggle "follow tail" to stop
  auto-scrolling while you read.
- **Bookmarks ("points")** — hover any line and click ★ to save it with a label.
  Click a bookmark to jump back to it. Each bookmark snapshots the line's text, so
  it stays viewable even after the line scrolls out of the buffer.
- **Triggers** — add a phrase (or regex) and get notified whenever a new matching
  line appears: a desktop notification (Web Notifications, permission asked once),
  an in-page toast with a sound, **and** the matching line printed with a terminal
  bell in the console where `ciabatta watch` is running. Seed triggers up front
  with `-t`/`--trigger` (repeatable) or add them live in the sidebar, which also
  keeps a running hit count and a feed of recent matches.

Bookmarks and triggers **persist to disk** under `~/.ciabatta/watch/`, keyed by
the command, so they come back the next time you watch the same command. Log
lines themselves are never written to disk. The in-memory buffer is bounded
(`--max-lines`, default 200,000); older lines are dropped once it's full.

Useful flags:

- `-t, --trigger PHRASE` — notify on lines containing this phrase (repeatable).
- `-p, --port PORT` — the ciabatta daemon's port (default `8099`).
- `--stop ID` — stop a running session. Ctrl-C on `ciabatta watch` only
  detaches from it; the command keeps running in the daemon.
- `--max-lines N` — cap the in-memory log buffer (default `200000`).
- `--no-open` — don't open the browser automatically.

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

### `CIABATTA_ENV=local`

Set `CIABATTA_ENV=local` (or pass `--local`) to resolve `CIABATTA_BRANCH` /
`_COMMIT` / `_TAG` / `_BUILD_NUMBER` from your **local git** repository instead
of a CI system — so `ciabatta push` / `pull` just work on a dev machine without
having to pass `-e` on every invocation:

```bash
export CIABATTA_ENV=local
ciabatta push ciabatta_binary
```

**`ciabatta pull` finds the best commit for your branch** (in both local and CI
mode): if the exact commit has no published artifact, it walks the branch's git
history and pulls the most recent commit that does (over HTTP registries like
Nexus). This just needs the branch history to be available — a normal CI checkout
has it; it tries the branch ref, then `origin/<branch>`, then `HEAD`.

## The daemon and the web app

Every browser-facing feature — todo, watch, run, analyze and the AI mind map
— is served by **one background daemon** on **one port**, at
`http://127.0.0.1:8099`. Each is a page in a single web app rather than a
separate server on its own port.

You almost never start it by hand. Any command with a web view checks for a
running daemon and launches one in the background if there isn't one:

```bash
ciabatta todo        # starts the daemon if needed, opens http://127.0.0.1:8099/todo
ciabatta watch make    # reuses the same daemon, opens /watch/<id>
```

| Page | What it does |
| --- | --- |
| `/` | Dashboard — daemon status and every tool in one place. |
| `/todo` | Your personal task list (global, not per-project). |
| `/watch` | Watch sessions; `/watch/<id>` is one session's live log. |
| `/run` | Runs; `/run/<id>` is a live flowchart, `/run/builder` the editor. |
| `/analyze` | The dependency graph. |
| `/ai` | The architecture mind map and background assistant jobs. |

### Managing it

```bash
ciabatta daemon status     # is it running, and where
ciabatta daemon stop       # stop it (background work stops with it)
ciabatta daemon restart
ciabatta daemon logs -f    # follow ~/.ciabatta/daemon.log
ciabatta daemon serve      # run it in the foreground, e.g. to debug startup
```

The port is `8099` by default; override it with `CIABATTA_DAEMON_PORT` or `-p`
on any command. `CIABATTA_BIND_HOST=0.0.0.0` exposes it beyond loopback — see
the security note below before you do that.

### Projects

One daemon serves every checkout you use. Each command registers its working
directory, and the project switcher in the top bar picks which one the
per-project pages are showing. The todo list is deliberately *not* scoped: it
lives in `~/.ciabatta/todos.json` and follows you between repos.

### The daemon owns your work

`ciabatta watch` and `ciabatta run --gui` hand the work to the daemon rather
than running it in your terminal. That means:

- **Ctrl-C on `ciabatta watch` detaches, it doesn't kill.** The command keeps
  running and stays live in the browser. Stop it for real with
  `ciabatta watch --stop <ID>` or the Stop button.
- `ciabatta run --gui` returns as soon as the run starts. Closing the
  terminal — or the laptop — doesn't abandon a run mid-flight.
- Stopping the daemon stops everything it owns.

### Security

The daemon is long-lived and its API can start processes, so every
state-changing route requires a bearer token. It's generated at startup and
stored in `~/.ciabatta/daemon.json` (mode `0600`); the web app receives it in
the served page, and the CLI reads it from that file. `GET /api/health` is the
only unauthenticated route.

This matters most with `CIABATTA_BIND_HOST=0.0.0.0`: without the token, anyone
who could reach the port could run commands as you. Keep it on loopback unless
you have a reason not to.

## Web frontend

Two separate front ends live in this repo:

- **`tool_frontend/`** — the daemon's web app described above (React, MUI,
  TanStack, React Flow). It's compiled into the binary, so a release is still a
  single file. Build it with
  `yarn turbo run build --filter=ciabatta-tool-frontend`; `yarn dev` inside
  `tool_frontend/` gives HMR against a running daemon.
- **`frontend/`** — the public docs site on GitHub Pages, with download links
  and usage instructions. See the
  [project site](https://forsyth-creations.github.io/Ciabatta/).

Building the Rust binary without a built `tool_frontend/dist` still works: the
daemon serves a placeholder page telling you to run the yarn build. CI and the
release workflow always build it first.

## License

Licensed under the [MIT License](LICENSE).
