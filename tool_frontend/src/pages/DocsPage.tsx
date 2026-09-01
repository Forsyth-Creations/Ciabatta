/**
 * The manual, shipped inside the app.
 *
 * Docs live here rather than only in the README because of where this app runs:
 * it is embedded in the `ciabatta` binary and served by a daemon on loopback,
 * often on a machine that is mid-build, offline, or running a version that
 * isn't whatever `main` says today. Documentation that ships in the same
 * bundle as the UI can't drift from the UI, and is readable without leaving
 * the tab.
 *
 * Scope is deliberately "how do I use this app, and what is the API underneath
 * it" — not a copy of the README's install/CLI material, which belongs with the
 * CLI. Where a page has a CLI equivalent, this says so and stops there.
 *
 * The section list drives both the table of contents and the body, so a new
 * section can't be added to one and forgotten in the other.
 */

import BoltIcon from "@mui/icons-material/Bolt";
import CheckIcon from "@mui/icons-material/Check";
import ContentCopyIcon from "@mui/icons-material/ContentCopy";
import DownloadIcon from "@mui/icons-material/Download";
import {
  Alert,
  Box,
  Button,
  Chip,
  Divider,
  Grid2 as Grid,
  List,
  ListItemButton,
  ListItemText,
  Stack,
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableRow,
  Typography,
} from "@mui/material";
import { Link } from "@tanstack/react-router";
import { Fragment, useState } from "react";
import type { ReactNode } from "react";

import { PageHeader } from "../components/Page";
import { useEditorExtensions, useHealth } from "../api/queries";
import { monoFontStack } from "../theme";

/** Clears the fixed app bar when the browser jumps to an anchor. */
const ANCHOR_OFFSET = 84;

// ─── Small typographic helpers ──────────────────────────────────────────────

/** Inline code: paths, endpoints, commands mentioned mid-sentence. */
function C({ children }: { children: ReactNode }) {
  return (
    <Box
      component="code"
      sx={{
        fontFamily: monoFontStack,
        fontSize: "0.875em",
        px: 0.5,
        py: 0.125,
        borderRadius: 0.5,
        bgcolor: "action.hover",
        wordBreak: "break-word",
      }}
    >
      {children}
    </Box>
  );
}

/** A block of shell or YAML to copy. */
function Pre({ children }: { children: string }) {
  return (
    <Box
      component="pre"
      sx={{
        fontFamily: monoFontStack,
        fontSize: 13,
        lineHeight: 1.6,
        p: 1.5,
        my: 2,
        borderRadius: 1,
        border: 1,
        borderColor: "divider",
        bgcolor: "action.hover",
        overflowX: "auto",
      }}
    >
      {children}
    </Box>
  );
}

function P({ children }: { children: ReactNode }) {
  return (
    <Typography variant="body2" sx={{ mb: 1.5, maxWidth: "78ch", lineHeight: 1.7 }}>
      {children}
    </Typography>
  );
}

function Bullets({ items }: { items: ReactNode[] }) {
  return (
    <Box component="ul" sx={{ pl: 3, mb: 2, maxWidth: "78ch" }}>
      {items.map((item, index) => (
        <Typography key={index} component="li" variant="body2" sx={{ mb: 0.75, lineHeight: 1.7 }}>
          {item}
        </Typography>
      ))}
    </Box>
  );
}

function SubHeading({ children }: { children: ReactNode }) {
  return (
    <Typography variant="h3" sx={{ mt: 3, mb: 1 }}>
      {children}
    </Typography>
  );
}

// ─── The API reference table ────────────────────────────────────────────────

interface Endpoint {
  method: "GET" | "POST" | "DELETE";
  path: string;
  note: string;
}

interface EndpointGroup {
  name: string;
  /** Whether these routes want a `?project=<id>` (or a `project` field). */
  scoped: boolean;
  endpoints: Endpoint[];
}

const ENDPOINTS: EndpointGroup[] = [
  {
    name: "Daemon",
    scoped: false,
    endpoints: [
      { method: "GET", path: "/api/health", note: "Liveness, version, pid. The one route with no token." },
      { method: "POST", path: "/api/shutdown", note: "Ask the daemon to exit gracefully." },
      { method: "GET", path: "/api/projects", note: "Registered checkouts, newest first." },
      { method: "POST", path: "/api/projects", note: "Register a checkout by path." },
      { method: "DELETE", path: "/api/projects/{id}", note: "Forget a checkout. The files are untouched." },
    ],
  },
  {
    name: "Todo",
    scoped: false,
    endpoints: [
      { method: "GET", path: "/api/todos", note: "The whole list." },
      { method: "POST", path: "/api/todos", note: "Add a task." },
      { method: "POST", path: "/api/todos/toggle", note: "Mark done or not done." },
      { method: "POST", path: "/api/todos/edit", note: "Change a task's text." },
      { method: "POST", path: "/api/todos/priority", note: "Set low, medium, or high." },
      { method: "POST", path: "/api/todos/delete", note: "Remove a task." },
      { method: "POST", path: "/api/todos/ship", note: "Hand a task to the assistant. Needs a project — the agent edits files." },
    ],
  },
  {
    name: "Watch",
    scoped: true,
    endpoints: [
      { method: "GET", path: "/api/watch/sessions", note: "Every session the daemon owns." },
      { method: "POST", path: "/api/watch/sessions", note: "Start a command under a new session." },
      { method: "GET", path: "/api/watch/sessions/{id}", note: "A snapshot: recent lines, bookmarks, triggers." },
      { method: "GET", path: "/api/watch/sessions/{id}/stream", note: "SSE. A frame per batch of lines, and on exit." },
      { method: "GET", path: "/api/watch/sessions/{id}/search", note: "Search the full buffer: q, mode=any|all, regex." },
      { method: "GET", path: "/api/watch/sessions/{id}/export", note: "The whole session as a text transcript, as a download. ?timestamps=true." },
      { method: "POST", path: "/api/watch/sessions/{id}/stop", note: "Stop the process, keep the output." },
      { method: "DELETE", path: "/api/watch/sessions/{id}", note: "Discard the session and its output." },
      { method: "POST", path: "/api/watch/sessions/{id}/bookmarks", note: "Pin a line. /bookmarks/delete removes one." },
      { method: "POST", path: "/api/watch/sessions/{id}/triggers", note: "Watch for a pattern. /triggers/delete removes one." },
    ],
  },
  {
    name: "Workspace",
    scoped: true,
    endpoints: [
      { method: "GET", path: "/api/workspace", note: "The catalogue: members, their workflows, the toolchain." },
      { method: "GET", path: "/api/workspace/graph", note: "One workflow compiled across packages. Takes workflow=." },
      { method: "GET", path: "/api/workspace/env-drift", note: "Which .env variables changed since ciabatta last ran here. A peek: it never acknowledges the drift." },
    ],
  },
  {
    name: "Run",
    scoped: true,
    endpoints: [
      { method: "GET", path: "/api/run/workflows", note: "Workflow names this project can run." },
      { method: "POST", path: "/api/run/preflight", note: "What a start would need, without starting it." },
      { method: "GET", path: "/api/run/runs", note: "Runs the daemon owns." },
      { method: "POST", path: "/api/run/runs", note: "Start a workflow (workflow, or workflows: [], plus filter: []). 422 lists missing_env." },
      { method: "GET", path: "/api/run/runs/{id}", note: "Current state of every step." },
      { method: "GET", path: "/api/run/runs/{id}/stream", note: "SSE. Step transitions and log lines as they happen." },
      { method: "POST", path: "/api/run/runs/{id}/choose", note: "Answer a step that is waiting on a decision." },
    ],
  },
  {
    name: "Analyze",
    scoped: true,
    endpoints: [
      { method: "GET", path: "/api/analyze/graph", note: "The last scan's dependency graph." },
      { method: "POST", path: "/api/analyze/scans", note: "Start a fresh scan." },
      { method: "GET", path: "/api/analyze/status", note: "Whether a scan is in flight for this project." },
    ],
  },
  {
    name: "AI",
    scoped: true,
    endpoints: [
      { method: "GET", path: "/api/ai/graph", note: "The mind map: architectures, files, pending proposals." },
      { method: "GET", path: "/api/ai/jobs", note: "Background assistant jobs and their output." },
      { method: "POST", path: "/api/ai/ask", note: "Ask a question. Serialized per project." },
      { method: "POST", path: "/api/ai/ship", note: "Queue a task as a background job." },
      { method: "POST", path: "/api/ai/confirm", note: "Accept or reject one tag proposal." },
      { method: "POST", path: "/api/ai/confirm-all", note: "Accept or reject every pending proposal." },
      { method: "POST", path: "/api/ai/prune", note: "Forget a file or an architecture." },
      { method: "POST", path: "/api/ai/feedback", note: "Tell the assistant it got something wrong." },
    ],
  },
];

const METHOD_COLOR = {
  GET: "default",
  POST: "primary",
  DELETE: "error",
} as const;

function EndpointTable() {
  return (
    <Box sx={{ overflowX: "auto" }}>
      <Table size="small" sx={{ minWidth: 620 }}>
        <TableHead>
          <TableRow>
            <TableCell sx={{ width: 90 }}>Method</TableCell>
            <TableCell>Path</TableCell>
            <TableCell>What it does</TableCell>
          </TableRow>
        </TableHead>
        <TableBody>
          {ENDPOINTS.map((group) => (
            <Fragment key={group.name}>
              <TableRow>
                <TableCell colSpan={3} sx={{ borderBottom: 0, pt: 2.5 }}>
                  <Stack direction="row" spacing={1} alignItems="center">
                    <Typography variant="h3">{group.name}</Typography>
                    {group.scoped && (
                      <Chip size="small" variant="outlined" label="project-scoped" />
                    )}
                  </Stack>
                </TableCell>
              </TableRow>
              {group.endpoints.map((endpoint) => (
                <TableRow key={`${endpoint.method} ${endpoint.path}`} hover>
                  <TableCell>
                    <Chip
                      size="small"
                      variant="outlined"
                      color={METHOD_COLOR[endpoint.method]}
                      label={endpoint.method}
                    />
                  </TableCell>
                  <TableCell sx={{ fontFamily: monoFontStack, fontSize: 13 }}>
                    {endpoint.path}
                  </TableCell>
                  <TableCell>
                    <Typography variant="body2" color="text.secondary">
                      {endpoint.note}
                    </Typography>
                  </TableCell>
                </TableRow>
              ))}
            </Fragment>
          ))}
        </TableBody>
      </Table>
    </Box>
  );
}

// ─── The command reference ──────────────────────────────────────────────────

interface Command {
  /** How you'd type it, with its most useful arguments. */
  usage: string;
  /** What it does, in one line. */
  note: string;
  /** The flags worth knowing about, as (flag, meaning). */
  flags?: [string, string][];
}

interface CommandGroup {
  name: string;
  blurb: string;
  commands: Command[];
}

/**
 * What ciabatta can run, from the app.
 *
 * You are usually reading this page *because* you are in the browser and the
 * thing you want to do next happens in a terminal — so the reference has to be
 * here, not only in `--help`. Kept to what a command is for and the flags that
 * change what it does; `ciabatta <command> --help` is still the exhaustive list.
 */
