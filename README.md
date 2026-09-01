<div align="center" style="margin: 24px 0;">
  <a href="https://forsyth-creations.github.io/Ciabatta/" style="display: inline-block; padding: 12px 28px; background: linear-gradient(135deg, #d97742, #b5562b); color: #fff; font-family: 'Segoe UI', sans-serif; font-size: 18px; font-weight: 600; text-decoration: none; border-radius: 8px; box-shadow: 0 4px 10px rgba(0,0,0,0.15);">
    🍞 Ciabatta
  </a>
</div>

**Monorepo orchestration and artifact publishing.**

Ciabatta is a fast, cross-platform CLI that does two jobs, from one declarative
YAML file.

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
- **One config, many registries.** Describe your registries once in
  `.ciabatta/ciabatta.yaml`; a workflow step publishes to any of them.
- **CI-aware.** Automatically resolves `CIABATTA_BRANCH`, `CIABATTA_COMMIT`,
  `CIABATTA_TAG`, and `CIABATTA_BUILD_NUMBER` from GitLab, GitHub Actions,
  Jenkins, CircleCI, Azure DevOps, or Bitbucket — and lets you template them
  into publish paths.
- **Parallel with live progress.** Independent branches of a graph run at once,
  scheduled against their real dependencies. Runs print plain text; add `--tui`
  for the `ratatui` live view, or `--gui` for the browser.
- **Push *and* pull.** Because Ciabatta knows where things live, it can fetch
  artifacts back down, not just upload them — and a pull step names the push it
  mirrors rather than repeating it.
- **Bring your own auth.** Login is handled by your own scripts — Ciabatta just
  makes the resolved variables available to them as environment variables.
- **Truly drop-in.** Linux builds are statically linked (musl), so there is no
  glibc version requirement: download, extract, run, on any distro.

## Installation

### One line

**Linux / macOS**
```bash
curl -fsSL https://forsyth-creations.github.io/Ciabatta/install.sh | sh
```

**Windows** (PowerShell)
```powershell
irm https://forsyth-creations.github.io/Ciabatta/install.ps1 | iex
```

To pin a version, `sh` needs `-s --` before the options — everything after that
goes to the installer rather than to `sh`:

```bash
curl -fsSL https://forsyth-creations.github.io/Ciabatta/install.sh | sh -s -- --version 0.3.0
curl -fsSL https://forsyth-creations.github.io/Ciabatta/install.sh | sh -s -- --list
curl -fsSL https://forsyth-creations.github.io/Ciabatta/install.sh | sh -s -- --dir ~/bin
```

`iex` has the same problem and the same shape of fix — turn the script into a
block so you can call it with arguments:

```powershell
& ([scriptblock]::Create((irm https://forsyth-creations.github.io/Ciabatta/install.ps1))) -Version 0.3.0
& ([scriptblock]::Create((irm https://forsyth-creations.github.io/Ciabatta/install.ps1))) -List
```

| Option | What it does |
| --- | --- |
| `--version VERSION` / `-Version` | Install that release (`0.3.0`, `v0.3.0`, or `latest`). |
| `--dir DIR` / `-Dir` | Install there instead of the default. |
| `--list` / `-List` | Print the available versions and exit. |
| `--help` | Usage. |

`CIABATTA_VERSION` and `CIABATTA_INSTALL_DIR` do the same thing for callers that
find environment variables easier to set; an explicit flag wins over them.

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

# 2. See what there is to run
ciabatta list

# 3. Walk every step without side effects
ciabatta release --dry-run

# 4. Publish for real
ciabatta release

# 5. Just the publishing steps, skipping the builds
ciabatta release --filter kind:push
```

Ciabatta discovers your project by walking up to find the `.ciabatta/`
directory; the directory **above** it is treated as the project root that
artifacts are published from. For workflows it goes one level further out: the
**monorepo root** is your git root, and every directory beneath it with a
`.ciabatta/ciabatta.yaml` is a sub-workspace.

## Commands

| Command | What it does |
| --- | --- |
| `ciabatta <WORKFLOW>` | Run that workflow across every sub-workspace that defines one, in dependency order. `ciabatta build`, `ciabatta test`, … |
| `ciabatta workflow <NAME>` | The same thing, spelled out — for a workflow whose name collides with a command below. |
| `ciabatta dry-run [TARGET...]` | What a run would reuse from the cache and what it would rebuild — and for a rebuild, exactly what changed. Runs nothing. `--diff` for the lines. |
| `ciabatta cache <init\|status\|prune\|clean>` | Set up and inspect the build cache. `init [WORKFLOW]` proposes inputs and outputs from what's actually in the directory and writes them into that workflow's file. |
| `ciabatta remote-cache <init\|start\|login\|logout\|status\|add-user>` | Run or connect to a shared cache the whole team reads from. |
| `ciabatta self update` | Update this binary from the remote cache that serves it, verified against the SHA-256 it advertises. |
| `ciabatta convert --script PATH` | Turn an existing script into a workflow: its tools, its variables, its outputs, and the description in its header. |
| `ciabatta register` | Tell the daemon this checkout exists, so it shows up in the web app's project switcher. `init` does this for you. |
| `ciabatta config migrate` | Convert this checkout's TOML config files to YAML. |
| `ciabatta lsp` | Speak the Language Server Protocol on stdio, so an editor can complete and check `.ciabatta/` files. Your editor extension launches this; you don't. |
| `ciabatta list` | Every workflow in the monorepo — with descriptions, owners and dependencies. `-s TERM` to search, `-v` for steps. |
| `ciabatta init --lib` | Opt this package in as a sub-workspace: a `workspace:` identity plus a starter workflow. |
| `ciabatta init [--ci SYSTEM]` | Create a `.ciabatta/` directory with a starter publishing `ciabatta.yaml`. |
| `ciabatta configure` | Interactively add a registry — no hand-editing YAML. |
| `ciabatta configure auto` | Analyze the project and pick publishing workflows from an interactive checklist (Docker → ECR/Nexus, Rust binaries → crates.io / S3 / Nexus). |
| `ciabatta tui` (alias `browse`) | Interactive registry browser — inspect registries and walk what has been published. |
| `ciabatta analyze` | Build the project's dependency graph and open an interactive view. |
| `ciabatta watch <command>` | Run a command and stream its logs into a live, searchable web view with bookmarks and notification triggers. `--list` to see running sessions, `--attach <ID>` to tail one, `--stop <ID>` to end it. |
| `ciabatta todo [TASK] [--global]` | Task list, scoped to the project you're in — or to the global list with `--global`. With a TASK, adds it and exits; without, opens the todo page. |
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

Useful flags on any workflow (`ciabatta build`, `ciabatta release`, …):

- `--graph` — print the graph and stop. Nothing runs.
- `--dry-run` — walk every step, executing none of them.
- `--only MEMBER` — start from one sub-workspace. Its dependencies still come
  along, so the result stays correct. Repeatable.
- `--isolated` — don't follow dependencies out of what you selected, for when
  everything upstream is already built.
- `-e, --env KEY=VALUE` — set a variable for every step. Repeatable.
- `--tui` — run inside the live terminal UI. **Runs print plain text by
  default**, which is what CI wants and what you can pipe; the TUI is opt-in.
  (`--no-tui` is still accepted and does nothing.)
- `--gui` — watch the graph run live in a browser instead of the terminal.

Global flags (any command):

- `--debug` — enable debug logging to stderr. You can also set `CIABATTA_DEBUG=1`,
  or `CIABATTA_LOG=ciabatta=trace` for finer control.

When a push step's `artifact` is a **directory**, Ciabatta uploads each file in
it individually, recreating the folder structure under the step's
`publish_path` (creating sub-folders in the registry as needed) — so
`artifact: frontend/dist` publishes the whole `dist` tree.

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

That writes a `workspace:` identity into `packages/api/.ciabatta/ciabatta.yaml`:

```yaml
workspace:
  name: api
  description: Public REST API
  owner: Ada
  depends_on: [proto:generate]   # what this package needs first
  tags: [backend]
  requires: [cargo]              # tools every workflow here needs on PATH
