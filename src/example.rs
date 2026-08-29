//! `ciabatta init --example`: a worked monorepo you can actually run.
//!
//! Reading a config reference tells you what the fields are. It doesn't tell
//! you what a monorepo that uses them well looks like — which package owns the
//! generated protobufs, where the cross-package dependency gets declared, what
//! a publish step looks like next to the build that produced the artifact.
//!
//! So this generates the whole thing: four sub-workspaces with real
//! dependencies between them, workflows that span them, scripts on disk, a
//! toolchain section, tagged steps to filter on, a recovery node, a persistent
//! dev server, and a README explaining every decision. Every step runs `echo`
//! or `sh`, so the generated repo works on a machine with no toolchain
//! installed — `ciabatta build` in it succeeds on the first try, which is
//! the entire point of an example.
//!
//! Optional slices are opt-in because they need infrastructure the reader may
//! not have: [`Options::nexus`] adds a registry and publish steps,
//! [`Options::docker`] adds an image build and a deploy workflow.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

/// What to include in the generated example.
#[derive(Debug, Clone, Default)]
pub struct Options {
    /// Where to write it. Defaults to `./ciabatta-example`.
    pub into: Option<PathBuf>,
    /// Add a Nexus registry and a `release` workflow whose
    /// last node is a `kind = "push"` step.
    pub nexus: bool,
    /// Add a Dockerfile, a container registry, and a `deploy` workflow.
    pub docker: bool,
    /// Overwrite files that are already there.
    pub force: bool,
}

impl Options {
    /// Whether every optional slice was asked for.
    fn everything(&self) -> bool {
        self.nexus && self.docker
    }
}

/// One generated file: its path relative to the example root, and its contents.
struct File {
    path: &'static str,
    contents: String,
    /// Written with the executable bit on unix — the scripts have to be
    /// runnable or the example fails on its first step.
    executable: bool,
}

fn file(path: &'static str, contents: impl Into<String>) -> File {
    File {
        path,
        contents: contents.into(),
        executable: false,
    }
}

fn script(path: &'static str, contents: impl Into<String>) -> File {
    File {
        path,
        contents: contents.into(),
        executable: true,
    }
}

/// Generate the example repo and print what was written.
pub fn generate(options: &Options) -> Result<std::path::PathBuf> {
    let root = match &options.into {
        Some(path) => path.clone(),
        None => std::env::current_dir()
            .context("Failed to get current directory")?
            .join("ciabatta-example"),
    };

    let files = plan(options);

    // Check the whole plan before writing any of it: a half-written example is
    // worse than none, and "it already exists" should be one message rather
    // than a surprise three files in.
    if !options.force {
        let clashes: Vec<&str> = files
            .iter()
            .filter(|f| root.join(f.path).exists())
            .map(|f| f.path)
            .collect();
        if !clashes.is_empty() {
            bail!(
                "{} already has {} of the example's files (e.g. {}).\n\
                 Pass --force to overwrite, or --into <DIR> to write somewhere else.",
                root.display(),
                clashes.len(),
                clashes
                    .iter()
                    .take(3)
                    .copied()
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
    }

    for entry in &files {
        let path = root.join(entry.path);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("Failed to create {}", parent.display()))?;
        }
        std::fs::write(&path, &entry.contents)
            .with_context(|| format!("Failed to write {}", path.display()))?;
        if entry.executable {
            make_executable(&path)?;
        }
    }

    println!(
        "Created a ciabatta example monorepo in {} ({} files).",
        root.display(),
        files.len()
    );
    println!();
    println!("It contains four sub-workspaces that genuinely depend on each other:");
    println!("  proto   generates the API stubs everything else needs");
    println!("  common  shared library, built on the generated stubs");
    println!("  api     the service — depends on proto and common");
    println!("  web     the frontend — depends on api");
    if options.nexus {
        println!("  …plus a Nexus registry and a `release` workflow that publishes to it");
    }
    if options.docker {
        println!("  …plus a Dockerfile and a `deploy` workflow that builds and pushes an image");
    }
    println!();
    println!("Try it:");
    println!("  cd {}", root.display());
    println!("  ciabatta list                  what exists, and who owns it");
    println!("  ciabatta build --graph         explore the resolved graph, run nothing");
    println!("  ciabatta build                 run it — every step is an echo, so it works");
    println!("  ciabatta build test            both workflows as one graph");
    println!("  ciabatta test --filter tag:fast    just the fast steps");
    println!();
    println!("README.md in there explains every file and why it's shaped that way.");
    Ok(root)
}