const COMMANDS: CommandGroup[] = [
  {
    name: "Running things",
    blurb:
      "One command runs everything, because there is only one thing to run. A workflow is a named DAG of steps; every package that declares that name joins in, and the whole thing compiles to a single graph.",
    commands: [
      {
        usage: "ciabatta <WORKFLOW> [ALSO…]",
        note: "Compile every package's workflow of that name into one graph and run it in dependency order. Naming several folds them into the same graph, so shared dependencies run once.",
        flags: [
          ["--filter TERM", "Run only the steps this selects. Repeatable. See below."],
          ["--graph", "Explore the resolved graph interactively and run nothing."],
          ["--dry-run", "Walk every step without executing it."],
          ["--only MEMBER", "Start from these sub-workspaces; dependencies still come along."],
          ["--isolated", "Don't follow dependencies into other sub-workspaces."],
          ["--gui", "Watch it live in this app instead of the terminal."],
          ["-e KEY=VALUE", "Set a variable for every step. Beats .env and CI."],
          ["--tui", "Watch it in the terminal TUI. Runs print plain text by default."],
        ],
      },
      {
        usage: "ciabatta build",
        note: "Any name ciabatta doesn't recognise as a command is a workflow, so this is the same as ciabatta run build. Use the longer ciabatta workflow <name> when a workflow's name collides with a real command.",
      },
      {
        usage: "ciabatta run build test",
        note: "Several targets compile into a single graph rather than running one after the other — a dependency both of them need runs once.",
      },
      {
        usage: "ciabatta run --build",
        note: "Open the visual flowchart builder. Designs a flowchart file; runs nothing.",
      },
    ],
  },
  {
    name: "Seeing what exists",
    blurb:
      "The questions a monorepo usually can't answer: what is there to run, who owns it, and what will actually happen if I run it.",
    commands: [
      {
        usage: "ciabatta list",
        note: "Every sub-workspace, its workflows, their owners and what they need.",
        flags: [
          ["-s TERM", "Search names, descriptions, owners, tags, and the commands steps run."],
          ["-v", "Also list every step inside each workflow."],
        ],
      },
      {
        usage: "ciabatta run <target> --graph",
        note: "The resolved graph: every step in wave order, and per step what it does, who owns it, what it waits for, what waits on it. Honours --filter. Add --tui to explore it interactively instead of printing it.",
      },
      {
        usage: "ciabatta config reference",
        note: "The full config file schema.",
      },
    ],
  },
  {
    name: "Starting a project",
    blurb: "Opting a repo, or one package in it, into ciabatta.",
    commands: [
      {
        usage: "ciabatta init --example",
        note: "Generate a complete worked monorepo to learn from: four sub-workspaces that genuinely depend on each other, workflows spanning them, scripts, tags, timeouts, a recovery node, and a README explaining every part. Every step runs, so it works on a bare machine.",
        flags: [
          ["--into DIR", "Where to write it. Defaults to ./ciabatta-example."],
          ["--nexus", "Add a registry and a release workflow that publishes as a graph step."],
          ["--docker", "Add a Dockerfile and a deploy workflow."],
          ["--all", "Include everything optional."],
        ],
      },
      {
        usage: "ciabatta init --lib",
        note: "Opt the current directory in as a sub-workspace: a `workspace:` identity plus a starter workflow. Prompts for a description and an owner, on purpose.",
        flags: [
          ["--depends-on MEMBER", 'Declare a dependency: "other" or "other:workflow".'],
          ["--workflow NAME", "Name of the starter workflow. Defaults to build."],
        ],
      },
      {
        usage: "ciabatta init",
        note: "A publishing-only config in the current directory — registries, no workspace identity.",
      },
      { usage: "ciabatta configure", note: "Set up registries interactively." },
      {
        usage: "ciabatta register",
        note: "Tell the daemon this checkout exists, so it appears in the project switcher. Every web-facing command does this for the directory it ran in, and `init` does it for a new one — this is for a checkout nothing has been run in yet.",
        flags: [
          ["--path DIR", "Register that directory instead of the current one."],
          ["--quiet", "Print just the project id, for a script."],
        ],
      },
    ],
  },
  {
    name: "Publishing",
    blurb:
      "Publishing is a step, not a command. A step with kind: push moves an artifact to a registry; it sits on the graph, declares what it needs, and so cannot run before the artifact exists.",
    commands: [
      {
        usage: "ciabatta <WORKFLOW> --filter kind:push",
        note: "Run only the transfer steps of a workflow, skipping the builds that feed them.",
        flags: [
          ["--dry-run", "Show what would move, and where, without moving it."],
          ["--local", "Resolve CIABATTA_* from local git rather than CI."],
        ],
      },
      {
        usage: "ciabatta source",
        note: 'Print the resolved CIABATTA_* variables as shell exports: eval "$(ciabatta source)".',
      },
    ],
  },
  {
    name: "Watching and inspecting",
    blurb: "Long-running commands, and what the codebase is made of.",
    commands: [
      {
        usage: "ciabatta watch <command>",
        note: "Run a command and stream its logs into this app. The daemon owns it, so Ctrl-C detaches rather than kills.",
        flags: [
          ["-t PHRASE", "Notify when an output line contains this. Repeatable."],
          ["--list", "List the sessions the daemon is running."],
          ["--attach ID", "Follow an existing session — how you tail a persistent step."],
          ["--stop ID", "Actually stop one."],
        ],
      },
      {
        usage: "ciabatta analyze",
        note: "Scan the codebase's dependency graph and serve it here.",
        flags: [["--check-vulns", "Also query the OSV database for known vulnerabilities."]],
      },
      { usage: "ciabatta tui", note: "The terminal registry browser." },
      { usage: "ciabatta todo [TASK]", note: "Your task list. With text, adds it and exits." },
    ],
  },
  {
    name: "The assistant and the daemon",
    blurb: "",
    commands: [
      {
        usage: "ciabatta ai",
        note: "Chat with an assistant that learns this codebase, with the live architecture map here.",
        flags: [
          ["ask <question>", "One-shot question, plain output."],
          ["ship <task>", "Hand a task to the agent to complete in the background."],
          ["burn-in", "Traverse the codebase and build the whole mind map in one pass."],
          ["report [DAYS]", "Summarize what changed recently. --pdf to save it."],
        ],
      },
      {
        usage: "ciabatta daemon <status|stop|restart|logs>",
        note: "Inspect or restart the background daemon serving this app. You rarely need it — any command with a web view starts it.",
      },
    ],
  },
];

function CommandReference() {
  return (
    <Box sx={{ my: 2 }}>
      {COMMANDS.map((group) => (
        <Box key={group.name} sx={{ mb: 3 }}>
          <SubHeading>{group.name}</SubHeading>
          {group.blurb && <P>{group.blurb}</P>}
          <Stack spacing={1.5} sx={{ maxWidth: "78ch" }}>
            {group.commands.map((command) => (
              <Box
                key={command.usage}
                sx={{
                  border: 1,
                  borderColor: "divider",
                  borderRadius: 1,
                  p: 1.5,
                  bgcolor: "background.paper",
                }}
              >
                <Typography
                  sx={{ fontFamily: monoFontStack, fontSize: 13.5, fontWeight: 600, mb: 0.5 }}
                >
                  {command.usage}
                </Typography>
                <Typography variant="body2" sx={{ lineHeight: 1.7 }}>
                  {command.note}
                </Typography>
                {command.flags && (
                  <Box sx={{ mt: 1, display: "grid", gridTemplateColumns: "auto 1fr", gap: 0.75 }}>
                    {command.flags.map(([flag, meaning]) => (
                      <Fragment key={flag}>
                        <Typography
                          sx={{
                            fontFamily: monoFontStack,
                            fontSize: 12.5,
                            color: "text.secondary",
                            whiteSpace: "nowrap",
                          }}
                        >
                          {flag}
                        </Typography>
                        <Typography variant="caption" color="text.secondary" sx={{ lineHeight: 1.6 }}>
                          {meaning}
                        </Typography>
                      </Fragment>
                    ))}
                  </Box>
                )}
              </Box>
            ))}
          </Stack>
        </Box>
      ))}
    </Box>
  );
}

// ─── Editor setup ───────────────────────────────────────────────────────────

/**
 * The config schemas, as this daemon serves them.
 *
 * All three have to live in one directory: `ciabatta.schema.json` and
 * `workflow.schema.json` both `$ref` into `common.schema.json` by relative
 * path, so a copy of one without the others resolves to nothing.
 */
const SCHEMA_FILES = [
  {
    file: "ciabatta.schema.json",
    covers: ".ciabatta/ciabatta.yaml",
    what: "The package's identity, its registries, its toolchain and its cache.",
  },
  {
    file: "workflow.schema.json",
    covers: ".ciabatta/workflows/*.yaml",
    what: "A workflow and its steps — the file you write most often.",
  },
  {
    file: "common.schema.json",
    covers: "—",
    what: "Step and cache definitions the other two share. Needed by both; not referenced directly.",
  },
];

/** A labelled block of JSON or YAML with a button that copies it. */
function CopyBlock({ children, label }: { children: string; label: string }) {
  const [copied, setCopied] = useState(false);

  return (
    <Box sx={{ position: "relative", "&:hover .copy": { opacity: 1 } }}>
      <Pre>{children}</Pre>
      <Button
        className="copy"
        size="small"
        startIcon={copied ? <CheckIcon /> : <ContentCopyIcon />}
        onClick={() => {
          void navigator.clipboard.writeText(children).then(() => {
            setCopied(true);
            window.setTimeout(() => setCopied(false), 2000);
          });
        }}
        sx={{
          position: "absolute",
          top: 20,
          right: 8,
          opacity: 0,
          transition: "opacity 120ms",
          bgcolor: "background.paper",
        }}
      >
        {copied ? "Copied" : label}
      </Button>
    </Box>
  );
}

/** `104495` -> `102 KB`. Sizes here are always well under a megabyte. */
function kilobytes(bytes: number): string {
  return `${Math.round(bytes / 1024)} KB`;
}

const RELEASES_URL = "https://github.com/forsyth-creations/ciabatta/releases/latest";

/**
 * The VS Code extension, offered by the binary that is serving this page.
 *
 * Worth a component rather than a static link because the answer varies by
 * build. A release binary carries the `.vsix`; one built with a plain
 * `cargo build` does not, and pretending otherwise would hand someone a 404
 * where a button was promised. So ask, and say which situation you are in.
 */
function ExtensionDownloads() {
  const { data, isPending, isError } = useEditorExtensions();
  const vsix = data?.find((e) => e.file.endsWith(".vsix"));

  if (isPending) return null;

  if (isError || !vsix) {
    return (
      <Alert severity="info" sx={{ my: 2, maxWidth: "90ch" }}>
        This binary was built without <C>yarn package</C>, so it carries no extension to hand
        you. Grab the <C>.vsix</C> from the{" "}
        <Box component="a" href={RELEASES_URL} target="_blank" rel="noreferrer">
          releases page
        </Box>
        , or build one from a checkout with <C>yarn package</C>.
      </Alert>
    );
  }

  return (
    <Box sx={{ my: 2 }}>
      <Button
        variant="contained"
        component="a"
        href={`/extensions/${vsix.file}`}
        download={vsix.file}
        startIcon={<DownloadIcon />}
      >
        {vsix.file}
      </Button>
      <Typography variant="caption" color="text.secondary" sx={{ ml: 1.5 }}>
        {kilobytes(vsix.bytes)} — built from the same commit as this binary
      </Typography>
    </Box>
  );
}

/**
 * The half of editor setup that has to know where it is running.
 *
 * The schema URLs go into somebody's editor settings, and they have to name
 * the port *this* daemon is on — which is knowable here and nowhere in a
 * static document. Hence a component rather than more prose: `origin` is read
 * at render time, so the block you copy is the block that works.
 */