```

`description` and `owner` aren't decoration — they're what `ciabatta list` shows
everyone else, so `init --lib` defaults the owner to your git user and nags you
if either is blank.

### Workflows

One file per workflow, in `.ciabatta/workflows/`. The **filename is the
workflow name**, and every package that defines a workflow of that name joins
the same graph.

```yaml
# packages/api/.ciabatta/workflows/build.yaml
description: Compile the API service
owner: Ada
needs: [proto:generate]        # cross-package deps for this workflow
REQUIRED_ENV: [API_TOKEN]      # refuse to start unless set
env_file: .env                 # relative to this package (and the default)
env_default: .env.default      # the checked-in template .env comes from

steps:
  - name: compile
    description: Build the release binary
    run: cargo build --release
    requires: [cargo, protoc]  # tools this step needs
    timeout: 10m
    retries: 1

  - name: publish
    description: Publish the binary to Nexus
    kind: push                 # the built-in registry transfer
    needs: [compile]
    registry: nexus            # a name from `registries:`
    artifact: target/release/api
    publish_path: "api/{CIABATTA_BRANCH}/{CIABATTA_COMMIT}/api"
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

```yaml
# .ciabatta/ciabatta.yaml at the repo root
workspace:
  umbrella: true           # the root is shared settings, not a package

toolchain:
  protoc:
    hint: brew install protobuf
    check: protoc --version         # optional: smarter than a PATH lookup
    description: Protocol buffer compiler
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

### Stale workflows

A repository records everything about a workflow except the thing that says
whether it still works: when anybody last ran it. Somebody adds
`deploy-staging`, the staging environment goes away, and the workflow stays —
listed, documented, apparently runnable, and broken in a way nobody finds until
they try.

So every run writes down what it ran, how it went and how long it took, and
anything past `stale_after` is flagged:

```yaml
# .ciabatta/ciabatta.yaml, at the monorepo root
workspace:
  stale_after: 30d      # the default
```

```
$ ciabatta list
    deploy           Deploy to staging
                     last run: 94 days ago (success, 7 run(s))  ← STALE

2 workflow(s) not run in over 30 days:
  api:deploy-staging           94 days ago
Each is either worth running or worth deleting — a workflow nobody runs is
one nobody has noticed is broken.
```

**"Never run" is not the same as stale.** The history lives under
`.ciabatta/history/` and is not committed — it is observation, not
configuration, and a file every run rewrites would conflict on every merge. So a
fresh clone knows nothing, which is an absence of evidence rather than evidence
a workflow is dead, and is reported differently.

Which is why it is shared. With a [remote cache](#remote-cache) configured, each
run reports what it ran and takes back what everyone else has run — so the
question becomes "when did *anyone* last run this". A workflow you personally
have not touched since March may be the one CI runs hourly; one nobody anywhere
has run since March is the one worth deleting. Both directions happen at the end
of a run, so `ciabatta list` never waits on the network.

The server sees every project at once, which no single checkout can, and has its
own threshold — a cache serving five teams may want to hear about a quarter of
silence where one team calls a fortnight stale:

```
$ ciabatta remote-cache status

Projects:
  api        3f2a…   412 hit / 38 miss  ·  2 stale workflow(s)

Workflows: 24 tracked, 3 not run by anyone in over 30d
  api        api:deploy-staging       94 days ago  (7 run(s) ever)
```

### Background tasks

Some things a build needs are not steps in it: a mock API the integration tests
call, a database container, a bundler in watch mode. They have to be *running*,
they never finish, and waiting for one is waiting forever. They go in the
workflow's `background:` array, beside `steps:` rather than inside it.

```yaml
# packages/mock-api/.ciabatta/workflows/serve.yaml — declared once, by whoever owns it
steps:
  - name: api
    run: node mock.js
    persistent: true

# packages/web/.ciabatta/workflows/test.yaml — used by whoever needs it up
needs:
  - proto:generate     # must finish first
background:
  - mock-api:serve     # must be running; nothing waits for it

steps:
  - name: integration
    run: yarn test:integration