#[cfg(unix)]
fn make_executable(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = std::fs::metadata(path)?.permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(path, perms)
        .with_context(|| format!("Failed to chmod {}", path.display()))
}

#[cfg(not(unix))]
fn make_executable(_path: &Path) -> Result<()> {
    Ok(())
}

/// Everything the example is made of, as (path, contents) pairs.
///
/// Building the whole list up front — rather than writing as we go — is what
/// makes the "already exists" check possible, and lets the tests assert on the
/// generated repo without touching a disk.
fn plan(options: &Options) -> Vec<File> {
    let mut files = vec![
        file("README.md", readme(options)),
        file(".gitignore", GITIGNORE),
        file(".env.example", ENV_EXAMPLE),
        file(".ciabatta/ciabatta.yaml", root_config(options)),
        // ─── proto ──────────────────────────────────────────────────────────
        file("packages/proto/.ciabatta/ciabatta.yaml", PROTO_CONFIG),
        file(
            "packages/proto/.ciabatta/workflows/generate.yaml",
            PROTO_GENERATE,
        ),
        script("packages/proto/scripts/generate.sh", PROTO_SCRIPT),
        file("packages/proto/api.proto", PROTO_FILE),
        // ─── common ─────────────────────────────────────────────────────────
        file("packages/common/.ciabatta/ciabatta.yaml", COMMON_CONFIG),
        file(
            "packages/common/.ciabatta/workflows/build.yaml",
            COMMON_BUILD,
        ),
        file("packages/common/.ciabatta/workflows/test.yaml", COMMON_TEST),
        // ─── api ────────────────────────────────────────────────────────────
        file("packages/api/.ciabatta/ciabatta.yaml", api_config(options)),
        file("packages/api/.ciabatta/workflows/build.yaml", API_BUILD),
        file("packages/api/.ciabatta/workflows/test.yaml", API_TEST),
        script("packages/api/scripts/build.sh", API_BUILD_SCRIPT),
        file("packages/api/.env", API_ENV),
        // ─── web ────────────────────────────────────────────────────────────
        file("packages/web/.ciabatta/ciabatta.yaml", WEB_CONFIG),
        file("packages/web/.ciabatta/workflows/build.yaml", WEB_BUILD),
        file("packages/web/.ciabatta/workflows/test.yaml", WEB_TEST),
        file("packages/web/.ciabatta/workflows/dev.yaml", WEB_DEV),
    ];

    if options.nexus {
        files.push(file(
            "packages/api/.ciabatta/workflows/release.yaml",
            API_RELEASE,
        ));
    }
    if options.docker {
        files.push(file("packages/api/Dockerfile", DOCKERFILE));
        files.push(file(
            "packages/api/.ciabatta/workflows/deploy.yaml",
            API_DEPLOY,
        ));
    }
    files
}

// ─── Root ───────────────────────────────────────────────────────────────────