function EditorSetup() {
  const origin = typeof window === "undefined" ? "http://127.0.0.1:8099" : window.location.origin;

  const served = `{
  "lsp": {
    "yaml-language-server": {
      "settings": {
        "yaml": {
          "schemas": {
            "${origin}/schemas/ciabatta.schema.json": [
              "**/.ciabatta/ciabatta.yaml"
            ],
            "${origin}/schemas/workflow.schema.json": [
              "**/.ciabatta/workflows/*.yaml"
            ]
          }
        }
      }
    }
  }
}`;

  const committed = `{
  "lsp": {
    "yaml-language-server": {
      "settings": {
        "yaml": {
          "schemas": {
            ".ciabatta/schemas/ciabatta.schema.json": [
              "**/.ciabatta/ciabatta.yaml"
            ],
            ".ciabatta/schemas/workflow.schema.json": [
              "**/.ciabatta/workflows/*.yaml"
            ]
          }
        }
      }
    }
  }
}`;

  return (
    <>
      <SubHeading>Two halves</SubHeading>
      <P>
        Editor support is two independent pieces, and it is worth knowing which one you are
        missing when something doesn&apos;t work.
      </P>
      <Bullets
        items={[
          <>
            <strong>The JSON Schemas</strong> describe the <em>shape</em> of the files: every
            field, what it takes, what it is for. That is where field-name completion, hover
            documentation and &ldquo;unknown field&rdquo; errors come from. Plain JSON Schema, no
            binary needed, works in any editor with YAML support.
          </>,
          <>
            <strong>
              <C>ciabatta lsp</C>
            </strong>{" "}
            is a language server, and a subcommand of the CLI you already have. It knows what a
            schema cannot: which sub-workspaces <em>this</em> monorepo contains, which workflows
            they define, which tools the root&apos;s <C>toolchain:</C> can install. That is what
            completes a <C>needs:</C> and warns when one points at nothing.
          </>,
        ]}
      />
      <P>
        A <C>needs:</C> on a step names steps in the same file; a <C>needs:</C> on the workflow
        names other packages&apos; workflows. Same word, different vocabulary — the server keeps
        them straight, and flags <C>protos</C> when you meant <C>proto</C>.
      </P>

      <SubHeading>VS Code</SubHeading>
      <P>
        The extension isn&apos;t on the Marketplace, so this daemon serves it. Download it and
        either drag the file onto the Extensions panel, or run{" "}
        <strong>Extensions: Install from VSIX…</strong> from the command palette.
      </P>
      <ExtensionDownloads />
      <P>
        It depends on Red Hat&apos;s YAML extension, which VS Code installs alongside it, and it
        carries its own copy of the schemas — so that half needs no CLI at all. For the
        repository-aware half, put the binary on your PATH:</P>
      <Pre>{`cargo install ciabatta`}</Pre>
      <P>
        That is all. Without the binary you still get every field and its documentation, and
        nothing complains. To run a build of your own instead, point{" "}
        <C>ciabatta.server.path</C> at it and use <strong>Ciabatta: Restart Language Server</strong>
        .
      </P>
      <P>
        Building it from a checkout: <C>yarn install</C>, then{" "}
        <C>yarn workspace ciabatta-vscode build</C>, then F5 with <C>editors/vscode</C> open.
      </P>

      <SubHeading>Zed</SubHeading>
      <P>
        Zed extensions launch language servers rather than shipping them, so install the CLI
        first — it is the same binary that runs your builds, which is what keeps completions
        agreeing with <C>ciabatta build</C>:
      </P>
      <Pre>{`cargo install ciabatta`}</Pre>
      <P>
        Then install the extension: <strong>zed: install dev extension</strong> and pick{" "}
        <C>editors/zed</C> from a ciabatta checkout.
      </P>
      <P>
        The schemas need one settings block, because Zed has no equivalent of the contribution
        point VS Code uses. Put either of these in your project&apos;s <C>.zed/settings.json</C>.
      </P>
      <P>
        <strong>Served by this daemon</strong> — nothing to download, but the schemas resolve only
        while it is running:
      </P>
      <CopyBlock label="Copy">{served}</CopyBlock>
      <P>
        <strong>Committed to the repo</strong> — works offline, on a colleague&apos;s machine, and
        in CI. Download the files below into <C>.ciabatta/schemas/</C> and commit them:
      </P>
      <CopyBlock label="Copy">{committed}</CopyBlock>

      <SubHeading>Downloads</SubHeading>
      <P>
        The schemas this binary carries. Save <strong>all three</strong> into one directory: the
        first two reference the third by relative path, and separated they resolve to nothing.
      </P>
      <Table size="small" sx={{ mb: 2, maxWidth: "90ch" }}>
        <TableHead>
          <TableRow>
            <TableCell>File</TableCell>
            <TableCell>Validates</TableCell>
            <TableCell>What it covers</TableCell>
            <TableCell align="right">&nbsp;</TableCell>
          </TableRow>
        </TableHead>
        <TableBody>
          {SCHEMA_FILES.map(({ file, covers, what }) => (
            <TableRow key={file}>
              <TableCell sx={{ fontFamily: monoFontStack, fontSize: 13, whiteSpace: "nowrap" }}>
                {file}
              </TableCell>
              <TableCell sx={{ fontFamily: monoFontStack, fontSize: 13, whiteSpace: "nowrap" }}>
                {covers}
              </TableCell>
              <TableCell>
                <Typography variant="caption" color="text.secondary" sx={{ lineHeight: 1.6 }}>
                  {what}
                </Typography>
              </TableCell>
              <TableCell align="right">
                <Button
                  size="small"
                  component="a"
                  href={`/schemas/${file}`}
                  download={file}
                  startIcon={<DownloadIcon />}
                >
                  Download
                </Button>
              </TableCell>
            </TableRow>
          ))}
        </TableBody>
      </Table>
      <P>
        They are served unauthenticated on purpose — the thing fetching them is a language server
        reading a URL out of a settings file, and a schema is a public description of a file
        format. Everything under <C>/api</C> still needs the token.
      </P>

      <SubHeading>Any other editor</SubHeading>
      <P>
        Nothing above is specific to those two. Point any JSON Schema-aware YAML tool at the same
        files, and any LSP client at <C>ciabatta lsp</C>, which speaks the protocol on stdio.
        The same schemas will validate a config in CI.
      </P>
    </>
  );
}

// ─── The LDAP settings table ────────────────────────────────────────────────

interface LdapSetting {
  name: string;
  required: "yes" | "one of" | "no";
  fallback: string;
  note: string;
}

/** Every field under `auth.ldap`, with the default the server applies. */
const LDAP_SETTINGS: LdapSetting[] = [
  {
    name: "url",
    required: "yes",
    fallback: "—",
    note: "ldaps://host:636. Plain ldap:// works and is for test directories only — a bind sends the password.",
  },
  {
    name: "bind_dn",
    required: "one of",
    fallback: "—",
    note: "A DN template containing {username}. Use when everyone lives under one branch. No search, so no service account.",
  },
  {
    name: "base_dn",
    required: "one of",
    fallback: "—",
    note: "Where to search for the user's DN instead. Use when people are spread across OUs.",
  },
  {
    name: "user_filter",
    required: "no",
    fallback: "(uid={username})",
    note: "The filter that finds them under base_dn. Active Directory usually wants (sAMAccountName={username}).",
  },
  {
    name: "search_dn",
    required: "no",
    fallback: "anonymous",
    note: "A service account to run that search as, when the directory refuses anonymous search.",
  },
  {
    name: "search_password_env",
    required: "no",
    fallback: "—",
    note: "The environment variable holding that account's password. Required if search_dn is set; never the password itself.",
  },
  {
    name: "required_group",
    required: "no",
    fallback: "anyone who binds",
    note: "Refuse anyone who isn't a member. Without it, every account in the directory can read the cache.",
  },
  {
    name: "write_groups",
    required: "no",
    fallback: "everyone may write",
    note: "Members of these may write; everyone else who gets in is read-only. An empty list means no restriction.",
  },
  {
    name: "group_attribute",
    required: "no",
    fallback: "memberOf",
    note: "The attribute on the user's entry listing their groups. Read after the bind, as the user.",
  },
  {
    name: "tls_verify",
    required: "no",
    fallback: "true",
    note: "Verify the directory's certificate. Leave it on outside a test server.",
  },
  {
    name: "timeout",
    required: "no",
    fallback: "10",
    note: "Seconds to wait on the directory before giving up.",
  },
];

function LdapSettingsTable() {
  return (
    <Box sx={{ overflowX: "auto" }}>
      <Table size="small" sx={{ minWidth: 680 }}>
        <TableHead>
          <TableRow>
            <TableCell sx={{ width: 200 }}>Setting</TableCell>
            <TableCell sx={{ width: 150 }}>Default</TableCell>
            <TableCell>What it does</TableCell>
          </TableRow>
        </TableHead>
        <TableBody>
          {LDAP_SETTINGS.map((setting) => (
            <TableRow key={setting.name} hover>
              <TableCell sx={{ fontFamily: monoFontStack, fontSize: 13, verticalAlign: "top" }}>
                <Stack spacing={0.5} alignItems="flex-start">
                  <span>{setting.name}</span>
                  {setting.required !== "no" && (
                    <Chip
                      size="small"
                      variant="outlined"
                      color={setting.required === "yes" ? "warning" : "default"}
                      label={setting.required === "yes" ? "required" : "one of these two"}
                    />
                  )}
                </Stack>
              </TableCell>
              <TableCell
                sx={{ fontFamily: monoFontStack, fontSize: 13, verticalAlign: "top" }}
              >
                {setting.fallback}
              </TableCell>
              <TableCell sx={{ verticalAlign: "top" }}>
                <Typography variant="body2" color="text.secondary">
                  {setting.note}
                </Typography>
              </TableCell>
            </TableRow>
          ))}
        </TableBody>
      </Table>
    </Box>
  );
}

// ─── The sections ───────────────────────────────────────────────────────────

interface DocSection {
  id: string;
  title: string;
  body: ReactNode;
}