```

Named exactly the way `needs` names things — `"<member>"` for that
sub-workspace's workflow of this name, `"<member>:<workflow>"` for a specific
one — because they are the same kind of thing: a target that already exists,
declared once, in its own package, by whoever owns it. **The only difference
from `needs` is that a `needs` target is waited for and a `background` target is
merely started.** Its steps are started before the first wave and **gate
nothing** — no mistake in one can hold a build up. The graph draws them in a row
of their own at the bottom, under a lightning bolt, rather than in a wave.

A background target keeps the order its own steps declare among themselves — a
database has to be up before the app that talks to it — but it may not declare
workflow-level `needs`, because there is nowhere for those to run: it starts
before the first wave and waits for nothing.

Being started before wave 1 is not the same as being *ready* before wave 1. If
a step would race the server it talks to, have that step wait for the port;
ciabatta can't, because "ready" is a different question for every server.

**Background vs. persistent** is a question of what happens when the run ends,
and nothing else:

| | When the run ends |
| --- | --- |
| `background:` entry | **Stopped.** It existed to get this run through; leaving it up would hand you a process still holding its port for the next run to collide with. |
| step with `persistent = true` | **Left running.** The daemon owns it, so `ciabatta dev` leaves you a dev server to work against. Stop it with `ciabatta watch --stop <id>`. |

Either way it runs as a watch session labelled with the node that started it, so
its output is readable live and afterwards — a stopped background task leaves
its session behind even though its process is gone.

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

Ciabatta reads `.ciabatta/ciabatta.yaml`. Registries describe *where* things go;
a workflow step with `kind: push` says *what* to publish and *how*.

The fastest way to start is `ciabatta configure` (add a registry interactively)
or `ciabatta configure auto` (let Ciabatta inspect the repo and suggest
publishing workflows). You can also edit the file by hand:

```yaml
system:
  ci: github           # gitlab | github | jenkins | circleci | azure | bitbucket
  containers: docker   # docker | podman — when omitted, Ciabatta auto-detects
                       # what is installed (prefers podman, then docker; asks
                       # you to choose if both are present).

registries:
  nexus:
    # url and login_script expand environment variables with bash-style
    # defaults, so one config can target different environments.
    type: nexus
    url: "https://${NEXUS_HOST:-nexus.example.com}"   # bare Nexus host
    repository: raw-hosted   # which repo artifacts publish into
    format: raw              # raw | npm | pypi
    tls_verify: true
    needs_auth: true

  s3:
    type: s3
    url: s3://my-artifacts-bucket    # the bucket, with the s3:// scheme

workflows:
  release:
    description: Build the frontend and publish it
    steps:
      - name: build
        run: npm run build

      # Copy a local artifact to a templated publish path.
      - name: publish
        kind: push
        needs: [build]
        registry: nexus
        artifact: frontend/dist
        publish_path: "frontend/{CIABATTA_BRANCH}/{CIABATTA_COMMIT}/frontend"

  fetch:
    description: Pull the published bundle back down
    steps:
      # The same artifact, the other way. `from` names the push it mirrors, so
      # the registry and path are stated once.
      - name: fetch
        kind: pull
        from: release:publish
```

Workflows usually live one-file-each in `.ciabatta/workflows/<name>.yaml` — the
filename is the workflow name. Writing them inline under `workflows:` as above
suits a small project; a monorepo package gets a file.

A few rules worth knowing:

- If a `publish_path` references a variable that isn't set, Ciabatta **errors
  immediately** rather than publishing to a half-resolved path.
- Step commands and login scripts all receive every resolved `CIABATTA_*`
  variable (plus anything you pass with `-e`) in their environment.

Run `ciabatta config reference` for the full, always-up-to-date field listing.

### Nexus repositories: raw, npm, and PyPI

A Nexus registry picks its target repository and publish mechanism with three
fields:

- `repository` — the Nexus repo name (e.g. `raw-hosted`, `npm-hosted`). When set,
  `url` is the bare Nexus host and `/repository/<repository>` is appended for you.
  When omitted, `url` is used as the full repository URL (backwards compatible).
- `format` — `raw` (default), `npm`, or `pypi`, selecting how the push happens.
- `base_path` — *raw only*: an optional prefix prepended to every step's
  `publish_path`, so raw artifacts land under a common folder.

| `format` | How a push works | Requirements |
| --- | --- | --- |
| `raw` | HTTP `PUT` (pull is HTTP `GET`) | none |
| `npm` | `npm publish <artifact> --registry <repo>` | `npm` on `PATH` |
| `pypi` | `twine upload --repository-url <repo> <files>` | `twine` on `PATH` |

For `npm` / `pypi`, a push step's `artifact` is the package tarball or the
`dist/` directory to publish, and `publish_path` is not used (the package name and
version determine where it lands). Both read credentials from
`CIABATTA_<NAME>_USER` / `_PASS`; npm also accepts a `CIABATTA_<NAME>_TOKEN`
bearer token. A `kind: pull` step supports only `raw` repositories — fetch
npm/PyPI packages with their native clients.

```yaml
# Publish an npm package straight to a Nexus npm repository.
registries:
  sdk:
    type: nexus
    url: https://nexus.example.com
    repository: npm-hosted
    format: npm

workflows:
  release:
    steps:
      - name: publish
        kind: push
        registry: sdk
        artifact: packages/sdk   # tarball or package directory
```

### S3

An S3 registry drives the AWS CLI, so it's just a bucket URL: set `url` to
`s3://<bucket>` and a push step's `publish_path` becomes the object key.

```yaml
registries:
  s3:
    type: s3                       # inferred when the name contains "s3"
    url: s3://my-artifacts-bucket

workflows:
  release:
    steps:
      - name: publish
        kind: push
        registry: s3
        artifact: target/release/app
        publish_path: "app/{CIABATTA_BRANCH}/{CIABATTA_COMMIT}/app"
        # uploads to s3://my-artifacts-bucket/app/<branch>/<commit>/app
```

- **Auth** uses the standard AWS credential chain — `AWS_ACCESS_KEY_ID` /
  `AWS_SECRET_ACCESS_KEY`, `AWS_PROFILE`, or an instance/role profile — so no
  `login_script` is needed. Set `AWS_REGION` if your bucket isn't in the CLI's
  default region.