const GITIGNORE: &str = "\
# Build output the workflows produce.
dist/
target/
packages/*/generated/

# Local environment. .env.example is checked in; the real one is not.
.env
!.env.example

# ciabatta's own cache (the .env drift snapshot).
.ciabatta/cache/
";

const ENV_EXAMPLE: &str = "\
# Copy to .env and fill in. Checked in so the *names* of the variables a build
# needs are reviewable — ciabatta notices when this file gains or loses one and
# tells you on the next run, so a pull that adds a required variable doesn't
# turn into a confusing failure ten minutes later.

# Which deployment this build is for. The api's `deploy` workflow keys off it.
DEPLOY_ENV=dev

# Where the api's tests point themselves.
API_URL=http://localhost:8080
";

/// The umbrella root: shared toolchain hints and variables, but not a package.
fn root_config(options: &Options) -> String {
    let mut out = String::from(
        r#"# The monorepo root.
#
# `umbrella: true` says this directory is not itself a package — it holds the
# shared `toolchain:` hints and standard variables every sub-workspace inherits,
# and stays out of `ciabatta list` as a package of its own.

workspace:
  umbrella: true
  description: Example monorepo showing how ciabatta orchestrates sub-workspaces

  # Standard variables every step in every package can count on. A sub-workspace
  # or a single step can add to these; the more specific one wins a collision.
  env:
    LOG_LEVEL: info

# ─── Toolchain ─────────────────────────────────────────────────────────────────
# What a step means when it says `requires: [protoc]`, and — crucially — how to
# get it. Written down once, here, so the person who hits "protoc: not found"
# gets the install command instead of a search engine.
#
# `check` is for tools a bare PATH lookup can't find (a plugin, a version).
toolchain:
  sh:
    description: POSIX shell — every step in this example runs through it
    hint: already on your machine

  protoc:
    description: Protocol buffer compiler
    hint: "brew install protobuf   (apt: apt-get install -y protobuf-compiler)"
    check: protoc --version
"#,
    );

    if options.docker {
        out.push_str(
            r#"
  docker:
    description: Container runtime for the deploy workflow
    hint: "https://docs.docker.com/get-docker/  (podman works too)"
"#,
        );
    }

    out.push_str(
        r#"
# ─── A workflow defined at the root ────────────────────────────────────────────
# Workflows usually live in packages, one file each. A small one can be written
# inline here instead — same engine, same command, just a matter of where it is
# convenient to write it down.
#
#   ciabatta smoke
#
workflows:
  smoke:
    description: Cheapest possible check that the repo is wired up
    steps:
      - name: ping
        run: "echo 'ciabatta example: everything is where it should be'"
        tags: [fast]
"#,
    );

    // Same again for `registries:`: whichever option comes first opens the key.
    if options.nexus || options.docker {
        out.push_str(
            r#"
# ─── Registries ────────────────────────────────────────────────────────────────
# Where built artifacts go.
#
# Credentials come from CIABATTA_<REGISTRY>_USER / CIABATTA_<REGISTRY>_PASS, or
# from a login_script. They are never written in this file.
registries:
"#,
        );
    }

    if options.nexus {
        out.push_str(
            r#"  nexus:
    url: https://nexus.example.com/repository/releases/
    needs_auth: true
    tls_verify: true
"#,
        );
    }

    if options.docker {
        out.push_str(
            r#"  ghcr:
    url: ghcr.io/example
    needs_auth: true
"#,
        );
    }

    out
}

// ─── proto ──────────────────────────────────────────────────────────────────

const PROTO_CONFIG: &str = r#"# The package that owns the API schema.
#
# Nothing here depends on anything: proto is the root of this monorepo's
# dependency graph, which is exactly why everything else has to wait for it.

workspace:
  name: proto
  description: Protobuf definitions and the stubs generated from them
  owner: Platform Team
  tags: [codegen, schema]
  depends_on: []
  requires: [sh]
"#;

const PROTO_GENERATE: &str = r#"# `ciabatta generate` — or, more usefully, whatever pulls this in.
#
# Note the name: this workflow is NOT called "build". Other packages depend on
# it explicitly with `depends_on: [proto:generate]`, which is how a package says
# "I need the stubs" rather than "I need whatever proto calls a build".

description: Generate the client/server stubs from api.proto
owner: Platform Team
tags: [codegen]

# A real project would put `requires: [protoc]` here and the graph would refuse
# to start without it, printing the `toolchain.protoc` hint from the root
# config. This example uses sh so it runs anywhere.
requires: [sh]

steps:
  - name: generate
    description: Write generated/api.stub.txt from api.proto
    script: scripts/generate.sh
    tags: [codegen]
    # A hung codegen must not hold up the whole monorepo.
    timeout: 2m
"#;

const PROTO_SCRIPT: &str = r#"#!/bin/sh
# Stands in for `protoc --rust_out=generated api.proto`.
#
# Scripts run from their own sub-workspace directory, so these relative paths
# are the ones you'd write if you ran the script by hand.
set -eu

mkdir -p generated
{
  echo "// Generated from api.proto — do not edit."
  echo "// Generated at: $(date -u '+%Y-%m-%dT%H:%M:%SZ')"
  grep '^message\|^rpc\|^service' api.proto || true
} > generated/api.stub.txt

echo "proto: wrote generated/api.stub.txt"
"#;

const PROTO_FILE: &str = r#"syntax = "proto3";
package example;

service Api {
  rpc GetThing (GetThingRequest) returns (Thing);
}

message GetThingRequest {
  string id = 1;
}

message Thing {
  string id = 1;
  string name = 2;
}
"#;

// ─── common ─────────────────────────────────────────────────────────────────

const COMMON_CONFIG: &str = r#"# A shared library, built on the generated stubs.
#
# `depends_on` is declared once at the package level and applies to every
# workflow here — "we always need the stubs first" is one line, not one line per
# workflow.

workspace:
  name: common
  description: Shared helpers used by both the api and the web app
  owner: Platform Team
  tags: [library]
  depends_on: [proto:generate]
  requires: [sh]

  env:
    # Every step in this package sees this, on top of the root's workspace env.
    COMMON_STRICT: "1"
"#;

const COMMON_BUILD: &str = r#"description: Compile the shared library
owner: Platform Team

steps:
  - name: compile
    description: Build common against the generated stubs
    run: "mkdir -p dist && echo 'common built' > dist/common.txt && echo 'common: built'"
    tags: [library]
"#;

const COMMON_TEST: &str = r#"description: Test the shared library
owner: Platform Team

# This workflow needs common's own build, not just proto's stubs. `self:` names
# another workflow in this same package.
needs: [self:build]

steps:
  - name: unit
    description: Fast unit tests — no network, no fixtures
    run: "echo 'common: 42 tests passed'"
    tags: [fast]
"#;

// ─── api ────────────────────────────────────────────────────────────────────

fn api_config(options: &Options) -> String {
    let mut out = String::from(
        r#"# The service. This is the package with the interesting dependencies.
#
# It needs proto's generated stubs and common's compiled library, and says so —
# so `ciabatta build` from anywhere in the repo runs proto, then common,
# then this, in that order, without anyone having to remember it.

workspace:
  name: api
  description: The public REST/gRPC service
  owner: API Team
  tags: [backend, service]
  depends_on: [proto:generate, common]
  requires: [sh]

  # Ciabatta sources `.env` from the package by default, so this line only makes
  # that explicit. Point it somewhere else to override it, and name the
  # checked-in template it's generated from with `env_default`.
  env_file: .env
  env_default: .env.example
"#,
    );

    if options.nexus || options.docker {
        out.push_str(
            r#"
# The publish steps in this package's release/deploy workflows name a registry
# defined at the monorepo root, so credentials and registry URLs live in one
# place rather than being repeated per package.
"#,
        );
    }
    out
}

const API_BUILD: &str = r#"description: Build the api binary
owner: API Team
requires: [sh]

# Refuse to start unless these are set, rather than failing halfway through with
# something unhelpful. Sourced from .env, the environment, CI, or -e flags.
REQUIRED_ENV: [API_URL]

steps:
  - name: compile
    description: Compile the service binary into dist/api
    script: scripts/build.sh
    tags: [slow]
    timeout: 10m
    # Transient failures (a flaky mirror, a busy disk) get one more go before the
    # graph gives up on this branch.
    retries: 1
    # …and if it still fails, route to the recovery node below instead of taking
    # the whole run down with it.
    on_error: fix-build

  - name: package
    description: Tar the binary up for publishing
    run: "tar czf dist/api.tgz -C dist api && echo 'api: packaged dist/api.tgz'"
    needs: [compile]
    tags: [slow]

  # A recovery node: not part of the success graph, entered only when `compile`
  # fails. In a terminal you're offered the choice; in the web view it's a
  # button. `retry` re-runs the failed step once a fix succeeds.
  - name: fix-build
    description: The build failed — try to clear it up and go again
    recover: true
    retry: compile
    message: "api failed to build. What should I try?"
    options:
      - label: Clean the output directory and rebuild
        run: "rm -rf dist && echo 'api: cleaned dist/'"
        default: true

      - label: Regenerate the stubs, in case they're stale
        run: cd ../proto && sh scripts/generate.sh
"#;

const API_TEST: &str = r#"description: Test the api
owner: API Team
needs: [self:build]

# Tagged steps are what `--filter` selects on:
#   ciabatta test --filter tag:fast      the quick loop
#   ciabatta test --filter '!tag:flaky'  everything but the unreliable ones
steps:
  - name: unit
    description: Unit tests — no I/O, milliseconds
    run: "echo 'api: 128 unit tests passed'"
    tags: [fast]

  - name: integration
    description: Talks to a database; slower, and occasionally flaky
    run: "echo 'api: 14 integration tests passed against $API_URL'"
    needs: [unit]
    tags: [slow, flaky]
    retries: 2
    # One flaky suite shouldn't fail the whole monorepo's test run: this branch
    # reports its failure at the end and everything else carries on.
    continue_on_error: true
"#;

const API_BUILD_SCRIPT: &str = r##"#!/bin/sh
# Stands in for a real compile.
#
# Runs from packages/api/ — its own sub-workspace — so these paths are the ones
# you'd type if you ran it yourself. The generated stubs are read from the proto
# package, which is why this package declares depends_on = ["proto:generate"].
set -eu

if [ ! -f ../proto/generated/api.stub.txt ]; then
  echo "api: the generated stubs are missing — did proto:generate run?" >&2
  exit 1
fi

mkdir -p dist
{
  echo "#!/bin/sh"
  echo "echo 'api service, built against:'"
  echo "cat <<'STUB'"
  cat ../proto/generated/api.stub.txt
  echo "STUB"
} > dist/api
chmod +x dist/api

echo "api: built dist/api (API_URL=${API_URL:-unset}, LOG_LEVEL=${LOG_LEVEL:-unset})"
"##;

const API_ENV: &str = r#"# Sourced before any api workflow runs. Values already set in the environment,
# in CI, or with -e win over these, so this file is defaults rather than truth.
#
# Change a value here and run `ciabatta build` again: ciabatta notices and
# says which variable moved, because a changed environment is one of the harder
# build failures to diagnose from the error alone.
API_URL=http://localhost:8080
API_TIMEOUT=30s
"#;

const API_RELEASE: &str = r#"# Publishing is a node on the graph, not a separate command.
#
# A `kind: push` step names the registry and the path itself, and declares what
# it `needs` — so it cannot possibly run before the artifact it publishes
# exists. That ordering is the whole reason it belongs on the graph.
#
#   ciabatta release
#   ciabatta release --filter kind:push    (just the publish, artifact in hand)

description: Build, package, and publish the api
owner: API Team
needs: [self:build]
tags: [release]

REQUIRED_ENV: [CIABATTA_NEXUS_USER, CIABATTA_NEXUS_PASS]

steps:
  - name: verify
    description: Refuse to publish something that isn't there
    run: "test -f dist/api.tgz && echo 'api: artifact present'"
    tags: [release]

  - name: publish
    description: Upload the packaged binary to Nexus
    kind: push
    needs: [verify]
    tags: [release]
    registry: nexus
    artifact: dist/api.tgz
    # The CIABATTA_* variables are resolved from git (or from CI) before the push.
    publish_path: "example/api/{CIABATTA_BRANCH}/{CIABATTA_COMMIT}/api.tgz"
"#;

const DOCKERFILE: &str = r#"# Built by the `deploy` workflow. Deliberately tiny — this example is about
# the orchestration around the image, not the image.
FROM alpine:3.20
WORKDIR /app
COPY dist/api /app/api
CMD ["/app/api"]
"#;

const API_DEPLOY: &str = r#"# Build an image and ship it.
#
# This workflow shows conditions: `when` gates a step on the environment, so one
# graph covers dev and prod instead of two nearly-identical files.
#
#   ciabatta deploy -e DEPLOY_ENV=prod

description: Build the container image and deploy it
owner: API Team
needs: [self:build]
tags: [deploy]
requires: [sh]

REQUIRED_ENV: [DEPLOY_ENV]

steps:
  - name: image
    description: Build the container image from packages/api/Dockerfile
    # A real project drops the echo:
    #   docker build -t ghcr.io/example/api:$CIABATTA_COMMIT .
    run: 'echo "api: would run docker build -t ghcr.io/example/api:${CIABATTA_COMMIT:-dev} ."'
    tags: [deploy]
    timeout: 15m

  - name: push-image
    description: Push the image to the registry
    kind: push
    needs: [image]
    tags: [deploy]
    registry: ghcr
    local_image: "ghcr.io/example/api:{CIABATTA_COMMIT}"

  - name: smoke
    description: Hit the deployed service once to prove it came up
    run: "echo 'api: smoke test passed'"
    needs: [push-image]
    tags: [deploy]

  - name: announce
    description: Tell the team — production only
    run: "echo 'api: announcing the production deploy'"
    needs: [smoke]
    # Only runs when the condition holds; otherwise it's skipped and anything
    # depending on it still goes ahead.
    when: env.DEPLOY_ENV == prod
    tags: [deploy]
"#;

// ─── web ────────────────────────────────────────────────────────────────────

const WEB_CONFIG: &str = r#"# The frontend. Depends on the api — a bare package name (rather than
# "api:build") means "whatever api calls this same workflow".
#
# So `ciabatta build` waits for api's build, and `ciabatta test` waits
# for api's test, from this one declaration.

workspace:
  name: web
  description: The browser app
  owner: Web Team
  tags: [frontend]
  depends_on: [api]
  requires: [sh]
"#;

const WEB_BUILD: &str = r#"description: Bundle the frontend
owner: Web Team

steps:
  - name: bundle
    description: Produce dist/bundle.js
    run: "mkdir -p dist && echo '// bundled' > dist/bundle.js && echo 'web: bundled'"
    tags: [frontend, slow]
"#;

const WEB_TEST: &str = r#"description: Test the frontend
owner: Web Team
needs: [self:build]

steps:
  - name: unit
    description: Component tests
    run: "echo 'web: 61 component tests passed'"
    tags: [fast, frontend]
"#;

const WEB_DEV: &str = r#"# A workflow with a step that never exits.
#
# `persistent: true` starts the dev server, releases everything downstream
# immediately, and hands the process to the ciabatta daemon — so it keeps
# running after the graph finishes instead of hanging it forever. The run prints
# a session id:
#
#   ciabatta watch --attach <ID>    follow its output
#   ciabatta watch --list           find it again later
#   ciabatta watch --stop <ID>      stop it

description: Run the app locally against a live api
owner: Web Team
tags: [dev]

steps:
  - name: serve
    description: The dev server — keeps running after this workflow finishes
    run: "echo 'web: dev server listening on http://localhost:3000' && sleep 3600"
    persistent: true
    tags: [dev]

  - name: open
    description: Print where to go, once the server is up
    run: "echo 'web: open http://localhost:3000'"
    needs: [serve]
    tags: [dev]
"#;

// ─── README ─────────────────────────────────────────────────────────────────

/// The generated README: what's here, how to run it, and the habits the layout
/// is trying to establish.
fn readme(options: &Options) -> String {
    let mut out = String::from(
        r#"# Ciabatta example monorepo

A worked example, generated by `ciabatta init --example`. Everything in here
runs: each step is an `echo` or a small shell script, so you can execute the
whole graph on a machine with no toolchain installed and watch what happens.

Start here:

```sh
ciabatta list                    # everything this repo can do, and who owns it
ciabatta build --graph       # explore the resolved graph; runs nothing
ciabatta build               # actually run it
```

## The problem this shape solves

A monorepo accumulates scripts nobody owns, that quietly depend on each other in
ways nobody wrote down. You find out that `api` needs `proto`'s generated stubs
when the build fails on a fresh checkout — and the person who knew that left.

Ciabatta's answer is that dependencies are **declared, in the package that has
them**, and the resulting graph is **shown to you before it runs**.

## What's in here

```
.ciabatta/ciabatta.yaml       the monorepo root: shared toolchain + variables
packages/
  proto/                      the API schema, and the stubs generated from it
  common/                     shared library — needs proto's stubs
  api/                        the service — needs proto and common
  web/                        the frontend — needs api
```

The dependency graph those declarations add up to:

```
proto:generate ──▶ common:build ──┐
       │                          ├──▶ api:build ──▶ web:build
       └──────────────────────────┘
```

Nobody wrote that graph down. Each package declared what *it* needs, and
`ciabatta build` worked out the rest.

## Where each idea lives

| What | Where to look |
| --- | --- |
| Cross-package dependency | `packages/api/.ciabatta/ciabatta.yaml` → `depends_on` |
| Depending on one specific workflow | `depends_on = ["proto:generate"]` in the same file |
| Depending on another workflow in the same package | `needs: [self:build]` in `packages/api/.ciabatta/workflows/test.yaml` |
| Ownership and descriptions | every `ciabatta.yaml` and every step |
| Toolchain hints | `.ciabatta/ciabatta.yaml` → `toolchain:` |
| Tags, for `--filter` | steps in `packages/api/.ciabatta/workflows/test.yaml` |
| Timeouts and retries | `packages/api/.ciabatta/workflows/build.yaml` |
| A recovery node (`on_error`) | `packages/api/.ciabatta/workflows/build.yaml` → `fix-build` |
| A step that never exits | `packages/web/.ciabatta/workflows/dev.yaml` → `persistent` |
| Required variables | `packages/api/.ciabatta/workflows/build.yaml` → `REQUIRED_ENV` |
| `.env` files | `packages/api/.env`, and `.env.example` at the root |
| A workflow written inline rather than in its own file | `.ciabatta/ciabatta.yaml` → `workflows.smoke` |
"#,
    );

    if options.nexus {
        out.push_str(
            "| Publishing as a graph node | `packages/api/.ciabatta/workflows/release.yaml` |\n\
             | Registry and credentials | `.ciabatta/ciabatta.yaml` → `registries.nexus` |\n",
        );
    }
    if options.docker {
        out.push_str(
            "| Container build and deploy | `packages/api/.ciabatta/workflows/deploy.yaml` |\n\
             | Conditional steps (`when`) | the `announce` step in that file |\n",
        );
    }

    out.push_str(
        r#"
## Running things

There is one command, because there is one thing to run. A **workflow** is a
named DAG of steps; every package that declares that name joins in, and the
whole thing compiles to a single graph.

```sh
ciabatta build                  # every package's `build`, in dependency order
ciabatta build test             # both, compiled into ONE graph
ciabatta smoke                  # a workflow written inline at the root
ciabatta build --only api       # start from api (its dependencies still run)
ciabatta build --only api --isolated   # just api, nothing upstream
ciabatta build --dry-run        # walk every step, execute nothing
ciabatta build --gui            # watch it live in a browser
```

`ciabatta build` (no `run`) does the same thing — any name ciabatta doesn't
recognise as a command is treated as a workflow.

### Seeing the graph first

```sh
ciabatta build --graph
```

Prints exactly what would run: every step in wave order, and for each one what
it does, who owns it, what it's waiting for, and which tools it needs. Nothing
executes. Add `--tui` for an interactive view of the same thing, one node at a
time — arrow keys move, `q` quits.

### Running part of a graph

`--filter` prunes the graph to the steps you care about:

```sh
ciabatta test --filter tag:fast            # only steps tagged fast
ciabatta test --filter '!tag:flaky'        # everything except the flaky ones
ciabatta build --filter workspace:api      # only the api package's steps
ciabatta test --filter tag:fast --filter tag:frontend   # either one
```

Selectors are `tag:`, `workspace:` (or `member:`), `kind:`, `owner:`, `step:`,
or a bare word that searches all of them. `!` in front excludes.

A filter **prunes** rather than expands: the steps that survive run without the
dependencies you filtered away, on the assumption those already happened. It's
the fast loop for debugging one component — not how you build a fresh checkout.
Ciabatta tells you which dependencies it cut, every time.

## Environment variables

`.env` files are sourced before a run; `packages/api/.env` shows the shape.
Precedence, weakest first: `.env` file → CI-derived → ambient environment →
`-e KEY=VALUE` on the command line.

Ciabatta snapshots the variables these files define (names and value *hashes* —
never the values) into `.ciabatta/cache/`. When they change — someone pulls a
branch that adds a required variable — the next run says so before it starts,
rather than failing later with an error about something else. Try it: edit
`packages/api/.env` and run `ciabatta build` again.

## Best practices this example is demonstrating

1. **Every package declares what it needs.** Not a build script that happens to
   `cd ../proto` — a `depends_on` line that the graph can read.
2. **Every script has a description and an owner.** They show up in
   `ciabatta list`, so nobody has to open a file to find out what it does or who
   to ask. `ciabatta init --lib` nags you when they're missing, on purpose.
3. **Name workflows for what they accomplish**, and the same thing across
   packages. `build` means build everywhere; that shared name is what lets one
   command run all of them.
4. **Tag steps by cost and kind.** `fast`, `slow`, `flaky`, `integration` —
   they're what makes `--filter` useful six months from now.
5. **Put install instructions in `toolchain:`.** The person who hits
   "protoc: not found" should get the fix, not a search engine.
6. **Give long steps a `timeout`,** flaky ones `retries`, and non-critical ones
   `continue_on_error`. One bad step shouldn't hold up the repo.
7. **Publishing belongs on the graph.** A `kind = "push"` step can't run before
   the artifact exists, which a separate `push` command absolutely can.

## Next steps

- `ciabatta list --search backend` — search everything by name, owner, or tag.
- `ciabatta init --lib` in a new directory — opt another package in.
- `ciabatta config reference` — the full schema.
"#,
    );

    if !options.everything() {
        out.push_str("\nThis example was generated without ");
        let missing: Vec<&str> = [
            (!options.nexus).then_some("--nexus"),
            (!options.docker).then_some("--docker"),
        ]
        .into_iter()
        .flatten()
        .collect();
        out.push_str(&missing.join(" or "));
        out.push_str(
            ". Re-run `ciabatta init --example` with \
             those flags for publishing and container-deploy examples too.\n",
        );
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ciab_example_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    /// Every generated config must actually parse as the schema it claims to
    /// be — an example that doesn't load is worse than no example.
    #[test]
    fn every_generated_config_parses() {
        let options = Options {
            nexus: true,
            docker: true,
            ..Default::default()
        };
        // The optional slices add entries under `registries:`, which in YAML
        // means extending one key rather than repeating a header. Check each
        // combination lands everything it should.
        for (nexus, docker, registries, workflows) in [
            (false, false, vec![], vec!["smoke"]),
            (true, false, vec!["nexus"], vec!["smoke"]),
            (false, true, vec!["ghcr"], vec!["smoke"]),
            (true, true, vec!["nexus", "ghcr"], vec!["smoke"]),
        ] {
            let opts = Options {
                nexus,
                docker,
                ..Default::default()
            };
            let rendered = root_config(&opts);
            let cfg: crate::config::CiabattaConfig = crate::format::from_str(
                &rendered,
                crate::format::Format::Yaml,
            )
            .unwrap_or_else(|e| {
                panic!("root config (nexus={nexus}, docker={docker}) broke: {e}\n{rendered}")
            });

            let mut got: Vec<&str> = cfg.registries.keys().map(|s| s.as_str()).collect();
            got.sort_unstable();
            let mut want = registries.clone();
            want.sort_unstable();
            assert_eq!(got, want, "registries with nexus={nexus}, docker={docker}");

            let mut got: Vec<&str> = cfg.workflows.keys().map(|s| s.as_str()).collect();
            got.sort_unstable();
            let mut want = workflows.clone();
            want.sort_unstable();
            assert_eq!(got, want, "workflows with nexus={nexus}, docker={docker}");
        }

        for entry in plan(&options) {
            if !crate::format::is_config_file(std::path::Path::new(entry.path)) {
                continue;
            }
            let format = crate::format::Format::of_path(std::path::Path::new(entry.path));
            if entry.path.ends_with("ciabatta.yaml") {
                crate::format::from_str::<crate::config::CiabattaConfig>(&entry.contents, format)
                    .unwrap_or_else(|e| panic!("{} does not parse as a config: {e}", entry.path));
            } else {
                crate::format::from_str::<crate::workspace::Workflow>(&entry.contents, format)
                    .unwrap_or_else(|e| panic!("{} does not parse as a workflow: {e}", entry.path));
            }
        }
    }

    #[test]
    fn the_optional_slices_are_opt_in() {
        let bare: Vec<&str> = plan(&Options::default()).iter().map(|f| f.path).collect();
        assert!(!bare.iter().any(|p| p.contains("release.yaml")));
        assert!(!bare.iter().any(|p| p.contains("Dockerfile")));

        let full = Options {
            nexus: true,
            docker: true,
            ..Default::default()
        };
        let paths: Vec<&str> = plan(&full).iter().map(|f| f.path).collect();
        assert!(paths.iter().any(|p| p.contains("release.yaml")));
        assert!(paths.iter().any(|p| p.contains("Dockerfile")));
        assert!(paths.iter().any(|p| p.contains("deploy.yaml")));
    }

    /// The generated repo has to load as a workspace with the dependency graph
    /// the README claims it has.
    #[test]
    fn the_generated_repo_compiles_the_graph_the_readme_describes() {
        let root = scratch("graph");
        generate(&Options {
            into: Some(root.clone()),
            nexus: true,
            docker: true,
            force: true,
        })
        .unwrap();

        let ws = crate::workspace::Workspace::load(&root).unwrap();
        let mut names: Vec<&str> = ws.members.iter().map(|m| m.name.as_str()).collect();
        names.sort();
        assert_eq!(names, vec!["api", "common", "proto", "web"]);

        let graph = crate::workspace::graph::build(
            &ws,
            "build",
            &crate::workspace::graph::Selection::default(),
        )
        .unwrap();

        // proto's stubs come first, and web's bundle comes last — the ordering
        // the README draws, derived only from the declarations.
        let order: Vec<&str> = graph.steps.iter().map(|s| s.name.as_str()).collect();
        let position = |needle: &str| order.iter().position(|n| *n == needle).unwrap();
        assert!(position("proto:generate") < position("common:compile"));
        assert!(position("common:compile") < position("api:compile"));
        assert!(position("api:compile") < position("web:bundle"));

        // Tags cascaded, so the filters the README advertises actually select.
        let filters = crate::run::filter::parse_all(&["tag:fast".to_string()]).unwrap();
        let test_graph = crate::workspace::graph::build(
            &ws,
            "test",
            &crate::workspace::graph::Selection::default(),
        )
        .unwrap();
        let (kept, _) = crate::run::filter::apply(&test_graph.steps, &filters).unwrap();
        assert!(
            !kept.is_empty(),
            "--filter tag:fast should select something"
        );
        assert!(kept.iter().all(|s| s.tags.iter().any(|t| t == "fast")));

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn generating_twice_is_refused_unless_forced() {
        let root = scratch("twice");
        let options = Options {
            into: Some(root.clone()),
            ..Default::default()
        };
        generate(&options).unwrap();

        let err = generate(&options).unwrap_err().to_string();
        assert!(err.contains("--force"), "{err}");

        generate(&Options {
            force: true,
            ..options
        })
        .unwrap();

        std::fs::remove_dir_all(&root).ok();
    }

    #[cfg(unix)]
    #[test]
    fn the_scripts_are_executable() {
        use std::os::unix::fs::PermissionsExt;
        let root = scratch("exec");
        generate(&Options {
            into: Some(root.clone()),
            ..Default::default()
        })
        .unwrap();

        let script = root.join("packages/proto/scripts/generate.sh");
        let mode = std::fs::metadata(&script).unwrap().permissions().mode();
        assert!(mode & 0o111 != 0, "generate.sh must be runnable: {mode:o}");

        std::fs::remove_dir_all(&root).ok();
    }
}