const SECTIONS: DocSection[] = [
  {
    id: "overview",
    title: "What this is",
    body: (
      <>
        <P>
          One daemon, one web app. Ciabatta&apos;s tools used to be separate servers on separate
          ports, each with its own layout and its own idea of what a project was; they are now
          pages in this app, backed by a single local HTTP API.
        </P>
        <P>
          The important consequence is <strong>ownership</strong>: the daemon owns the work, not
          the terminal that asked for it. A watch session, a run, and a serial capture all outlive
          the command that started them and the tab that is watching them. Close the browser, come
          back tomorrow, and the session is still there with its logs intact. (A{" "}
          <a href="#run">background task</a> is the one thing deliberately stopped when its run
          ends — its <em>output</em> outlives the run, the process does not.)
        </P>
        <P>
          The daemon starts on demand — any ciabatta command probes for one and launches it if
          nothing answers — and you can drive it directly:
        </P>
        <Pre>{`ciabatta daemon serve            # run it in the foreground
ciabatta daemon serve --port 9000
ciabatta daemon stop             # ask it to exit`}</Pre>
        <P>
          It binds loopback by default. See <a href="#security">Tokens and access</a> before
          changing that — this API can start processes.
        </P>
      </>
    ),
  },
  {
    id: "navigating",
    title: "Finding your way around",
    body: (
      <>
        <Bullets
          items={[
            <>
              <strong>Project switcher</strong> (top bar) — nearly everything except Todo is
              per-checkout, and the switcher decides which one. Projects register themselves the
              first time you run a ciabatta command inside them. Your choice is remembered, and
              falls back to whichever project the daemon lists first.
            </>,
            <>
              <strong>Health chip</strong> — polls <C>/api/health</C> every ten seconds. If it goes
              red the daemon is gone; if the version differs from what you just installed, an old
              daemon is still holding the port.
            </>,
            <>
              <strong>Navigation rail</strong> — collapsible, and the collapsed state is remembered
              on wide screens. Below ~900px it becomes an overlay that closes when you pick a
              destination, because the log and graph views want the width.
            </>,
            <>
              <strong>Colour mode</strong> — the toggle sits next to the health chip and persists.
            </>,
            <>
              <strong>Every view is a URL.</strong> <C>/watch/3</C> and <C>/run/12</C> are real
              links you can bookmark or paste to a colleague on the same machine; the daemon serves
              the app for any non-asset path, so reloading them works.
            </>,
          ]}
        />
      </>
    ),
  },
  {
    id: "commands",
    title: "Commands",
    body: (
      <>
        <P>
          What ciabatta can run. You are usually reading this because the next thing you want to do
          happens in a terminal, so the reference lives here as well as in <C>--help</C> — which
          remains the exhaustive list for any one command.
        </P>
        <Alert severity="success" sx={{ mb: 2, maxWidth: "78ch" }}>
          <strong>Everything is a workflow.</strong> A workflow is a named DAG of steps, declared
          in a package&apos;s <C>.ciabatta/workflows/&lt;name&gt;.yaml</C> — the filename is the
          name. Running it collects every package that declares that name, follows the
          dependencies between them, and runs the result as one graph. Publishing an artifact is a
          step on that graph (<C>kind: push</C>), not a separate command.
        </Alert>
        <CommandReference />

        <SubHeading>Filtering a graph</SubHeading>
        <P>
          <C>--filter</C> narrows a run to the steps you care about, which is how you iterate on
          one package without sitting through the whole monorepo:
        </P>
        <Pre>{`ciabatta run test --filter tag:fast              # only steps tagged fast
ciabatta run test --filter '!tag:flaky'         # everything except the flaky ones
ciabatta run build --filter workspace:api       # one package's steps
ciabatta run release --filter kind:push         # just the publish, artifact in hand
ciabatta run test --filter tag:fast --filter tag:smoke   # either one`}</Pre>
        <P>
          Selectors are <C>tag:</C>, <C>workspace:</C> (alias <C>member:</C>), <C>kind:</C>,{" "}
          <C>owner:</C>, <C>step:</C>, or a bare word that searches all of them plus descriptions.
          A leading <C>!</C> excludes, and exclusions beat matches. Positive terms are OR&apos;d —
          a filter list reads as &quot;the things I want&quot;. Tags cascade from the sub-workspace
          to the workflow to the step, so a step inherits every label above it.
        </P>
        <Alert severity="warning" sx={{ my: 2, maxWidth: "78ch" }}>
          A filter <strong>prunes</strong> the graph rather than expanding a selection: the
          surviving steps run without the dependencies you filtered away, on the assumption those
          already happened. It is the fast debug loop, not how you build a fresh checkout.
          Ciabatta reports every dependency edge it cut, so this is never silent.
        </Alert>

        <SubHeading>Environment variables</SubHeading>
        <P>
          Precedence, weakest first: <C>.env</C> files → CI-derived → the ambient environment →{" "}
          <C>-e KEY=VALUE</C>. A workflow can insist on variables with <C>REQUIRED_ENV</C>, checked
          before anything runs rather than halfway through.
        </P>
        <P>
          <strong>Among the files themselves, nearest wins.</strong> A step in{" "}
          <C>packages/api</C> reads <C>packages/api/.env</C>; whatever that file doesn&apos;t set
          comes from the workspace above it, up to the monorepo root. A sibling package&apos;s{" "}
          <C>.env</C> is never a fallback. Each step&apos;s chain is listed under the step on the{" "}
          <Link to="/run">Run page</Link>, so &quot;which file did this value come from?&quot; has
          a visible answer.
        </P>
        <P>
          A missing <C>.env</C> is generated from the checked-in template at the start of a run —
          the declared <C>env_default</C>, or a conventional <C>.env.example</C> /{" "}
          <C>.env.default</C> that is simply there — for the project and for every sub-workspace
          the run touches. It never overwrites a file that exists.
        </P>
        <P>
          Ciabatta snapshots the variables its <C>.env</C> files define — names and value{" "}
          <em>hashes</em>, never the values — under <C>.ciabatta/cache/</C>. When they change,
          because someone pulled a branch that adds a required variable, the next run says which
          ones moved before it starts. The same drift is served at{" "}
          <C>/api/workspace/env-drift</C>.
        </P>
      </>
    ),
  },
  {
    id: "editors",
    title: "Editors",
    body: <EditorSetup />,
  },
  {
    id: "todo",
    title: "Todo",
    body: (
      <>
        <P>
          A task list stored in <C>~/.ciabatta/todos.json</C>, scoped to the project you are looking
          at. The switcher at the top of the page selects whose list you see, so notes written in
          one repo do not clutter another.
        </P>
        <SubHeading>The global list</SubHeading>
        <P>
          Not everything you write down is about a repo. The globe button on a task makes it{" "}
          <strong>global</strong>: it leaves the project&apos;s list and appears on the{" "}
          <Link to="/">dashboard</Link>, where it stays whichever project you switch to. A global
          task can be filed back under the selected project the same way. From a terminal,{" "}
          <C>ciabatta todo --global &quot;…&quot;</C> adds one directly.
        </P>
        <P>
          The two lists are deliberately disjoint — a global task appears on the dashboard and
          nowhere else. Showing it under every project as well would turn the thing you set aside
          into the thing you see most often. Tasks written before todos were scoped carry no
          project, so they land on the global list, which is where something nobody attached to a
          repo belongs anyway.
        </P>
        <P>
          Removing a project from the switcher promotes its tasks to the global list rather than
          leaving them attached to an id nothing resolves — which would be deletion without saying
          so.
        </P>
        <P>
          Click a task&apos;s text to edit it in place. The editor is multi-line, because a task is
          often a paragraph and a box that scrolls sideways makes anything longer than a sentence
          unreadable while you are writing it — so Enter inserts a newline, and{" "}
          <C>⌘/Ctrl+Enter</C> saves. Clicking away saves too, because after typing that means
          &ldquo;keep it&rdquo; far more often than it means the opposite; Escape abandons. An empty
          edit is treated as a mis-key rather than a delete — the bin does that.
        </P>
        <P>
          Tasks carry a priority (low, medium, high) and a done flag. <strong>Ship</strong> hands
          the task to the assistant as a background job and returns its job number — the agent edits
          files, so it needs to know whose. Follow it on the <Link to="/ai">AI page</Link>.
        </P>
      </>
    ),
  },
  {
    id: "watch",
    title: "Watch",
    body: (
      <>
        <P>
          Run a command and stream its output into a live, searchable view. The daemon spawns the
          process, so the session survives closing this tab or the terminal — including the
          sessions a workflow&apos;s <a href="#run">background tasks</a> leave behind, which show
          up here labelled with the graph node that started them. Those sessions stay readable
          after their run has stopped the process behind them.
        </P>
        <SubHeading>In a session</SubHeading>
        <Bullets
          items={[
            <>
              <strong>Search</strong> covers the whole buffer on the daemon side, not just the lines
              currently rendered. Choose <em>any</em> or <em>all</em> for multi-term queries, or
              switch to regex.
            </>,
            <>
              <strong>Bookmarks</strong> pin a line with a label and a snippet, so &quot;the point
              where it broke&quot; is still findable after another 50,000 lines.
            </>,
            <>
              <strong>Triggers</strong> are patterns (literal or regex) the daemon matches as output
              arrives; each hit is recorded with its line, so you can start a long build and come
              back to the list of matches for <C>error</C>.
            </>,
            <>
              <strong>Stop</strong> ends the process but keeps the output. <strong>Discard</strong>{" "}
              throws the session away entirely.
            </>,
            <>
              <strong>Export</strong> (the share icon) saves the log to a file or copies it to the
              clipboard, ready to send to someone else.
            </>,
          ]}
        />
        <SubHeading>Sending a log to someone</SubHeading>
        <P>
          The export button builds the transcript on the daemon, not from what this tab happens to
          have streamed — so it is the <em>whole</em> buffer, with the command, the exit status,
          and your bookmarks in the header, and <C>stderr</C> lines marked as such. If the ring
          buffer dropped older output, the file says so rather than starting mid-story.
        </P>
        <Bullets
          items={[
            <>
              <strong>Download as a file</strong> — a <C>.log</C> named after the step (or the
              command), for attaching to a ticket.
            </>,
            <>
              <strong>Download with timestamps</strong> — every line prefixed with when it arrived.
              Reach for this when the question is <em>where did it stall</em>; skip it when you are
              sending someone a stack trace.
            </>,
            <>
              <strong>Copy to clipboard</strong> — straight into chat. Needs a secure context, so
              it may be refused over plain HTTP on a non-loopback host; the download always works.
            </>,
          ]}
        />
        <P>
          The same thing from a terminal is <C>ciabatta watch --attach ID {">"} out.log</C>.
        </P>
        <Alert severity="info" sx={{ my: 2, maxWidth: "78ch" }}>
          There is no box here for typing a command to run. The daemon executes with your full
          privileges, so a free-text shell field in a web page is a remote-execution surface for
          anything that can reach the port. Start sessions from the CLI, where the person starting
          one is the person at the keyboard.
        </Alert>
        <P>
          Output arrives over SSE and is flushed on animation frames, and both the daemon and the
          browser cap what they retain — a command emitting thousands of lines a second will drop
          the oldest lines rather than lock the tab up.
        </P>
      </>
    ),
  },
  {
    id: "workspace",
    title: "Workspace",
    body: (
      <>
        <P>
          The answer to &quot;what can I run in this monorepo, and what happens if I do&quot;. It
          reads the <C>.ciabatta</C> declarations off disk — no scan — so the whole catalogue of
          sub-workspaces, their workflows, owners, tags, and steps arrives in one request and
          searching happens locally.
        </P>
        <P>
          Search is deliberately generous: it matches names, descriptions, owners, tags,{" "}
          <em>and the commands steps actually run</em>. &quot;Which package runs protoc?&quot; is
          the question a monorepo otherwise can&apos;t answer.
        </P>
        <SubHeading>Graphing a workflow</SubHeading>
        <P>
          Pick a workflow name and the daemon compiles the graph that would run, following
          cross-package dependencies. Nodes say which sub-workspace they came from, and are laid
          out by dependency wave. Missing toolchain entries are called out separately — a build
          that would fail for want of <C>protoc</C> says so before it starts.
        </P>
        <P>Step badges mean:</P>
        <Bullets
          items={[
            <>
              <Chip size="small" color="primary" label="push" /> — the special publishing phase,
              identifiable so it can be skipped or required as a unit.
            </>,
            <>
              <Chip size="small" variant="outlined" color="info" icon={<BoltIcon />} label="background" />{" "}
              — from the workflow&apos;s <C>background:</C> array. Up before wave 1, gates nothing,
              stopped when the run ends. Drawn in its own row at the bottom rather than in a wave.
            </>,
            <>
              <Chip size="small" variant="outlined" color="info" label="persistent" /> — a step that
              never exits and is <em>left</em> running when the run ends. Tail either under Watch.
            </>,
            <>
              <Chip size="small" variant="outlined" label="timeout" /> — killed past its limit, and
              the rest of the graph carries on.
            </>,
            <>
              <Chip size="small" variant="outlined" label="non-blocking" /> — its failure skips
              dependents but does not stop the run.
            </>,
          ]}
        />
        <P>
          From here you can start the graph directly, with or without <strong>dry run</strong>. It
          becomes an ordinary run on the <Link to="/run">Run page</Link>; the daemon compiles the
          graph the same way <C>ciabatta build</C> does, so the UI and the CLI can&apos;t disagree
          about what executes.
        </P>

        <SubHeading>Stale workflows</SubHeading>
        <P>
          A repository records everything about a workflow except the one thing that says whether it
          still works: when anybody last ran it. So somebody adds <C>deploy-staging</C>, the staging
          environment goes away, and the workflow stays — listed, documented, apparently a thing you
          could run, and broken in a way nobody discovers until they try.
        </P>
        <P>
          Every run writes down what it ran, how it went and how long it took, and each workflow
          here carries a chip saying when it last ran. Anything past{" "}
          <C>workspace.stale_after</C> in the root config (30 days by default) is flagged{" "}
          <strong>stale</strong>. <C>ciabatta list</C> shows the same thing and names them all at
          the end.
        </P>
        <Alert severity="info" sx={{ mb: 2, maxWidth: "78ch" }}>
          <strong>&ldquo;Never run&rdquo; is not the same as stale</strong>, and is deliberately not
          styled like a verdict. The history is per checkout and not committed — it is observation,
          not configuration, and a file every run rewrites would conflict on every merge — so a
          fresh clone starts out knowing nothing. That is an absence of evidence, not evidence the
          workflow is dead.
        </Alert>
        <P>
          Which is why it is shared. When the project has a{" "}
          <a href="#remote-cache">remote cache</a>, each run reports what it ran and takes back what
          everyone else has run, so the answer is &ldquo;when did <em>anyone</em> last run
          this&rdquo; rather than &ldquo;when did I&rdquo;. A workflow you have not touched since
          March may be the one CI runs hourly; one nobody anywhere has run since March is the one
          worth deleting. Both directions happen at the end of a run, so reading this page never
          waits on the network — a cache that is down costs you a stale picture, not a slow page.
        </P>
        <P>
          The server sees every project at once, which no single checkout can. Its own{" "}
          <C>stale_after</C> — a cache serving five teams may want to hear about a quarter of
          silence where one team calls a fortnight stale — decides what it reports:
        </P>
        <Pre>{`$ ciabatta remote-cache status

Projects:
  api        3f2a…   412 hit / 38 miss  ·  2 stale workflow(s)

Workflows: 24 tracked, 3 not run by anyone in over 30d
  api        api:deploy-staging       94 days ago  (7 run(s) ever)`}</Pre>
      </>
    ),
  },
  {
    id: "run",
    title: "Run",
    body: (
      <>
        <P>
          Executes a step DAG live. Pick a workflow (or arrive from Workspace with one already
          compiled), optionally tick <strong>dry run</strong>, and start. The daemon owns the run,
          so it keeps going with the tab closed.
        </P>
        <SubHeading>Missing environment</SubHeading>
        <P>
          If a workflow declares variables the daemon&apos;s environment lacks, the start is rejected
          with the list rather than begun and aborted halfway. The launcher prompts for those values
          and retries — nothing half-executes because a variable was unset.
        </P>
        <SubHeading>Watching a run</SubHeading>
        <Bullets
          items={[
            <>
              Steps are drawn as a graph with their status. Solid edges are <C>needs</C>{" "}
              dependencies; the others are failure branches and retries.
            </>,
            <>
              <strong>Recovery steps</strong> are the fix-it branches a failure diverts into, rather
              than a dead end.
            </>,
            <>
              A step can <strong>ask a question</strong> mid-run; the prompt appears with its
              options and the run waits for your answer.
            </>,
            <>Selecting any step shows its logs, streamed as they are produced.</>,
          ]}
        />
        <SubHeading>Background tasks</SubHeading>
        <P>
          Some things a build needs are not steps in it. A mock API the integration tests talk to, a
          database container, a bundler in watch mode — they have to be <em>running</em>, they never
          finish, and waiting for one is waiting forever. Those go in the workflow&apos;s{" "}
          <C>background:</C> array, alongside <C>steps:</C> rather than inside it.
        </P>
        <Pre>{`background:
  - name: mock-api
    description: Stub API the integration step talks to
    run: node scripts/mock-api.js

steps:
  - name: compile
    run: cargo build
  - name: integration
    run: yarn test:integration
    needs: [compile]`}</Pre>
        <P>
          An array of their own, not a flag on a step, because they are not steps: they never
          finish, so they can hold no position in the order, and nothing can sensibly declare{" "}
          <C>needs</C> on one. Each is <strong>started before the first wave</strong>, so it is up
          by the time anything wants it, and <strong>gates nothing</strong> — no mistake in one can
          hold a build up, and it can never be the reason a run is slow. The graph views say the
          same thing: background tasks are drawn in their own row at the bottom, under a lightning
          bolt, because a wave means &ldquo;the next one waits for these&rdquo; and nothing here
          waits.
        </P>
        <Alert severity="info" sx={{ mb: 2, maxWidth: "78ch" }}>
          <strong>Nothing waits for it, so nothing checks it either.</strong> Started before the
          first wave is not the same as <em>ready</em> before the first wave — a step may still
          reach the mock API before it is listening. If that race matters, have the step wait for
          the port rather than assuming; the graph cannot do it for you, because
          &ldquo;ready&rdquo; is a different question for every server.
        </Alert>

        <SubHeading>Background vs. persistent</SubHeading>
        <P>
          There are two ways to run something that never exits, and the only difference is{" "}
          <strong>what happens when the run ends</strong>.
        </P>
        <Bullets
          items={[
            <>
              A <C>background:</C> entry is <strong>stopped</strong> when every stage has succeeded
              or failed. It existed to get this run through; leaving it up would hand you a process
              still holding its port for the next run to collide with.
            </>,
            <>
              A step with <C>persistent: true</C> is <strong>left running</strong>. The daemon takes
              ownership, so <C>ciabatta dev</C> leaves you a dev server to work against — killing it
              on the way out would make persistence pointless. Stop it yourself with{" "}
              <C>ciabatta watch --stop &lt;id&gt;</C>.
            </>,
          ]}
        />
        <P>
          Both run as <Link to="/watch">watch sessions</Link>, labelled with the node that started
          them, so their output is readable live and afterwards — a stopped background task leaves
          its session behind even though its process is gone. To follow either while a run is going:
        </P>
        <Pre>{`ciabatta watch --attach 3     # follow it
ciabatta watch --stop 3       # stop it early`}</Pre>
        <P>
          If no daemon can be reached, either kind still runs — as a child of the run itself, still
          blocking nothing — but it cannot outlive this process and its output goes to the run&apos;s
          log rather than a session. The log says so at the time rather than leaving it to be
          discovered.
        </P>

        <SubHeading>Stopping a run</SubHeading>
        <P>
          A run in flight has a <strong>Stop</strong> button beside its status. The daemon{" "}
          <em>asks</em> the engine to stop rather than killing it: the step running now is killed,
          nothing further is scheduled, and the background tasks the run started are stopped on the
          way past. Killing the run outright would leave those behind, which is the state stopping
          most needs to avoid.
        </P>
        <Alert severity="warning" sx={{ mb: 2, maxWidth: "78ch" }}>
          <strong>What has already happened stays happened.</strong> Steps are side-effecting shell
          work; stopping halfway can leave a migration applied and the deploy that follows it not.
          Stopping is a way out of a run that is going nowhere, not an undo.
        </Alert>
        <P>
          A stopped run is reported as stopped, not as a failed build — nobody should go looking for
          a bug that isn&apos;t there. Its logs stay readable. Clicking Stop on a run that has
          already finished does nothing.
        </P>

        <SubHeading>Flowchart builder</SubHeading>
        <P>
          <Link to="/run/builder">The builder</Link> is an authoring tool, not an executor. Lay out
          steps, their <C>needs</C>, and their error branches, then copy the generated config into
          your <C>ciabatta.yaml</C>. Nothing you build there runs until it is committed to the file.
        </P>
      </>
    ),
  },
  {
    id: "cache",
    title: "Cache",
    body: (
      <>
        <P>
          Caching is off until a workspace opts in, because a cache that turns itself on is a cache
          that will one day serve somebody a stale artifact they never asked it to keep. Opting in
          is one line — and it is the same line where you say what your inputs are, which is the
          part that actually has to be right.
        </P>
        <Pre>{`ciabatta cache init build      # propose inputs and outputs for the \`build\` workflow
ciabatta dry-run build         # what would be reused, and why not
ciabatta dry-run build --diff  # ...with the lines that changed`}</Pre>
        <P>
          The section lands in <C>.ciabatta/workflows/&lt;name&gt;.yaml</C>, next to the steps it
          describes — what a build reads is a property of that build, and a <C>build</C> and a{" "}
          <C>test</C> in one package read different files. A step can narrow it further with its
          own <C>cache:</C>, layered over the workflow&apos;s field by field. The shared cache
          server stays in <C>ciabatta.yaml</C>: that&apos;s one endpoint per checkout, not a
          property of any one build.
        </P>

        <SubHeading>Three dependencies</SubHeading>
        <P>
          A stage depends on exactly three things, and any of them changing is a rebuild: its{" "}
          <strong>input files</strong>, the <strong>environment variables</strong> it declared in{" "}
          <C>cache.env</C>, and the <strong>outputs of the stages it needs</strong>. The third is
          what makes a graph cacheable rather than just a directory — change a <C>.proto</C> file
          and <C>proto:generate</C> misses, its outputs change, and everything downstream of it
          misses too, each for a reason it can name.
        </P>
        <P>
          The <Link to="/cache">Cache page</Link> shows all three. For every stage it prints the
          decision, the input files it is judged on, the output files it produces, and — when the
          answer is &ldquo;rebuild&rdquo; — a diff in the shape of a pull request: the changed
          files with their lines, the variables that moved, and the upstream stages that produced
          something different. The same view is attached to each node of the{" "}
          <Link to="/workspace">workflow graph</Link>, with the graph&apos;s inputs above its first
          wave and its outputs below the last.
        </P>

        <SubHeading>Two things worth knowing</SubHeading>
        <P>
          <strong>An undeclared input is a wrong answer, not a slow one.</strong> A build that reads
          a file not listed in <C>inputs</C> will be handed a stale result when that file changes.
          That is why <C>cache init</C> scaffolds the inputs from what is actually in the directory
          rather than leaving them empty, and why the dry run exists at all.
        </P>
        <P>
          <strong>Outputs are verified, not assumed.</strong> A key match says the inputs did not
          change; it says nothing about whether somebody deleted <C>dist/</C> or hand-edited a
          generated file. So the outputs are hashed too, and a mismatch is a restore or a rebuild —
          the difference between &ldquo;we think this is current&rdquo; and &ldquo;this is
          current&rdquo;.
        </P>

        <SubHeading>Proving the inputs are right</SubHeading>
        <P>
          A dry run shows what the cache <em>thinks</em>. <C>--authoritative</C> checks whether it
          is entitled to think it.
        </P>
        <Pre>{`ciabatta build --authoritative`}</Pre>
        <P>
          Every step runs in its own directory holding the files it declared under <C>inputs</C>{" "}
          and nothing else, laid out the way the project root is — so a path that reaches sideways
          (<C>../schemas/*.json</C>) or writes upward (<C>../dist/thing.vsix</C>) still resolves. A
          step that reads something it never declared cannot find it and fails, now, rather than
          being handed a stale artifact weeks later when that file has changed and nothing noticed.
          Declared outputs are copied back, so the run leaves the same artifacts an ordinary one
          would.
        </P>
        <P>
          The cache is switched off for these runs. A cache hit skips a step, and a step that does
          not run is held to nothing — and the cache is the thing under suspicion to begin with.
        </P>
        <Alert severity="info" sx={{ my: 2, maxWidth: "90ch" }}>
          Opt-in, and it stays that way. There is no hermetic toolchain and no attempt to isolate{" "}
          <C>$HOME</C>, the network or the clock — the compiler and the package manager are
          whatever the machine has. It answers one question: <em>are my inputs complete?</em>
        </Alert>
        <P>
          Some steps need state that is not a source file. <C>yarn run check</C> has to sit inside
          its yarn project; a cargo build wants the shared <C>target/</C>. Listing{" "}
          <C>node_modules</C> under <C>inputs</C> would put a hundred thousand derived files into
          the cache key and call them sources, so name them separately — symlinked in, and
          explicitly outside what the run vouches for:
        </P>
        <Pre>{`ciabatta build --authoritative \\
  --sandbox-also node_modules --sandbox-also .yarn \\
  --sandbox-also package.json --sandbox-also yarn.lock`}</Pre>
        <P>
          A failed step keeps its sandbox, under <C>.ciabatta/.cache/authoritative/</C>, because
          what the step could see when it failed is the whole question. A step that declares no{" "}
          <C>inputs</C> is not isolated — an empty directory would fail it for reasons unrelated to
          its declarations — and is listed at the end as unverified.
        </P>
      </>
    ),
  },
  {
    id: "cache-config",
    title: "Configuring the cache",
    body: (
      <>
        <P>
          Cache settings live <strong>with the workflow they describe</strong>, in{" "}
          <C>.ciabatta/workflows/&lt;name&gt;.yaml</C>, next to the steps. What a build reads is a
          property of that build: a <C>build</C> and a <C>test</C> in the same package read
          different files and produce different things, and they need to be able to say so
          separately.
        </P>
        <Pre>{`# packages/api/.ciabatta/workflows/build.yaml
description: Build the api binary

cache:
  enabled: true
  inputs:  ["src/**/*", "Cargo.toml"]   # what the build READS
  outputs: ["target/release/api"]       # what the build WRITES
  exclude: [target]                     # never counted as an input
  env:     [PROFILE]                    # variables the RESULT depends on

steps:
  - name: compile
    run: cargo build --release`}</Pre>

        <SubHeading>The five fields</SubHeading>
        <Box sx={{ overflowX: "auto", mb: 2 }}>
          <Table size="small" sx={{ minWidth: 620 }}>
            <TableHead>
              <TableRow>
                <TableCell sx={{ width: 110 }}>Field</TableCell>
                <TableCell>What it means</TableCell>
              </TableRow>
            </TableHead>
            <TableBody>
              <TableRow hover>
                <TableCell>
                  <C>enabled</C>
                </TableCell>
                <TableCell>
                  Whether to cache at all. Off unless something says otherwise — see the layering
                  rule below, which is why this is three-state rather than a plain boolean.
                </TableCell>
              </TableRow>
              <TableRow hover>
                <TableCell>
                  <C>inputs</C>
                </TableCell>
                <TableCell>
                  Globs for the files the build reads, relative to the package directory. Changing
                  any of them changes the key, and so rebuilds. This is the field that has to be
                  right.
                </TableCell>
              </TableRow>
              <TableRow hover>
                <TableCell>
                  <C>outputs</C>
                </TableCell>
                <TableCell>
                  Globs for what the build writes. These are what gets stored, restored on a hit,
                  and verified before a hit is granted. Declare none and nothing can be restored,
                  so every build runs.
                </TableCell>
              </TableRow>
              <TableRow hover>
                <TableCell>
                  <C>env</C>
                </TableCell>
                <TableCell>
                  Variables the <em>result</em> depends on. A build that produces something
                  different under a different <C>PROFILE</C> must list it, or switching profiles
                  will silently reuse the other one&apos;s artifacts.
                </TableCell>
              </TableRow>
              <TableRow hover>
                <TableCell>
                  <C>exclude</C>
                </TableCell>
                <TableCell>
                  Patterns never treated as inputs even when <C>inputs</C> would match them. The
                  usual case is build output living under a source tree — without it, a build
                  invalidates itself with its own results and never hits twice. A bare directory
                  name is enough: <C>exclude: [target]</C> covers everything under it, no{" "}
                  <C>/**/*</C> required.
                </TableCell>
              </TableRow>
            </TableBody>
          </Table>
        </Box>

        <SubHeading>Narrowing it for one step</SubHeading>
        <P>
          A step can declare its own <C>cache:</C>, layered over the workflow&apos;s{" "}
          <strong>field by field</strong> — so it states only what differs and inherits the rest:
        </P>
        <Pre>{`cache:
  enabled: true
  inputs:  ["src/**/*"]
  outputs: ["dist/**/*"]

steps:
  - name: compile
    run: make

  - name: docs
    run: make docs
    cache:
      inputs: ["docs/**/*"]   # its own sources…
      # …and it still inherits outputs, env and exclude from above`}</Pre>
        <P>
          A list that a step <em>does</em> declare replaces the inherited one whole. Half-merged
          input globs would be very hard to reason about, and reasoning about exactly which files a
          build is judged on is the entire point.
        </P>
        <P>
          Two asymmetries worth knowing, because both are easy to assume the other way around:{" "}
          <C>exclude</C> filters <strong>inputs only</strong> — applying it to outputs would erase
          the very files a hit is supposed to restore. And in a monorepo, a nested sub-workspace is
          dropped from its parent&apos;s inputs automatically, so a package does not rebuild every
          time one of its children changes.
        </P>
        <Alert severity="info" sx={{ mb: 2, maxWidth: "78ch" }}>
          <strong>
            Declaring a dependency never turns caching on or off — only an explicit{" "}
            <C>enabled:</C> does.
          </strong>{" "}
          A step that writes <C>cache: {"{ env: [PROFILE] }"}</C> means &ldquo;I also depend on
          PROFILE&rdquo;, not &ldquo;stop caching me&rdquo;. The most specific explicit{" "}
          <C>enabled:</C> wins, so one step still opts out with <C>enabled: false</C>.
        </Alert>
        <P>
          There is a third, outermost level: a <C>cache:</C> in the package&apos;s{" "}
          <C>ciabatta.yaml</C> applies underneath every workflow in it. It is the right home for
          something genuinely repo-wide — a shared <C>exclude</C>, say — and the wrong home for
          inputs and outputs, which differ per build.
        </P>

        <SubHeading>Letting ciabatta write it</SubHeading>
        <P>
          The proposal comes from the directory&apos;s real contents rather than a template,
          because an empty <C>inputs</C> is the one failure mode that produces wrong answers rather
          than slow ones:
        </P>
        <Pre>{`ciabatta cache init build          # scaffold it for the \`build\` workflow
ciabatta cache init build --enable   # …and turn it on straight away`}</Pre>
        <P>
          With one workflow in the package the name is optional. With several you name the one you
          mean — guessing would write one build&apos;s file list into another&apos;s.{" "}
          <C>cache init</C> refuses to enable a proposal whose inputs or outputs are still{" "}
          <C>TODO</C>, because caching that can never hit reads as the feature being broken.
        </P>

        <SubHeading>What does not go here</SubHeading>
        <P>
          The shared cache <em>server</em> stays in <C>ciabatta.yaml</C>. It is one endpoint per
          checkout, not a property of any one build, and repeating it in four workflow files would
          be four places to change when the server moves.
        </P>
        <Pre>{`# .ciabatta/ciabatta.yaml
cache:
  remote:
    url: http://cache.example.com:8380
    project: 7f3a-…        # assigned on first contact — commit this`}</Pre>

        <SubHeading>Checking you got it right</SubHeading>
        <P>
          Do not take the config&apos;s word for it. The <Link to="/cache">Cache page</Link> shows,
          per step, the decision it reached and the exact input files it was judged on — so an{" "}
          <C>inputs</C> glob that quietly matches nothing is visible rather than inferred. The same
          view hangs off each node of the <Link to="/workspace">workflow graph</Link>. From a
          terminal, <C>ciabatta dry-run &lt;workflow&gt;</C> prints the same answer and{" "}
          <C>--diff</C> adds the lines that changed.
        </P>
      </>
    ),
  },
  {
    id: "features",
    title: "Build features",
    body: (
      <>
        <P>
          A feature is a build-shaping switch: telemetry compiled in or not, the new UI or the old
          one, the fast test suite or the slow one. Any environment variable named{" "}
          <C>CIABATTA_FEAT_&lt;NAME&gt;</C> is one.
        </P>
        <Pre>{`CIABATTA_FEAT_NEW_UI=1 ciabatta build`}</Pre>
        <P>
          Nothing is declared anywhere. The name after the prefix is the feature — <C>new_ui</C>{" "}
          above — matched case-insensitively, with <C>-</C> and <C>_</C> treated alike. An empty
          value, or <C>0</C>, <C>false</C>, <C>no</C> or <C>off</C>, turns the feature off;
          anything else turns it on. The same variable set in any <C>.env</C> the run sources
          counts identically, because features are read after the whole <C>env_file</C> chain has
          been layered in.
        </P>
        <P>A run says what it saw before it starts a step:</P>
        <Pre>{`[build] features: new_ui (off: telemetry)`}</Pre>

        <SubHeading>Gating a step</SubHeading>
        <P>
          Steps condition on features the way they condition on anything else, with the feature
          spelled as a feature rather than as a variable:
        </P>
        <Pre>{`steps:
  - name: bundle-new-ui
    run: yarn build:next
    when: "feature.new_ui"

  - name: bundle-legacy
    run: yarn build
    skip_if: "feature.new_ui"`}</Pre>
        <P>
          <C>!feature.x</C> negates, and the bare <C>CIABATTA_FEAT_NEW_UI</C> still works if you
          would rather write the variable out. Every step also gets <C>CIABATTA_FEATURES</C> — the
          enabled features, sorted and comma-separated — for scripts that want to pass the whole
          set on to something else rather than test one name.
        </P>

        <SubHeading>They are part of the cache key</SubHeading>
        <P>
          An artifact built with a feature on is not reusable by a build with it off. Before this,
          saying so meant listing the variable under <C>cache.env</C>, where one forgotten line
          silently served the other configuration&apos;s artifacts. Anything named with the prefix
          is in the key by construction, so that mistake is no longer available.
        </P>
        <P>
          A feature explicitly turned <em>off</em> is deliberately not in the key:{" "}
          <C>CIABATTA_FEAT_X=0</C> produces the same artifacts as never mentioning <C>X</C>, and
          giving them different keys would cost a rebuild to prove they were the same. It is still
          reported, so a misspelled name that did nothing does not look like one that worked.
        </P>
      </>
    ),
  },
  {
    id: "remote-cache",
    title: "Remote cache",
    body: (
      <>
        <P>
          A small server anyone can stand up, so a team&apos;s builds stop repeating each
          other&apos;s work. It keeps artifacts on its own filesystem in the same layout the local
          cache uses — no object store to provision, no database to migrate.
        </P>
        <Pre>{`# On the server
ciabatta remote-cache init
ciabatta remote-cache start

# On each developer's machine
ciabatta remote-cache login http://cache.example.com:8380
ciabatta cache init --remote http://cache.example.com:8380`}</Pre>
        <P>
          A project is known to the server by its name <em>and an id the server assigns</em>, and
          that id is written back into the workspace config to be committed. It is what makes every
          checkout and every CI runner resolve to the same project: names get reused and renamed,
          and two teams both calling their repo <C>api</C> must never end up silently sharing a
          cache.
        </P>
        <P>
          Authentication is <C>open</C>, <C>token</C>, or LDAPS against the directory you already
          run, with group membership deciding who gets in and who may write. Read access is a
          convenience; <strong>write access is trust</strong> — whoever can write to a cache decides
          what everyone else&apos;s build produces — which is why read-only access exists for both
          a token user and an LDAP group. <a href="#remote-cache-ldap">LDAP has its own section</a>:
          the settings, both ways of finding a user, and what each error means.
        </P>
        <P>
          The <Link to="/cache">Remote tab</Link> shows the hit rate, what is stored, the retention
          policy, and which ciabatta builds the server hands out. A rate near zero usually means the
          keys are not stable — an undeclared input, or something like a timestamp baked into a
          build — rather than that nothing is reusable.
        </P>

        <SubHeading>The server&apos;s own page</SubHeading>
        <P>
          The cache server serves a small admin page at its root — open{" "}
          <C>http://your-cache:8380/</C> in a browser. It shows the hit rate, what is stored, and
          the ciabatta builds it hands out, and it does the one thing the CLI does badly:{" "}
          <strong>minting credentials</strong>. <C>remote-cache add-user</C> prints a hash for you
          to paste into the config and restart around; the page writes the user to the
          server&apos;s own list and hands back the token there and then. That token is shown
          exactly once — only its SHA-256 is kept — so a lost one is reissued, never recovered.
        </P>
        <P>
          On a <C>token</C> server only an <strong>admin</strong> may do that. On an{" "}
          <C>open</C> server anyone who can reach it may, because open mode already means &ldquo;I
          trust whoever is on this network&rdquo; and refusing would leave no way to mint the first
          credential when locking the cache down — but a user created on an open server is{" "}
          <strong>never</strong> an admin, or somebody could grant themselves lasting control while
          the door was open and keep it after it was shut. On an <C>ldap</C> server nobody is an
          admin and there is nothing to mint — the{" "}
          <a href="#remote-cache-ldap">directory is the user list</a>.
        </P>
        <P>
          So the migration from open to authenticated is: create the users you want on the page,
          add one <C>admin: true</C> user to <C>auth.users</C> in the config, set{" "}
          <C>auth.mode: token</C>, and restart. Config-declared users stay yours — the page will
          neither shadow nor delete them.
        </P>

        <SubHeading>TLS</SubHeading>
        <P>
          The server speaks HTTP; put it behind a reverse proxy with TLS for anything beyond a
          trusted network. If that proxy uses a self-signed certificate, or an internal CA a machine
          does not have installed, that machine can opt out with <C>cache.remote.tls_verify: false</C>{" "}
          — or <C>remote-cache login --no-tls-verify</C>, which remembers the choice for later
          commands.
        </P>
        <Alert severity="warning" sx={{ mb: 2, maxWidth: "78ch" }}>
          With verification off, HTTPS is an encrypted channel to <em>whoever answered</em> — so the
          build artifacts it hands back are only as trustworthy as the network between you.
          Installing the CA certificate is the better fix wherever it is available.
        </Alert>

        <SubHeading>Running one locally</SubHeading>
        <P>
          Everything above works on one machine, which is the sanest way to try the remote cache
          before pointing a team at it. Two things to know first. The cache server and the ciabatta
          daemon are <strong>different processes</strong> — the daemon serves this web app on 8099,
          the cache is its own server on 8380 — so running both is just picking two free ports. And{" "}
          <C>remote-cache start</C> runs in the foreground: it is a server, and it holds the
          terminal until you stop it.
        </P>
        <Pre>{`# ── Terminal 1: the cache server ──────────────────────────────
mkdir -p ~/scratch/ciabatta-cache && cd ~/scratch/ciabatta-cache
ciabatta remote-cache init --port 8380

# Loopback only: this one is for you, not the network. (\`init\` writes
# 0.0.0.0, which is right for a shared cache and wrong for a local test.)
sed -i 's/bind: 0.0.0.0/bind: 127.0.0.1/' remote-cache.yaml

ciabatta remote-cache start          # holds this terminal`}</Pre>
        <Pre>{`# ── Terminal 2: the daemon and your project ───────────────────
# Move this web app off 8099 if something else is using it.
ciabatta daemon restart --port 9099

cd ~/code/my-project
ciabatta remote-cache login http://127.0.0.1:8380
ciabatta cache init --enable --remote http://127.0.0.1:8380

ciabatta run build                   # first build: uploads

rm -rf .ciabatta/cache dist          # pretend to be a colleague's machine
ciabatta run build                   # "restored from the remote cache"

ciabatta remote-cache status         # hit rate, storage, retention`}</Pre>
        <P>
          Open <C>http://127.0.0.1:8380/</C> while it is running: that is the server&apos;s own
          admin page, and on an <C>open</C> cache you can mint a credential there and use it
          straight away with <C>ciabatta remote-cache login</C>.
        </P>
        <P>
          Wiping <C>.ciabatta/cache</C> along with the build output is the whole trick: it leaves
          the workspace looking like a fresh checkout, so the only place the artifacts can come back
          from is the server.
        </P>
        <Alert severity="info" sx={{ mb: 2, maxWidth: "78ch" }}>
          <strong>On the daemon&apos;s port.</strong> <C>--port</C> picks the port a daemon{" "}
          <em>starts</em> on; it does not move one that is already running. A plain{" "}
          <C>ciabatta watch -p 9099</C> with a healthy daemon on 8099 quietly keeps using 8099 — so
          change it with <C>ciabatta daemon restart --port 9099</C>, or export{" "}
          <C>CIABATTA_DAEMON_PORT=9099</C> before the first command that starts one. There is one
          daemon record (<C>~/.ciabatta/daemon.json</C>), so there is one daemon at a time: the port
          moves rather than a second daemon appearing beside the first.
        </Alert>
        <P>
          When you are done, Ctrl-C the server and remove the directory you made — everything it
          stored is under there, and the workspace&apos;s <C>cache.remote</C> section is the only
          trace left in your project.
        </P>

        <SubHeading>Handing out ciabatta itself</SubHeading>
        <P>
          Point the server at the binaries you want your team on. It hashes them, mentions the
          version in every reply, and tells a client running something older. Then{" "}
          <C>ciabatta self update</C> fetches the new build from the server it already trusts,
          checks it against the advertised SHA-256, and only then replaces the binary. The hash
          decides, not the version string, so rebuilding without bumping the version still updates
          everybody.
        </P>
        <P>
          Nothing updates automatically. A build tool that swaps its own binary out from under a
          running CI job is a bad build tool; this notices, tells you, and waits to be asked.
        </P>
      </>
    ),
  },
  {
    id: "remote-cache-ldap",
    title: "LDAP for the remote cache",
    body: (
      <>
        <P>
          <C>auth.mode: ldap</C> hands the question of who somebody is back to the directory you
          already run, so a cache needs no user list of its own: people leave the company in one
          place, and the cache finds out. This is the whole of{" "}
          <C>auth.ldap</C> in the server&apos;s <C>remote-cache.yaml</C> — see{" "}
          <a href="#remote-cache">Remote cache</a> for the server itself.
        </P>
        <P>
          It is two steps, and keeping them apart is what makes the settings below make sense.{" "}
          <strong>Authentication</strong> is a bind: ciabatta connects as the user with the password
          they typed, and if the directory accepts it, the password was right. Nothing is verified
          locally and no credential is stored — ciabatta holds the password only for as long as it
          takes to pass it on. <strong>Authorization</strong> is a second look, after the bind:
          their group memberships decide whether they are allowed in at all, and whether they may
          write.
        </P>

        <SubHeading>Step one: turning a username into a DN</SubHeading>
        <P>
          A bind needs a full DN, and people type usernames. There are two ways across that gap and
          you must configure exactly one of them — the server refuses to start with neither.
        </P>
        <P>
          <strong>A template</strong>, when everyone lives under one branch. Cheapest: no search, no
          service account, one round trip.
        </P>
        <Pre>{`auth:
  mode: ldap
  ldap:
    url: ldaps://ldap.example.com:636
    bind_dn: "uid={username},ou=people,dc=example,dc=com"`}</Pre>
        <P>
          It stops working the moment somebody is in a different OU, which is the usual reason to
          want the other one. <C>bind_dn</C> must contain <C>{"{username}"}</C> — it is a template,
          and a DN without it would log everybody in as the same person, so the server refuses it at
          startup rather than at first login.
        </P>
        <P>
          <strong>A search</strong>, when they are not all in one place. Look the user up first,
          then bind as whatever DN comes back.
        </P>
        <Pre>{`auth:
  mode: ldap
  ldap:
    url: ldaps://ldap.example.com:636
    base_dn: "dc=example,dc=com"
    user_filter: "(uid={username})"          # AD: (sAMAccountName={username})
    # Only if the directory refuses anonymous search:
    search_dn: "cn=ciabatta,ou=services,dc=example,dc=com"
    search_password_env: CIABATTA_LDAP_PASSWORD`}</Pre>
        <P>
          The service account is optional — without <C>search_dn</C> the search is anonymous, which
          plenty of directories allow. If you do set it, <C>search_password_env</C> is required, and
          it names an <em>environment variable</em> rather than holding the password: a password in
          an environment variable is not wonderful, but a password in a config file that ends up in
          git is worse. The variable has to be set in the environment{" "}
          <C>remote-cache start</C> runs in, so it belongs in the service unit or the container
          definition, not in a shell you later close.
        </P>
        <Alert severity="info" sx={{ mb: 2, maxWidth: "78ch" }}>
          Usernames are escaped (RFC 4515) before they are substituted into either the template or
          the filter, so a name containing <C>)</C> or <C>*</C> cannot rewrite the query it lands
          in. You do not need to sanitise anything yourself.
        </Alert>

        <SubHeading>Step two: who gets in, and who may write</SubHeading>
        <P>
          After the bind, ciabatta reads the user&apos;s own entry for{" "}
          <C>group_attribute</C> — <C>memberOf</C> unless you say otherwise — and compares it
          against two lists. Comparisons ignore case, and the values are whatever your directory
          puts in that attribute, which is normally a full group DN rather than a bare name.
        </P>
        <Bullets
          items={[
            <>
              <C>required_group</C> — anyone not in it is refused, even though their password was
              correct. <strong>Without it, every account in the directory can read the cache</strong>
              , which is a larger set of people than it sounds.
            </>,
            <>
              <C>write_groups</C> — members may write; everyone else who got in is read-only. An
              empty list (the default) means everyone who authenticates may write.
            </>,
          ]}
        />
        <P>
          Read access is a convenience. <strong>Write access is trust</strong>: whoever can write to
          a cache decides what everyone else&apos;s build produces, because that is precisely what a
          cache hand-back is. A reasonable shape is engineers read, CI writes.
        </P>
        <Pre>{`    required_group: "cn=engineering,ou=groups,dc=example,dc=com"
    group_attribute: memberOf
    write_groups:
      - "cn=ci,ou=groups,dc=example,dc=com"`}</Pre>
        <Alert severity="warning" sx={{ mb: 2, maxWidth: "78ch" }}>
          <strong>Nobody is an admin on an LDAP cache.</strong> The directory is the user list, so
          there is nothing to mint and no user-management API to reach: LDAP never grants{" "}
          <C>admin</C>, and <C>auth.users</C> is not consulted in this mode at all. Membership
          changes belong in the directory. (The 403 you get from{" "}
          <C>/api/users</C> suggests adding <C>admin: true</C> under <C>auth.users</C>; that advice
          is for a <C>token</C> server and will not work here.)
        </Alert>

        <SubHeading>TLS</SubHeading>
        <P>
          <C>tls_verify</C> defaults to on and should stay on. It matters more here than anywhere
          else in ciabatta: LDAPS with verification off is an encrypted channel to{" "}
          <em>whoever answered the connection</em>, and for a protocol whose entire job is to say
          who somebody is, that is worse than useless — it means handing every password typed into{" "}
          <C>remote-cache login</C> to whatever the network pointed you at. Turn it off only against
          a directory you are running yourself to try this out.
        </P>

        <SubHeading>Every setting</SubHeading>
        <LdapSettingsTable />

        <SubHeading>What a developer does</SubHeading>
        <P>
          Nothing LDAP-specific. The client asks the server what it wants and prompts accordingly —
          for a password rather than a token, in this mode.
        </P>
        <Pre>{`ciabatta remote-cache login https://cache.example.com
# Username for https://cache.example.com: ada
# Password for https://cache.example.com:`}</Pre>
        <P>
          What comes back is a session, not the password: the server issues a bearer token, keeps
          only its SHA-256, and the client stores it in{" "}
          <C>~/.ciabatta/remote-cache.json</C> keyed by server URL. The directory is contacted once,
          at login — every build after that is the session token, so a slow directory costs one
          login rather than one round trip per step. Sessions last{" "}
          <C>auth.session_ttl</C> (30 days by default, and it is not LDAP-specific); after that,{" "}
          <C>login</C> again. Revoking someone in the directory stops them logging in again, but
          does not expire a session they already hold — shorten <C>session_ttl</C> if that gap
          matters to you.
        </P>
        <P>
          Non-interactive callers pass <C>--username</C> and <C>--password-env</C>, which is the
          shape CI wants:
        </P>
        <Pre>{`ciabatta remote-cache login https://cache.example.com \\
  --username ci --password-env CIABATTA_CACHE_PASSWORD`}</Pre>

        <SubHeading>Trying it before you point a team at it</SubHeading>
        <P>
          A throwaway directory is the fastest way to find out whether your filter is right. This
          one comes with a handful of users, all with the password <C>ada</C>:
        </P>
        <Pre>{`docker run --rm -p 1389:1389 \\
  -e LDAP_USERS=ada,grace -e LDAP_PASSWORDS=ada,grace \\
  -e LDAP_ROOT=dc=example,dc=org \\
  bitnami/openldap:latest`}</Pre>
        <Pre>{`# remote-cache.yaml — a test directory, so plaintext and no verification
auth:
  mode: ldap
  ldap:
    url: ldap://127.0.0.1:1389
    bind_dn: "cn={username},ou=users,dc=example,dc=org"
    tls_verify: false`}</Pre>
        <P>
          Then <C>ciabatta remote-cache start</C> in one terminal and{" "}
          <C>ciabatta remote-cache login http://127.0.0.1:8380</C> in another. A wrong filter shows
          up immediately as <C>Invalid username or password</C>; a wrong{" "}
          <C>required_group</C> shows up as the more specific message below, which is how you tell
          the two apart.
        </P>

        <SubHeading>When it doesn&apos;t work</SubHeading>
        <P>
          Settings are validated at <em>startup</em>, deliberately: a cache configured for LDAP with
          no <C>ldap:</C> block refuses to start rather than serving happily for a week and then
          failing the first person who tries to log in. So most mistakes are a server that will not
          come up, with the reason on stderr.
        </P>
        <Table size="small" sx={{ minWidth: 620, mb: 2 }}>
          <TableHead>
            <TableRow>
              <TableCell sx={{ width: "45%" }}>What you see</TableCell>
              <TableCell>What it means</TableCell>
            </TableRow>
          </TableHead>
          <TableBody>
            <TableRow hover>
              <TableCell sx={{ fontFamily: monoFontStack, fontSize: 13, verticalAlign: "top" }}>
                auth.mode is &apos;ldap&apos; but there is no auth.ldap section
              </TableCell>
              <TableCell>
                <Typography variant="body2" color="text.secondary">
                  The mode is set but the block is missing or commented out.
                </Typography>
              </TableCell>
            </TableRow>
            <TableRow hover>
              <TableCell sx={{ fontFamily: monoFontStack, fontSize: 13, verticalAlign: "top" }}>
                auth.ldap needs either bind_dn or base_dn
              </TableCell>
              <TableCell>
                <Typography variant="body2" color="text.secondary">
                  Neither route to a DN is configured. Pick one of the two above.
                </Typography>
              </TableCell>
            </TableRow>
            <TableRow hover>
              <TableCell sx={{ fontFamily: monoFontStack, fontSize: 13, verticalAlign: "top" }}>
                auth.ldap.bind_dn must contain {"{username}"}
              </TableCell>
              <TableCell>
                <Typography variant="body2" color="text.secondary">
                  A literal DN was given where a template belongs — it would authenticate everyone
                  as one person.
                </Typography>
              </TableCell>
            </TableRow>
            <TableRow hover>
              <TableCell sx={{ fontFamily: monoFontStack, fontSize: 13, verticalAlign: "top" }}>
                search_dn is set but search_password_env isn&apos;t
              </TableCell>
              <TableCell>
                <Typography variant="body2" color="text.secondary">
                  A service account with no way to authenticate it.
                </Typography>
              </TableCell>
            </TableRow>
            <TableRow hover>
              <TableCell sx={{ fontFamily: monoFontStack, fontSize: 13, verticalAlign: "top" }}>
                …names CIABATTA_LDAP_PASSWORD, but it isn&apos;t set
              </TableCell>
              <TableCell>
                <Typography variant="body2" color="text.secondary">
                  The variable is missing from the environment the <em>server</em> runs in. Usually
                  a service unit that never got it.
                </Typography>
              </TableCell>
            </TableRow>
            <TableRow hover>
              <TableCell sx={{ fontFamily: monoFontStack, fontSize: 13, verticalAlign: "top" }}>
                The LDAP service account could not bind
              </TableCell>
              <TableCell>
                <Typography variant="body2" color="text.secondary">
                  <C>search_dn</C> or its password is wrong. This is the search account, not the
                  person logging in.
                </Typography>
              </TableCell>
            </TableRow>
            <TableRow hover>
              <TableCell sx={{ fontFamily: monoFontStack, fontSize: 13, verticalAlign: "top" }}>
                Invalid username or password
              </TableCell>
              <TableCell>
                <Typography variant="body2" color="text.secondary">
                  Deliberately ambiguous — it is the same whether the account doesn&apos;t exist,
                  the filter didn&apos;t match it, or the password was wrong, so the endpoint
                  can&apos;t be used to enumerate accounts. Check <C>user_filter</C> against the
                  directory before assuming a typo.
                </Typography>
              </TableCell>
            </TableRow>
            <TableRow hover>
              <TableCell sx={{ fontFamily: monoFontStack, fontSize: 13, verticalAlign: "top" }}>
                ada authenticated, but is not a member of cn=engineering,…
              </TableCell>
              <TableCell>
                <Typography variant="body2" color="text.secondary">
                  The password was right and <C>required_group</C> refused them. If it is wrong,
                  compare against what <C>group_attribute</C> actually contains — usually full DNs,
                  not bare names.
                </Typography>
              </TableCell>
            </TableRow>
            <TableRow hover>
              <TableCell sx={{ fontFamily: monoFontStack, fontSize: 13, verticalAlign: "top" }}>
                ada has read-only access to this cache
              </TableCell>
              <TableCell>
                <Typography variant="body2" color="text.secondary">
                  They got in, but are in none of <C>write_groups</C>. Builds still read from the
                  cache; only the upload is refused.
                </Typography>
              </TableCell>
            </TableRow>
          </TableBody>
        </Table>
        <P>
          An empty password is rejected before the directory is contacted at all: most directories
          treat a bind with no password as an anonymous bind and <em>succeed</em>, which would let
          anyone in as anyone.
        </P>
      </>
    ),
  },
  {
    id: "analyze",
    title: "Analyze",
    body: (
      <>
        <P>
          The project&apos;s dependency graph: internal packages, external dependencies, and where
          artifacts get published. Filter nodes by name to cut a large graph down to the part you
          care about.
        </P>
        <P>
          Scans run on the daemon and are one-at-a-time per project — the page shows a scan in
          flight rather than starting a second. Optionally the scan checks dependencies against the{" "}
          <strong>OSV</strong> vulnerability database, which makes it slower and needs network
          access.
        </P>
      </>
    ),
  },
  {
    id: "ai",
    title: "AI",
    body: (
      <>
        <SubHeading>Mind map</SubHeading>
        <P>
          The architecture map the assistant builds as it learns the codebase: architectures at the
          centre, the files belonging to each around them, with a confidence score. Run{" "}
          <C>ciabatta ai burn-in</C> to have it traverse the codebase up front, or just start asking
          questions — it learns as it goes.
        </P>
        <P>
          The assistant proposes tags rather than applying them. Pending proposals are listed under
          the map to accept or reject, individually or in bulk, and selecting a node lets you{" "}
          <strong>forget</strong> a file or an entire architecture when the map has learned
          something wrong.
        </P>
        <SubHeading>Jobs</SubHeading>
        <P>
          Background tasks and their output. Ship one with <C>ciabatta ai ship &quot;…&quot;</C> or
          from the <Link to="/todo">Todo page</Link>. Questions asked from here are serialized per
          project, so two callers can&apos;t interleave one conversation.
        </P>
      </>
    ),
  },
  {
    id: "api",
    title: "The HTTP API",
    body: (
      <>
        <P>
          Everything this app does, it does through the routes below — there is no private channel.
          Anything here is equally available to <C>curl</C>, a script, or an editor plugin.
        </P>
        <P>
          Project-scoped routes take the project id as <C>?project=&lt;id&gt;</C> (or a{" "}
          <C>project</C> field in the body). Errors come back as <C>{'{ "error": "…" }'}</C> with a
          meaningful status; some carry structured fields as well, like the <C>missing_env</C> list
          on a rejected run.
        </P>
        <Pre>{`TOKEN=$(jq -r .token ~/.ciabatta/daemon.json)
PORT=$(jq -r .port  ~/.ciabatta/daemon.json)

curl -s "http://127.0.0.1:$PORT/api/health"

curl -s -H "Authorization: Bearer $TOKEN" \\
  "http://127.0.0.1:$PORT/api/projects"

curl -N -H "Authorization: Bearer $TOKEN" \\
  "http://127.0.0.1:$PORT/api/watch/sessions/1/stream"`}</Pre>
        <EndpointTable />
      </>
    ),
  },
  {
    id: "security",
    title: "Tokens and access",
    body: (
      <>
        <P>
          The daemon generates a token at startup and records it in{" "}
          <C>~/.ciabatta/daemon.json</C> alongside its port and pid. Every route except{" "}
          <C>/api/health</C> requires it as <C>Authorization: Bearer &lt;token&gt;</C>.
        </P>
        <P>
          There is no login flow because there is nothing to log into: the daemon injects the token
          into the page it serves as a <C>&lt;meta name=&quot;ciabatta-token&quot;&gt;</C> tag.
          Anyone who can load the page can already read the token file, so this costs a local user
          nothing — and it keeps mutating routes closed when the daemon is bound somewhere other
          than loopback. <C>EventSource</C> can&apos;t set headers, so streams accept{" "}
          <C>?token=</C> instead.
        </P>
        <Alert severity="warning" sx={{ my: 2, maxWidth: "78ch" }}>
          This API starts processes. Bound to anything but loopback, anyone who can reach the port
          and read the token can run commands as you. The daemon logs a warning when you do it;
          treat that as the whole security model.
        </Alert>
      </>
    ),
  },
  {
    id: "development",
    title: "Working on this app",
    body: (
      <>
        <P>
          The app is a Vite + React bundle in the <C>tool_frontend</C> workspace, compiled into the
          Rust binary. A released ciabatta is still a single file with no asset directory beside it.
        </P>
        <SubHeading>Dev server</SubHeading>
        <P>
          Vite serves on 5173 and proxies <C>/api</C> to a real daemon on 8099 (override with{" "}
          <C>CIABATTA_DAEMON_PORT</C>), so HMR runs against live data and everything stays
          same-origin. Vite serves its own <C>index.html</C>, so there is no injected token — pass
          it once as <C>?token=…</C> and it is remembered.
        </P>
        <Pre>{`ciabatta daemon serve --port 8099   # in one terminal
yarn workspace ciabatta-tool-frontend dev

# then open http://localhost:5173/?token=$(jq -r .token ~/.ciabatta/daemon.json)`}</Pre>
        <SubHeading>Building</SubHeading>
        <Pre>{`yarn install
yarn turbo run build --filter=ciabatta-tool-frontend
cargo build --release`}</Pre>
        <P>
          The Rust build embeds <C>tool_frontend/dist</C>. On a fresh clone that directory
          doesn&apos;t exist, so <C>build.rs</C> substitutes a placeholder page rather than failing
          the build — that way <C>cargo build</C> works without node installed. If you are reading a
          page that says the web app isn&apos;t built, that is what happened: run the yarn build and
          recompile. CI and the release workflow always build the bundle first.
        </P>
        <SubHeading>Adding a page</SubHeading>
        <Bullets
          items={[
            <>
              Routing is code-based in <C>src/router.tsx</C> — no codegen, because the bundle is
              compiled into a binary and generated-file drift is not worth the convenience.
            </>,
            <>
              Add the nav entry in <C>src/components/AppShell.tsx</C>, and use{" "}
              <C>PageHeader</C> / <C>RequireProject</C> from <C>src/components/Page.tsx</C> so the
              page looks like the others.
            </>,
            <>
              Register more specific routes before parameterised ones — <C>/run/builder</C> has to
              come before <C>/run/$runId</C>.
            </>,
            <>And add a section here, so the docs ship with the feature.</>,
          ]}
        />
      </>
    ),
  },
];