- The `aws` CLI must be installed and configured on the machine or CI runner;
  Ciabatta shells out to `aws s3 cp` for both push and pull.

### Docker / ECR images

For `docker`- and `ecr`-type registries, point a push step at a **locally-built
image** with `local_image`. Ciabatta retags it to the registry's target
reference and pushes it — so you don't have to bake the registry URL into your
`docker build`:

```yaml
registries:
  myecr:
    type: ecr                      # inferred when the name contains "ecr"
    url: 123456789.dkr.ecr.us-east-1.amazonaws.com

workflows:
  release:
    steps:
      # Build the image locally; ciabatta handles the tag + push.
      - name: image
        run: docker build -t app:latest .

      - name: publish
        kind: push
        needs: [image]
        registry: myecr
        local_image: app:latest                 # a local image (name or name:tag)
        publish_path: "app:{CIABATTA_COMMIT}"   # remote image ref (repo[:tag])
```

On push this runs `docker tag app:latest <url>/app:<commit>` then
`docker push <url>/app:<commit>`. On pull it pulls that remote reference and
retags it back to `app:latest`. Omit `publish_path` to reuse `local_image`
verbatim as the remote reference. ECR auto-logs in via
`aws ecr get-login-password`; plain Docker registries use
`CIABATTA_<REGISTRY>_USER` / `_PASS`.

## Ordering: edges, not stages

Publishing used to run through a fixed `login → pre → main → post` pipeline with
per-direction overrides. It doesn't any more — what those stages were for is
what a graph already does, and each piece is now a node you can see on the
graph, filter, cache and fail independently.

| Was | Is now |
| --- | --- |
| a `pre` command | a step, and a `needs` edge to it |
| a `post` command | a step that `needs` the transfer |
| a `login` override | the registry's own `login_script` |
| a `main` override | a `run:` on the transfer step itself |

```yaml
steps:
  - name: bundle
    run: python scripts/bundle.py

  - name: publish
    kind: push
    needs: [bundle]
    registry: nexus
    artifact: frontend/dist
    publish_path: "front/{CIABATTA_COMMIT}/dist"

  - name: notify
    run: ./scripts/notify.sh deployed
    needs: [publish]
```

## Runs

Everything ciabatta runs is a workflow: a **DAG of dependent steps** — build →
migrate → release, and so on. A workflow lives in its own file, one per name,
so a complex pipeline doesn't clutter `ciabatta.yaml`:

```yaml
# packages/web/.ciabatta/workflows/deploy.yaml — the filename is the name.
description: Migrate and release the web app
REQUIRED_ENV: [RUN_TOKEN, AWS_REGION]     # gate the whole graph

steps:
  - name: build
    script: scripts/build.sh        # a bash file… (or use `run:` inline)

  - name: migrate
    script: scripts/migrate.sh
    needs: [build]                  # runs once "build" succeeds (a DAG edge)
    on_error: fix_migrate           # on failure, jump to a recovery node

  - name: fix_migrate               # a recovery node: a choice of fixes
    recover: true
    message: "Migration failed — choose how to recover:"
    retry: migrate                  # re-run this step after a fix succeeds
    options:
      - label: Roll back
        script: scripts/rollback.sh
      - label: Force unlock
        run: make unlock
        default: true

  - name: release
    script: scripts/release.sh
    needs: [migrate]
```

Steps whose `needs` are all satisfied become eligible to run; the graph is
validated up front (missing edges, non-recovery `on_error` targets, and cycles
are rejected before anything runs).

**`REQUIRED_ENV`** lists variables the workflow needs. Before anything runs,
each is checked; if one is empty or unset the run is aborted — the missing
names are printed to the console and shown in the `--gui` view, and no step runs.

Started from the **web app**, a missing variable isn't a failure — the launcher
refuses to start the run and asks you for the values instead, then starts it with
what you typed. Ciabatta checks the daemon's own environment and any `env_file`
the workflow sources first, so it only prompts for what genuinely has nowhere
else to come from.

### The environment is printed before anything runs

A workflow's steps are shell scripts, and the difference between "works here"
and "fails there" is far more often a variable than the graph. So every run
prints the variables it depends on first — the same way it prints the graph
before executing it:

```
Environment for 'web' — 5 variable(s) this run depends on
  sourcing .env
  API_TOKEN     ••••••••                  [environment · REQUIRED_ENV · used by deploy]
  AWS_REGION    eu-west-1                 [env file · .env · REQUIRED_ENV · used by deploy]
  STAGE         prod                      [environment · REQUIRED_ENV · used by build, deploy]
  DATABASE_URL  postgres://localhost/app  [env file · .env]
  JOBS          4                         [config · used by build]
```

The list is everything the run is actually wired to: `REQUIRED_ENV`, every key
in the `.env` files it sources, the `[env]` tables that cascade from
sub-workspace to workflow to step, and every `$VAR` the step commands, working
directories and `when` / `skip_if` conditions read. Each line says where the
value came from (`environment` — your shell, CI, or `-e` — beats `env file`,
which beats `config`) and which steps depend on it. Anything unset is shown as
such, and required variables that are missing are called out before the run
aborts on them.

**Values whose names look like secrets** (`*_TOKEN`, `*_SECRET*`, `*PASSWORD*`,
`*_KEY`, `*_PASS`, `*AUTH*`, …) are masked. This output goes into CI logs.

The same list is served to the web app, where each variable is drawn as a node
feeding into the steps that read it — click one to light up its dependents.

### Build variables are auto-sourced

Every `ciabatta run` **auto-sources the `CIABATTA_*` build variables from your
local git** (`CIABATTA_BRANCH` / `_COMMIT` / `_TAG` / `_BUILD_NUMBER`, plus the
derived `CIABATTA_PATH`) and makes them available to every step, `run` command,
and phase hook — the same set `ciabatta source` prints, so you don't need to
`eval "$(ciabatta source)"` first. This happens regardless of `--local` /
`CIABATTA_ENV`, so a run's script can reference `$CIABATTA_COMMIT` on a plain
dev-machine run:

```yaml
steps:
  - name: release
    run: ./scripts/release.sh --tag $CIABATTA_COMMIT
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
ciabatta deploy --gui         # live view: graph + streaming logs + fix buttons
```

`--gui` hands the run to the daemon and opens a page at
`http://127.0.0.1:8099/run/<id>` showing each step lighting up as it runs,
per-step logs, and interactive recovery. The daemon owns the run, so it keeps
going if you close the terminal.

## Caching

Off until a workspace opts in. A cache that turns itself on is a cache that will
one day serve somebody a stale artifact they never asked it to keep.

```bash
ciabatta cache init build      # propose inputs and outputs for the `build` workflow
ciabatta dry-run build         # what would be reused, and why not
ciabatta dry-run build --diff  # ...with the lines that changed
ciabatta cache status          # what the local store is holding
```

**Cache settings live with the workflow they describe** — in
`.ciabatta/workflows/<name>.yaml`, next to the steps — because what a build
reads is a property of that build. A `build` and a `test` in the same package
read different files and produce different things, and one section per config
gave them no way to say so.

```yaml
# packages/api/.ciabatta/workflows/build.yaml
cache:
  enabled: true
  # Paths are relative to the WORKSPACE ROOT, not to this file — note the
  # `packages/api/` on each one. See below for why.
  inputs:  ["packages/api/src/**/*",     # what the build READS
            "packages/api/Cargo.toml"]
  outputs: ["packages/api/target/release/app"]   # what the build WRITES
  exclude: [packages/api/target]        # never counted as an input, so a build
                                        # can't invalidate itself with its own
                                        # output
  env: [PROFILE]                        # variables the RESULT depends on

steps:
  - name: compile
    run: cargo build --release
```

**Every path is relative to the workspace root**, wherever the `cache:` section
is written. One project is one cache — one store, one entry namespace, and one
`cache.remote`, which is read from the root — so one directory has to be what
those paths mean. Resolving them against each package instead would leave any
step reaching a sibling to spell it `../`, and a `../` in a stored path walks
back out of the cache entry it belongs to: every entry for that target ends up
writing its output to the same shared location, and restoring one hands back
another's file. `ciabatta why <step>` lists what a section actually matched,
which is the fastest way to check you got the prefix right.

`ciabatta cache init` writes the prefix for you — run it in the package and it
proposes paths already rooted correctly.

A step can narrow it further with its own `cache:`, layered over the workflow's
field by field — so a step that declares only `env:` keeps the workflow's inputs
and outputs, and does not silently turn caching off by failing to mention
`enabled`.

`ciabatta cache init` writes into the workflow file. With one workflow in the
package the name is optional; with several you name the one you mean, because
guessing would write the wrong answer into the wrong file.

A stage has exactly **three** dependencies, and any of them changing is a
rebuild:

1. its **input files**,
2. the **environment variables** it declared in `cache.env`,
3. the **outputs of the stages it needs**.

The third is what makes a *graph* cacheable rather than just a directory. Change
a `.proto` file and `proto:generate` misses; its outputs change; every stage
downstream of it misses too — each for a reason it can name.

That propagation runs on output hashes, so it needs outputs to hash. A stage
that declares none — an uncached one, or one that lists `inputs:` and stops —
runs every time and looks identical afterwards no matter what it just did, so a
downstream key agreeing proves nothing. **Anything behind such a stage runs
too**, and says why:

```
● build   rebuild — generate ran and declares no `cache.outputs`, so there's no
                    telling whether what this step consumes changed
```

The same holds for a stage that failed and was recovered around: what it left
behind is unrecorded, so nothing downstream is served from the cache. Declaring
`cache.outputs` on the stage is what buys the reuse back — then an unchanged
output set stops the invalidation there, and only what genuinely moved rebuilds.

Two things worth being explicit about:

**An undeclared input is a wrong answer, not a slow one.** If a build reads a
file that isn't in `inputs`, changing that file won't change the key and the
cache will confidently hand back the wrong artifact. That's why `cache init`
scaffolds `inputs` from the directory's real contents instead of leaving it
empty, and why `dry-run` exists.

**Outputs are verified, not assumed.** A key match says the inputs didn't
change; it says nothing about whether somebody deleted `dist/` or hand-edited a
generated file. So the outputs are hashed too, and a mismatch is a restore or a
rebuild — the difference between "we think this is current" and "this is
current".

`ciabatta dry-run` is the command that makes the cache trustworthy. For every
stage it prints the decision, and when the answer is "rebuild" it shows the diff
that explains it — the changed files with their lines, the environment variables
that moved, and the upstream stages that produced something different. The same
view is on the **Cache** page of the web app, and on each node of the workflow
graph.

### Proving the inputs are right: `--authoritative`

`dry-run` shows you what the cache *thinks*. `--authoritative` checks whether it
is entitled to think it.

```bash
ciabatta build --authoritative
```

Every step runs in its own directory containing the files it declared under
`cache.inputs` and nothing else, laid out the way the project root is — so a
path that reaches sideways (`../schemas/*.json`) or writes upward
(`../dist/thing.vsix`) still resolves. A step that reads something it never
declared doesn't find it and fails, right now, instead of being handed a stale
artifact six weeks later when the undeclared file has changed and nothing
noticed. Declared outputs are copied back afterwards, so the run leaves the same
artifacts in the same places as an ordinary one.

The cache is switched off for these runs. A cache hit skips a step, and a step
that doesn't run isn't held to anything — and the cache is the thing under
suspicion in the first place.

**It is opt-in and stays that way.** This is not Bazel and doesn't pretend to
be: there is no hermetic toolchain, and no attempt to isolate `$HOME`, the
network, or the clock. The compiler and the package manager are whatever the
machine has. It answers one question — *are my inputs complete?*

Some steps genuinely need state that is not a source file. `yarn run check` has
to sit inside its yarn project; a cargo build wants the shared `target/`.
Listing `node_modules` under `inputs` would put a hundred thousand derived files
in the cache key and call them sources, so name them separately instead:

```bash
ciabatta build --authoritative \
  --sandbox-also node_modules --sandbox-also .yarn \
  --sandbox-also package.json --sandbox-also yarn.lock --sandbox-also .yarnrc.yml
```