// ─── The page ───────────────────────────────────────────────────────────────

export function DocsPage() {
  const { data: health } = useHealth();

  return (
    <Box>
      <PageHeader
        title="Docs"
        description="How this app works, what each tool is for, and the API underneath it — shipped in the same binary, so it always matches what you're running."
      />

      <Grid container spacing={4}>
        <Grid size={{ xs: 12, lg: 9 }}>
          {SECTIONS.map((section, index) => (
            <Box
              key={section.id}
              id={section.id}
              component="section"
              sx={{ scrollMarginTop: `${ANCHOR_OFFSET}px` }}
            >
              {index > 0 && <Divider sx={{ my: 4 }} />}
              <Typography variant="h2" sx={{ mb: 1.5 }}>
                {section.title}
              </Typography>
              {section.body}
            </Box>
          ))}

          <Divider sx={{ my: 4 }} />
          <Typography variant="caption" color="text.secondary">
            {health
              ? `These docs ship with ciabatta ${health.version} — the daemon answering this page.`
              : "These docs ship with the binary serving this page."}
          </Typography>
        </Grid>

        {/* On narrower screens the nav rail already competes for width, and the
            sections are short enough to scroll. */}
        <Grid size={{ lg: 3 }} sx={{ display: { xs: "none", lg: "block" } }}>
          <Box sx={{ position: "sticky", top: ANCHOR_OFFSET }}>
            <Typography variant="overline" color="text.secondary">
              On this page
            </Typography>
            <List dense disablePadding>
              {SECTIONS.map((section) => (
                <ListItemButton
                  key={section.id}
                  component="a"
                  href={`#${section.id}`}
                  sx={{ borderRadius: 1, py: 0.25 }}
                >
                  <ListItemText
                    primary={section.title}
                    primaryTypographyProps={{ variant: "body2" }}
                  />
                </ListItemButton>
              ))}
            </List>
          </Box>
        </Grid>
      </Grid>
    </Box>
  );
}