Those paths are symlinked rather than copied, and everything they cover is
explicitly outside what the run vouches for. That it's a flag rather than a
config field is deliberate: a weakened check you retype at the call site stays
visible in a way that one line added to a config file two years ago does not.

A failed step's sandbox is kept, at `.ciabatta/.cache/authoritative/<step>/`,
because what the step could see when it failed is the whole question. A step
that declares no `inputs` at all isn't isolated — an empty directory would fail
it for reasons unrelated to its declarations — and is listed at the end as
unverified.

## The remote cache

A small server anyone can stand up, so a team's builds stop repeating each
other's work. It keeps artifacts on its own filesystem in the same layout the
local cache uses — no object store to provision, no database to migrate.

```bash
# On the server
ciabatta remote-cache init
ciabatta remote-cache start

# On each developer's machine
ciabatta remote-cache login http://cache.example.com:8380
ciabatta cache init --remote http://cache.example.com:8380
```

A project is known to the server by its **name and an id the server assigns**.
The id is written back into the workspace config and committed.

The remote is the one piece of cache config that stays in `ciabatta.yaml`: it's
a single server per checkout, and repeating it in every workflow file would be
four places to change when the server moves.

```yaml
# .ciabatta/ciabatta.yaml
cache:
  remote:
    url: http://cache.example.com:8380
    project: 7f3a-…        # assigned on first contact — commit this
```

That id is what makes every checkout and every CI runner resolve to the same
project. Names get reused and renamed; two teams both calling their repo `api`
must never end up silently sharing a cache, and the id is what prevents it.

### The server's own page

The cache server serves a small admin page at its root — open
`http://cache.example.com:8380/` in a browser. It shows the hit rate, what's
stored, and the ciabatta builds it hands out, and it does the one thing the CLI
does badly: **minting credentials**.

`ciabatta remote-cache add-user` prints a hash for you to paste into the config
and restart around, which is fine once and tiresome forever. The page writes the
user to the server's own list and hands back the token there and then. The token
is displayed exactly once — only its SHA-256 is kept — so if it's lost the
credential has to be reissued rather than recovered.

Two rules govern who may do that:

- On a **`token`** or **`ldap`** server, only an **admin** — a user with
  `admin: true`. Admin is granted in the operator's own config file, or by an
  existing admin.
- On an **`open`** server, anyone who can reach it. Open mode already means "I
  trust whoever is on this network", and refusing would leave no way to mint the
  first credential when locking the cache down. **But a user created on an open
  server is never an admin** — otherwise somebody could grant themselves lasting
  control while the door was open and keep it after it was shut.

So the migration from open to authenticated is: create the users you want on the
page, add one `admin: true` user to `auth.users` in the config, set
`auth.mode: token`, and restart. Server-managed users live in
`<storage>/users.json`; config-declared ones stay yours, and the page will
neither shadow nor delete them.

### TLS

The server speaks HTTP. Put it behind a reverse proxy with TLS for anything
beyond a trusted network. If that proxy uses a self-signed certificate, or an
internal CA a machine doesn't have installed, that machine can opt out of
verification:

```yaml
cache:
  remote:
    url: https://cache.example.com
    tls_verify: false     # defaults to true
```

`ciabatta remote-cache login --no-tls-verify` does the same for the login itself
and remembers it for later commands against that server.

Know what you're buying. With verification off, HTTPS is an encrypted channel to
whoever answered — so the build artifacts it hands back are only as trustworthy
as the network between you. Installing the CA certificate is the better fix
wherever it's available.

### Authentication

Authentication is `open`, `token`, or **LDAPS** against the directory you
already run:

```yaml
auth:
  mode: ldap
  ldap:
    url: ldaps://ldap.example.com:636
    bind_dn: "uid={username},ou=people,dc=example,dc=com"
    required_group: "cn=engineering,ou=groups,dc=example,dc=com"
    write_groups: ["cn=ci,ou=groups,dc=example,dc=com"]   # others are read-only
    tls_verify: true
```

Read access is a convenience; **write access is trust** — whoever can write to a
cache decides what everyone else's build produces. That's why `read_only` exists
on both a token user and an LDAP group: a fork's CI should benefit from the
cache without being able to poison it.

Cached artifacts are pruned on a retention policy, aged from **last use** rather
than from creation — the artifact everyone still depends on shouldn't be evicted
for being old:

```yaml
retention:
  max_age: 30d
  max_size: 10GB
```

### Running one locally

Everything above works on one machine, which is the sanest way to try the remote
cache before pointing a team at it. Two things to know first:

**The cache server and the ciabatta daemon are different processes.** The daemon
serves the web app on **8099**; the remote cache is its own server on **8380**.
They don't know about each other and don't share a port, so running both is just
picking two free ones.

**`remote-cache start` runs in the foreground.** It's a server — it holds the
terminal until you stop it. Use a second terminal, or background it.

```bash
# ── Terminal 1: the cache server ──────────────────────────────────────────
mkdir -p ~/scratch/ciabatta-cache && cd ~/scratch/ciabatta-cache
ciabatta remote-cache init --port 8380

# Loopback only: this one is for you, not the network. (`init` writes
# 0.0.0.0, which is right for a shared cache and wrong for a local test.)
sed -i 's/bind: 0.0.0.0/bind: 127.0.0.1/' remote-cache.yaml

ciabatta remote-cache start          # holds this terminal
```

```bash
# ── Terminal 2: the daemon and your project ───────────────────────────────
# Move the web app off 8099 if something else is using it.
ciabatta daemon restart --port 9099

cd ~/code/my-project
ciabatta remote-cache login http://127.0.0.1:8380
ciabatta cache init --enable --remote http://127.0.0.1:8380

ciabatta build                       # first build: uploads

rm -rf .ciabatta/cache dist          # pretend to be a colleague's machine
ciabatta build                       # "restored from the remote cache"

ciabatta remote-cache status         # hit rate, storage, retention
```

Open `http://127.0.0.1:8380/` while it's running: that's the server's own admin
page, and on an `open` cache you can mint a credential there and immediately use
it with `ciabatta remote-cache login`.

Wiping `.ciabatta/cache` along with the build output is the whole trick: it
leaves the workspace looking like a fresh checkout, so the only place the
artifacts can come back from is the server.

**On the daemon's port.** `--port` picks the port a daemon *starts* on; it does
not move one that's already running. A plain `ciabatta watch -p 9099` with a
healthy daemon on 8099 quietly keeps using 8099 — so change it with
`ciabatta daemon restart --port 9099`, or export `CIABATTA_DAEMON_PORT=9099`
before the first command that starts one. There's one daemon record
(`~/.ciabatta/daemon.json`), so there's one daemon at a time; the port moves,
rather than a second one appearing beside the first.

When you're done, Ctrl-C the server and `rm -rf ~/scratch/ciabatta-cache` —
everything it stored is under that directory, and the workspace's `cache.remote`
section is the only trace left in your project.

### Handing out ciabatta itself

A team on a shared cache already trusts one server and already talks to it on
every build, which makes it the obvious place to answer "is everyone on the same
ciabatta?".

```yaml
releases:
  version: "0.2.0"
  binaries:
    linux:   /srv/ciabatta/ciabatta-linux-x86_64
    windows: /srv/ciabatta/ciabatta-windows-x86_64.exe
```

The server hashes those, mentions the version in every reply, and tells a client
on something older. Then:

```bash
ciabatta self update --check    # is there one?
ciabatta self update            # install it
```

The download is checked against the advertised SHA-256 before anything on disk
is touched. The **hash** decides, not the version string — rebuild and copy a
new binary over the same path and your team still gets updated, because what's
advertised is the content, which is also what the client verifies.

Nothing updates automatically. A build tool that swaps its own binary out from
under a running CI job is a bad build tool; this notices, tells you, and waits
to be asked.

## Environment files

Four rules, in the order they apply:

1. **`.env` is the default.** A workspace that says nothing gets `.env` from its
   own directory. Nobody should have to configure the conventional thing.
2. **`env_file` overrides it** — and *replaces* it rather than adding to it,
   which is what "use this file instead" has to mean to be useful for keeping
   dev and prod settings apart.
3. **`env_default` is where a missing `.env` comes from.** `.env` is gitignored,
   so a fresh checkout doesn't have one; the checked-in template does. Naming it
   means ciabatta generates the `.env` rather than failing on a variable the
   developer has never heard of. A conventional template that's simply *there*
   — `.env.default`, `.env.example`, `.env.sample`, `.env.template` — counts
   without being declared: committing one has already said what the variables
   are. Generation happens at the start of a run, for the project and for every
   sub-workspace it touches, and never overwrites a file that exists.
4. **Nearest wins, then it looks outward.** A step in `packages/api` reads
   `packages/api/.env`; anything that file doesn't set comes from the workspace
   above it, up to the monorepo root. A *sibling's* `.env` is never a fallback —
   two packages that need the same variable declare it in the workspace above
   them, or each declares it for itself.

`REQUIRED_ENV` resolves up that same chain. A sub-library that needs `API_URL`
does **not** have to document it if the workspace above it already does —
demanding a template from every package that reads a shared variable would be
asking the same question once per package, and would make declaring it once
impossible. A run is refused only when *nothing* provides the variable: not the
package's own files, not any enclosing workspace's `.env` or checked-in
template, and not the environment the command is running in. Then the error says
where to put it:

```
Error: 'lib' declares environment variable(s) its build can't run without, and
nothing provides it: API_URL.

Looked in this workspace and every one enclosing it — their `.env` files, their
checked-in templates, and the environment this command is running in.

Set it in the environment, or write it down where whoever needs it will find it:

    packages/lib/.env   just this package
    .env                every package under the root
```

```yaml
workspace:
  env_file: .env               # the default; set it to override
  env_default: .env.default    # the checked-in template
```

```
.env                     SHARED=from-root   REGION=global
packages/api/.env        SHARED=from-api
packages/web/.env        SHARED=from-web

api:build   sees  SHARED=from-api   REGION=global    # its own file, then outward
web:build   sees  SHARED=from-web   REGION=global    # never api's
```

The environment a run *starts* with still beats every file, as it always has:
the shell, the CI system, and `-e KEY=VALUE` are on top of all of this. The run
page shows each step's chain, so "which `.env` did this value come from?" is a
question with a visible answer.

And one requirement that follows from the third: **a workspace whose workflows
declare `REQUIRED_ENV` must declare `env_default`.** Not bureaucracy — it's what
makes rule 3 possible. A repo where the required variables are written down
somewhere reviewable is a repo a new person can build; one where they aren't is
a repo where the answer lives in somebody's shell history.

`ciabatta watch` sources the same files a run would and prints exactly what it
resolved before the command starts, so a watched dev server and a `dev` workflow
step can't quietly see different environments.

## Build features

A **feature** is a build-shaping switch: telemetry compiled in or not, the new
UI or the old one, the fast test suite or the slow one. Any environment variable
named `CIABATTA_FEAT_<NAME>` is one.

```bash
CIABATTA_FEAT_NEW_UI=1 ciabatta build
```

There is nothing to declare. The name after the prefix is the feature — `new_ui`
above — matched case-insensitively with `-` and `_` treated alike. A value that
is empty, `0`, `false`, `no` or `off` turns the feature *off*; anything else
turns it on. The same variable set in any `.env` file the run sources counts
exactly the same, because features are read after the whole `env_file` chain has
been layered in.

A run says what it saw before it starts a step:

```
[build] features: new_ui (off: telemetry)
```

Steps gate on them the way they gate on anything else, with the feature spelled
as a feature rather than as a variable:

```yaml
steps:
  - name: bundle-new-ui
    run: yarn build:next
    when: "feature.new_ui"

  - name: bundle-legacy
    run: yarn build
    skip_if: "feature.new_ui"
```

`!feature.x` negates, and `CIABATTA_FEAT_NEW_UI` still works if you prefer to
write the variable out. Every step also gets `CIABATTA_FEATURES` — the enabled
features, sorted and comma-separated — for scripts that want to pass the whole
set on to something else rather than test one name.

**Features are part of the cache key, and you don't have to remember that.**
An artifact built with a feature on is not reusable by a build with it off, and
before this the only way to say so was to list the variable under `cache.env` —
where one forgotten line meant a build silently served the other
configuration's artifacts. Anything named with the prefix is in the key by
construction. A feature explicitly turned *off* is deliberately not in the key:
`CIABATTA_FEAT_X=0` produces the same artifacts as never mentioning `X`, and
giving them different keys would cost a rebuild to prove they were the same.

## Converting a script

A workflow step *is* a script. The only thing ciabatta adds is the declarations around
it — and that's the part nobody wants to write by hand.

```bash
ciabatta convert --script scripts/build.sh
```

It reads the script and does the tedious part: the tools it calls, the
environment variables it reads (and which have no fallback, so they belong in
`REQUIRED_ENV`), the files it writes, and the description sitting in its own
header comment. Everything it finds is printed for review before it's written,
and what it can't infer it leaves marked rather than guessing.

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
and derives publish points from your `kind: push` steps (and a publishable crate →
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
ciabatta push step — is **not** flagged as ciabatta-managed.

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

**Managed publish points.** Publish points that come from a ciabatta push step are
flagged **🍞 managed by ciabatta**, distinguishing them from inferred ones like
crates.io.

**Requirements & traceability.** Point `analyze` at a *requirements file* (one
requirement per line, `id` or `id, description`) to add a leftmost
**Requirements** column. A *trace file* — a CSV of `requirement,file`
connections — wires each requirement to the internal package that owns the
traced file(s), threading requirements through to the rest of the graph. Both
can be set on the command line or in config:

```yaml
analyze:
  requirements: docs/requirements.txt
  trace: docs/trace.csv
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
ciabatta release
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
| `/run` | Runs; `/run/<id>` is a live graph of one. |
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

`ciabatta watch` and `ciabatta <workflow> --gui` hand the work to the daemon rather
than running it in your terminal. That means:

- **Ctrl-C on `ciabatta watch` detaches, it doesn't kill.** The command keeps
  running and stays live in the browser. Stop it for real with
  `ciabatta watch --stop <ID>` or the Stop button.
- `ciabatta <workflow> --gui` returns as soon as the run starts. Closing the
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

## Editors

Ciabatta's config files are full of references to things defined in other
packages' files — the workflow a `needs:` points at, the tool a `requires:`
expects the root to know how to install, the registry a `push` step publishes
through. Get one wrong and the feedback arrives at build time, in someone
else's terminal.

The extensions in [`editors/`](editors/) move that feedback to the moment you
type it. There are two halves, and they know different things:

**JSON Schemas** describe the shape of the files — every field, what it takes,
what it's for. They're plain JSON Schema in [`editors/schemas/`](editors/schemas/),
so they need no binary and work in any editor with YAML support. A test in this
crate compares them against the serde structs field by field, so the
documentation you get while typing is the format ciabatta actually reads.

**`ciabatta lsp`** is the other half: a language server, and a subcommand of
the CLI you already have. It knows what a schema can't — which sub-workspaces
*this* monorepo contains, which workflows they define, which tools the root
promises to install:

```yaml
# .ciabatta/workflows/build.yaml
needs:
  - proto:g          # → proto:generate    Generate the protobufs
  -                  # → common            The shared library crate
```

```
needs: [protos]
        ~~~~~~
No sub-workspace here defines `protos`. Did you mean `proto`?
```

A step's `needs:` offers the steps in that file; a workflow's `needs:` offers
the other packages' workflows. Two fields spelled the same way that mean
different things, which is exactly the pair worth having an editor keep
straight.

| Editor | Install |
| --- | --- |
| VS Code | `ciabatta-vscode.vsix`, plus `cargo install ciabatta` for the server. See [`editors/vscode`](editors/vscode/). |
| Zed | The extension from a checkout, plus one settings block for the schemas. See [`editors/zed`](editors/zed/). |

Neither extension contains any knowledge of the format, which is why they
can't disagree with each other or with the build.

The VS Code extension isn't on the Marketplace, so there are three places to
get the `.vsix`, all of them the same file:

- **A running daemon** serves it at `127.0.0.1:8099/extensions`, and the
  Editors page of the web app has a download button. This is the copy built
  from the commit that built the binary, so the extension and the
  `ciabatta lsp` it launches can't be different versions.
- **The [releases page](https://github.com/forsyth-creations/ciabatta/releases/latest)**,
  alongside the binaries, and the [project site](https://forsyth-creations.github.io/Ciabatta/#editors)
  links straight at it.
- **A checkout**, with `yarn workspace ciabatta-vscode build`, which writes
  `editors/dist/ciabatta-vscode.vsix`.

Then either drag the file onto the Extensions panel or run
**Extensions: Install from VSIX…**.

## Web frontend

Two separate front ends live in this repo:

- **`tool_frontend/`** — the daemon's web app described above (React, MUI,
  TanStack, React Flow). It's compiled into the binary, so a release is still a
  single file. Build it with `yarn workspace ciabatta-tool-frontend build`;
  `yarn dev` inside `tool_frontend/` gives HMR against a running daemon.
- **`frontend/`** — the public docs site on GitHub Pages, with download links
  and usage instructions. See the
  [project site](https://forsyth-creations.github.io/Ciabatta/).

Building the Rust binary without them still works: the daemon serves a
placeholder page telling you to run the yarn build, and its Editors page offers
no download and points at the releases instead. CI and the release workflow
always build both first.

### Building it all

Ciabatta builds itself. `.ciabatta/workflows/build.yaml` orders the web app and
the extension ahead of the binary that embeds them, so one command does the
whole graph:

```bash
ciabatta build     # web app, extension, website, then the binary
ciabatta test      # fmt, clippy, the suite — exactly what CI gates on
```

That is also what CI runs, on all three platforms, which is why there is no
second list of build steps in `.github/workflows/ci.yml` to drift away from
this one. A checkout with no `ciabatta` on PATH bootstraps the same way CI
does — `cargo build --release` first, then the commands above — or drives the
JS half alone with `yarn build`.

## License

Licensed under the [MIT License](LICENSE).
