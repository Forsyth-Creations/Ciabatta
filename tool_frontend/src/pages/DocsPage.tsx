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

/**
 * A two-column table of config fields and what they do.
 *
 * A bulleted list of twenty settings scans as prose and reads as none: the
 * thing you came for is a name, and a name in a column is findable.
 */
function FieldTable({ rows }: { rows: [string, string][] }) {
  return (
    <Box sx={{ my: 2, maxWidth: "78ch", border: 1, borderColor: "divider", borderRadius: 1 }}>
      <Table size="small">
        <TableBody>
          {rows.map(([field, meaning]) => (
            <TableRow key={field}>
              <TableCell
                sx={{
                  fontFamily: monoFontStack,
                  fontSize: 13,
                  fontWeight: 600,
                  verticalAlign: "top",
                  whiteSpace: "nowrap",
                  // A literal 1 would be 100% in MUI's shorthand, which hands
                  // the whole table to the names column.
                  width: "1%",
                }}
              >
                {field}
              </TableCell>
              <TableCell sx={{ verticalAlign: "top" }}>
                <Typography variant="body2" color="text.secondary" sx={{ lineHeight: 1.6 }}>
                  {meaning}
                </Typography>
              </TableCell>
            </TableRow>
          ))}
        </TableBody>
      </Table>
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
    name: "Running a workflow",
    blurb:
      "There is one thing to run, so there is nearly one command to run it. A workflow is a named DAG of steps; every package that declares that name joins in, and the whole thing compiles to a single graph.",
    commands: [
      {
        usage: "ciabatta <WORKFLOW> [ALSO…]",
        note: "Any name that isn't one of ciabatta's own commands is a workflow: `ciabatta build`, `ciabatta test`. Naming several folds them into one graph, so a dependency both of them need runs once.",
        flags: [
          ["-f, --filter TERM", "Run only the steps this selects. Repeatable."],
          ["--only MEMBER", "Start from these sub-workspaces; their dependencies still come along."],
          ["--isolated", "Don't follow dependencies into other sub-workspaces."],
          ["--graph", "Print the resolved graph and run nothing."],
          ["--dry-run", "Walk every step, executing none of them."],
          ["-e KEY=VALUE", "Set a variable for every step. Beats .env and CI."],
          ["--gui", "Watch it live in this app."],
          ["--tui", "Watch it in the terminal UI. Runs print plain text by default."],
          ["--authoritative", "Run each step against only the files it declared — the way to find an incomplete inputs list."],
        ],
      },
      {
        usage: "ciabatta workflow build",
        note: "The same thing, spelled out. Use this longer form when a workflow's name collides with one of ciabatta's commands — a workflow called `list` or `watch` needs it. Alias: `ciabatta wf`.",
      },
      {
        usage: "ciabatta dry-run build --diff",
        note: "What a run would reuse and what it would rebuild, without running it. For a rebuild it names what changed: which input files (with the lines), which variables, which upstream steps.",
      },
    ],
  },
  {
    name: "Seeing what exists, and why",
    blurb:
      "The questions a monorepo usually can't answer: what is there to run, who owns it, and why did that rebuild.",
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
        usage: "ciabatta why api:build",
        note: "Everything one target is defined by: the file it's declared in, the directory it runs in, what it needs, the files it reads and writes, the variables it keys on, the commands it runs, and what the cache would do with all of that.",
        flags: [
          ["-a, --all", "Name every input file instead of counting them — how you find the one that shouldn't be there."],
          ["--json", "The same answer, for a script."],
        ],
      },
      {
        usage: "ciabatta build --graph",
        note: "The resolved graph in wave order, and per step what it does, who owns it, what it waits for and what waits on it. Honours --filter.",
      },
      {
        usage: "ciabatta config show | reference",
        note: "The resolved configuration, and the full config file schema.",
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
      {
        usage: "ciabatta convert --script scripts/build.sh",
        note: "Read an existing script, work out what it needs and what it produces, and write it into .ciabatta/ as a workflow so it can join the graph like everything else.",
      },
      { usage: "ciabatta configure", note: "Set up registries interactively." },
      {
        usage: "ciabatta register",
        note: "Tell the daemon this checkout exists, so it appears in the project switcher. Every web-facing command does this for the directory it ran in — this is for a checkout nothing has been run in yet.",
        flags: [
          ["--path DIR", "Register that directory instead of the current one."],
          ["--quiet", "Print just the project id, for a script."],
        ],
      },
    ],
  },
  {
    name: "The cache",
    blurb:
      "Caching is off until a workspace opts in: a cache that turns itself on is a cache that will one day serve a stale artifact nobody asked it to keep.",
    commands: [
      {
        usage: "ciabatta cache init [WORKFLOW]",
        note: "Look at what is actually in the directory and write a `cache:` section into the workflow's file proposing its inputs and outputs — with the paths already rooted correctly.",
      },
      {
        usage: "ciabatta cache status",
        note: "What the local cache is holding, and what it has saved.",
      },
      {
        usage: "ciabatta cache prune | clean",
        note: "Apply a retention policy, or empty the store for this project.",
      },
      {
        usage: "ciabatta remote-cache <init|start|login|status|add-user>",
        note: "Run a shared cache for the team, or log this machine in to one. See the remote cache section.",
      },
    ],
  },
  {
    name: "Publishing",
    blurb:
      "Publishing is a step, not a command. A step with kind: push moves an artifact to a registry; it sits on the graph, declares what it needs, and so cannot run before the artifact exists.",
    commands: [
      {
        usage: "ciabatta release --filter kind:push",
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
      { usage: "ciabatta self update", note: "Update this binary from the remote cache serving it." },
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
  /**
   * Which half of the page this belongs to: the walkthrough, or the reference
   * behind it.
   *
   * The page is a tutorial with a manual stapled to the back, and a flat
   * contents list of fifteen entries hid that — somebody landing here could not
   * tell "read this first" from "look this up when you need it".
   */
  group: "Start here" | "Reference";
}

const SECTIONS: DocSection[] = [
  {
    id: "inputs",
    group: "Start here",
    title: "Ciabatta cares about inputs",
    body: (
      <>
        <P>
          Everything else in this tool follows from one idea: <strong>a step is defined by what
          goes into it.</strong> Not by the command it runs — by the command <em>and</em> the files
          that command reads <em>and</em> the environment variables it reads <em>and</em> whatever
          had to happen first.
        </P>
        <P>Ciabatta calls those the three inputs of a step, and a step has exactly three:</P>
        <Bullets
          items={[
            <>
              <strong>Files</strong> — the sources it reads, declared as globs in{" "}
              <C>cache.inputs</C>. Change one and the step runs again.
            </>,
            <>
              <strong>Environment variables</strong> — the values it reads. Some decide{" "}
              <em>whether</em> it can run at all (<C>REQUIRED_ENV</C>); some decide{" "}
              <em>what it produces</em>, and those belong in <C>cache.env</C>.
            </>,
            <>
              <strong>The steps it needs</strong> — named in <C>needs</C>. If one of them produced
              something different, this step runs again too.
            </>,
          ]}
        />
        <P>
          Declare those and ciabatta can answer the questions a build system is actually asked:
          why did this rebuild, what will this run do before I start it, what does this step need
          that my machine doesn&apos;t have, and can we skip it. Leave them undeclared and it
          can&apos;t — and worse, a step that quietly reads a file nobody listed will one day be
          handed a stale result and nobody will notice for a week.
        </P>
        <Alert severity="info" sx={{ my: 2, maxWidth: "78ch" }}>
          This page is the walkthrough: set a project up, make a run depend on files, make it
          depend on variables, and write the workflow file that says so. Everything is a{" "}
          <a href="#commands">command you can copy</a>.
        </Alert>
        <P>
          The fastest way to see all of it working is to generate the worked example — four
          packages that really depend on each other, every step of which runs on a bare machine:
        </P>
        <Pre>{`ciabatta init --example        # writes ./ciabatta-example
cd ciabatta-example
ciabatta list                  # what exists, and who owns it
ciabatta build --graph         # the resolved graph, running nothing
ciabatta build                 # actually run it`}</Pre>
      </>
    ),
  },
  {
    id: "setup",
    group: "Start here",
    title: "Set a project up",
    body: (
      <>
        <P>
          A ciabatta project is a directory with a <C>.ciabatta/</C> in it. A monorepo is several
          of those, one per package, plus one at the root that owns the shared settings. Nothing
          else is required — no central manifest listing the packages, because the packages say
          who they are.
        </P>

        <SubHeading>1. Opt each package in</SubHeading>
        <P>
          Run this in each package. It writes the package&apos;s identity and a starter workflow,
          and asks for a description and an owner — on purpose, because those are what make{" "}
          <C>ciabatta list</C> worth reading six months later.
        </P>
        <Pre>{`cd packages/api
ciabatta init --lib --depends-on proto:generate`}</Pre>
        <P>Which leaves you with two files, and it is worth knowing which is which:</P>
        <Bullets
          items={[
            <>
              <C>.ciabatta/ciabatta.yaml</C> — <strong>who this package is.</strong> Its name,
              owner, tags, which packages it depends on, its <C>.env</C> file, and (at the root)
              the shared <C>toolchain:</C>, the registries, and the cache server.
            </>,
            <>
              <C>{".ciabatta/workflows/<name>.yaml"}</C> — <strong>what it can do.</strong> One
              file per workflow, and <em>the filename is the workflow&apos;s name</em>:{" "}
              <C>build.yaml</C> is what <C>ciabatta build</C> runs.
            </>,
          ]}
        />
        <Pre>{`my-repo/
  .ciabatta/ciabatta.yaml              # the root: umbrella: true, toolchain, shared env
  packages/
    proto/.ciabatta/ciabatta.yaml      # workspace: name: proto
    proto/.ciabatta/workflows/generate.yaml
    api/.ciabatta/ciabatta.yaml        # workspace: depends_on: [proto:generate, common]
    api/.ciabatta/workflows/build.yaml
    api/.ciabatta/workflows/test.yaml`}</Pre>

        <SubHeading>2. Say what the package is</SubHeading>
        <Pre>{`# packages/api/.ciabatta/ciabatta.yaml
workspace:
  name: api
  description: The public REST/gRPC service
  owner: API Team
  tags: [backend, service]

  # Cross-package dependencies, declared once for every workflow here:
  # "<member>" means that member's workflow of the same name,
  # "<member>:<workflow>" means one specific workflow.
  depends_on: [proto:generate, common]

  # Tools every workflow here needs on PATH. Missing ones are reported before
  # anything runs, with the install command from the root's toolchain: section.
  requires: [sh, cargo]`}</Pre>
        <P>
          The root config is the same file with <C>umbrella: true</C>, which says &quot;this
          directory is not a package of its own&quot; — it is where the shared <C>toolchain:</C>{" "}
          hints and the variables every package inherits live.
        </P>

        <SubHeading>3. Write a workflow</SubHeading>
        <P>
          Steps declare order with <C>needs</C>, and the order in the file means nothing. This is
          a complete, working file:
        </P>
        <Pre>{`# packages/api/.ciabatta/workflows/build.yaml
description: Build the api binary
owner: API Team
requires: [sh]

steps:
  - name: compile
    description: Compile the service binary into dist/api
    run: cargo build --release
    tags: [slow]
    timeout: 10m

  - name: package
    description: Tar the binary up for publishing
    run: tar czf dist/api.tgz -C target/release api
    needs: [compile]`}</Pre>
        <P>
          Already got a shell script that does this? <C>ciabatta convert --script
          scripts/build.sh</C> reads it, works out what it needs and what it produces, and writes
          the workflow for you.
        </P>

        <SubHeading>4. Run it, and look at it</SubHeading>
        <Pre>{`ciabatta list                  # every package, workflow, owner and dependency
ciabatta build                 # run it across every package that defines build
ciabatta build --graph         # …or just show me the graph
ciabatta build --gui           # …or watch it in this app
ciabatta register              # make this checkout appear in the project switcher`}</Pre>
        <P>
          From here the two things worth declaring are the inputs: the{" "}
          <a href="#files">files</a> each step reads, and the{" "}
          <a href="#env">variables</a> it depends on.
        </P>
      </>
    ),
  },
  {
    id: "files",
    group: "Start here",
    title: "Make a run depend on files",
    body: (
      <>
        <P>
          A step that declares the files it reads can be skipped when none of them changed, and
          can explain itself when they did. That declaration is the <C>cache:</C> block, and it
          lives in the workflow file rather than in the package config — because the files a{" "}
          <C>build</C> reads are not the files a <C>test</C> reads.
        </P>
        <P>Let ciabatta propose it from what is actually in the directory:</P>
        <Pre>{`cd packages/api
ciabatta cache init build      # writes a cache: section into workflows/build.yaml`}</Pre>
        <Pre>{`# packages/api/.ciabatta/workflows/build.yaml
cache:
  enabled: true
  # Every path is relative to the WORKSPACE ROOT, not to this file — note the
  # packages/api/ on each one.
  inputs:  ["packages/api/src/**/*", "packages/api/Cargo.toml", "Cargo.lock"]
  outputs: ["packages/api/target/release/api"]
  exclude: ["packages/api/target"]   # its own output is not one of its inputs
  env:     [PROFILE]                 # variables the RESULT depends on

steps:
  - name: compile
    run: cargo build --release`}</Pre>
        <Alert severity="warning" sx={{ my: 2, maxWidth: "78ch" }}>
          <strong>Paths are relative to the workspace root, wherever the block is written.</strong>{" "}
          One project is one cache, so one directory has to be what those paths mean. Getting the
          prefix wrong is the single most common mistake here, and{" "}
          <C>ciabatta why api:compile</C> is the fastest way to catch it — it prints what the
          globs actually matched.
        </Alert>

        <SubHeading>Then ask it what it thinks</SubHeading>
        <Pre>{`ciabatta why api:compile        # where it's declared, what it reads, what it writes
ciabatta why api:compile --all  # …naming every input file, in hash order
ciabatta dry-run build --diff   # what would rebuild, and which lines changed
ciabatta cache status           # what the local cache is holding, and what it saved`}</Pre>
        <P>
          The <Link to="/cache">Cache page</Link> is the same information with the diffs already
          expanded, and the <Link to="/run">Run page</Link>&apos;s <strong>Files</strong> toggle
          draws the file sets as nodes feeding the steps that read them — which is the graph the
          caching decision is actually made from.
        </P>

        <SubHeading>Proving the list is complete</SubHeading>
        <P>
          An <C>inputs</C> list that is missing a file is worse than no list at all: the step gets
          a cache hit it didn&apos;t earn. There is a flag that finds those, by running each step
          against <em>only</em> what it declared:
        </P>
        <Pre>{`ciabatta build --authoritative`}</Pre>
        <P>
          A step that reads something it never declared can&apos;t find it there and fails now,
          with its sandbox left on disk to look at — instead of quietly being handed a stale
          result weeks later. Add <C>--sandbox-also node_modules</C> for the ambient state that
          genuinely isn&apos;t a source file.
        </P>
      </>
    ),
  },
  {
    id: "env",
    group: "Start here",
    title: "Make a run depend on environment variables",
    body: (
      <>
        <P>
          Variables are the other half of what goes into a step, and there are three separate
          questions about them. Keeping them apart is most of what makes this straightforward:
        </P>
        <Bullets
          items={[
            <>
              <strong>Where does the value come from?</strong> — the <C>.env</C> chain, the
              ambient environment, CI, <C>-e</C>.
            </>,
            <>
              <strong>Must it be set?</strong> — <C>REQUIRED_ENV</C>, checked before anything
              runs.
            </>,
            <>
              <strong>Does the result depend on it?</strong> — <C>cache.env</C>, which folds it
              into the cache key.
            </>,
          ]}
        />

        <SubHeading>Where values come from</SubHeading>
        <P>Precedence, weakest first:</P>
        <Pre>{`.env files  →  CI-derived  →  the ambient environment  →  -e KEY=VALUE`}</Pre>
        <P>
          <strong>Among the files themselves, nearest wins.</strong> A step in{" "}
          <C>packages/api</C> reads <C>packages/api/.env</C>; whatever that file doesn&apos;t set
          comes from the workspace above it, up to the monorepo root. A sibling package&apos;s{" "}
          <C>.env</C> is never a fallback. Each step&apos;s chain is listed under the step on the{" "}
          <Link to="/run">Run page</Link>, so &quot;which file did this value come from?&quot; has
          a visible answer.
        </P>
        <Pre>{`# packages/api/.ciabatta/ciabatta.yaml
workspace:
  env_file: .env              # the default; set it to point somewhere else
  env_default: .env.example   # the checked-in template .env is generated from
  env:
    LOG_LEVEL: info           # a plain value every step here starts with`}</Pre>
        <P>
          A missing <C>.env</C> is generated from that template at the start of a run, for the
          project and for every sub-workspace the run touches. It never overwrites a file that
          exists. Commit the template, never the <C>.env</C>.
        </P>

        <SubHeading>Refusing to start without one</SubHeading>
        <Pre>{`# packages/api/.ciabatta/workflows/build.yaml
REQUIRED_ENV: [API_URL, DATABASE_URL]`}</Pre>
        <P>
          Checked before the first step runs, so a missing token fails in a second with the name
          of what&apos;s missing, rather than fifteen minutes in with a stack trace. Starting the
          run from this app asks you for the values instead of refusing — that is the same check,
          answered in a dialog.
        </P>

        <SubHeading>Variables the result depends on</SubHeading>
        <P>
          This is the one people miss. A build whose output differs by <C>PROFILE</C> has to{" "}
          <em>say</em> so, or switching profiles silently reuses the other one&apos;s artifacts:
        </P>
        <Pre>{`cache:
  env: [PROFILE, TARGET_ARCH]`}</Pre>
        <P>
          Ciabatta can tell you which ones you forgot. It records every variable a step actually
          reads — from its command, its working directory and its conditions — and the{" "}
          <Link to="/run">Run page</Link> flags any that are read but not declared, because those
          are precisely the ones a change to which will not invalidate anything.
        </P>

        <SubHeading>Using one in a step</SubHeading>
        <Pre>{`steps:
  - name: deploy
    run: ./deploy.sh --to "$RUN_ENV"
    # Only in production, and never from a developer's laptop:
    when: env.RUN_ENV == prod
    skip_if: env.IS_LOCAL
    env:
      DEPLOY_TIMEOUT: 600     # this step only, layered over the run's`}</Pre>

        <SubHeading>When somebody changes the variables</SubHeading>
        <P>
          Ciabatta snapshots the names its <C>.env</C> files define, and value <em>hashes</em> —
          never the values. When they move, because someone pulled a branch that adds a required
          variable, the next run says which ones changed before it starts. The same drift shows as
          a banner above the launcher on the <Link to="/run">Run page</Link>.
        </P>
        <Pre>{`ciabatta build -e API_URL=http://localhost:8080   # one run, one value
eval "$(ciabatta source)"                        # load the CIABATTA_* vars into your shell`}</Pre>
      </>
    ),
  },

  {
    id: "features",
    group: "Start here",
    title: "Build features are inputs too",
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
    id: "workflow-file",
    group: "Start here",
    title: "What goes in a workflow file",
    body: (
      <>
        <P>
          One file, one workflow, named after the file. Here is a realistic one using most of what
          there is — a build with a slow step that can time out, retry, and fall through to a
          recovery branch if it still fails:
        </P>
        <Pre>{`# packages/api/.ciabatta/workflows/build.yaml
description: Build and package the api binary
owner: API Team
requires: [sh, cargo]            # checked before anything runs
tags: [backend]                  # every step here inherits these

REQUIRED_ENV: [API_URL]

# Other packages' workflows that must finish first, on top of whatever
# workspace.depends_on already says.
needs: [proto:generate]

# Started before the first wave and stopped when the run ends. Nothing waits
# for these — they're the mock API the integration tests call.
background: [mock-api:serve]

cache:
  enabled: true
  inputs:  ["packages/api/src/**/*", "packages/api/Cargo.toml", "Cargo.lock"]
  outputs: ["packages/api/target/release/api", "packages/api/dist/**/*"]
  exclude: ["packages/api/target"]
  env:     [PROFILE]

steps:
  - name: compile
    description: Compile the service binary
    run: cargo build --release
    tags: [slow]
    timeout: 10m
    retries: 1                   # one more go, for a flaky mirror
    on_error: fix-build          # and if it still fails, go here

  - name: package
    description: Tar the binary up for publishing
    run: tar czf dist/api.tgz -C target/release api
    needs: [compile]             # names a step in THIS file

  - name: smoke
    description: Hit the packaged service once
    script: scripts/smoke.sh     # a bash script, path relative to the step's cwd
    needs: [package]
    continue_on_error: true      # report it, don't take the run down
    when: env.RUN_SMOKE          # only when that's truthy

  # A recovery node: not part of the success graph, entered only when compile
  # fails. In a terminal you're offered the choice; in this app it's a button.
  - name: fix-build
    recover: true
    retry: compile               # re-run this once a fix succeeds
    message: "api failed to build. What should I try?"
    options:
      - label: Clean the output directory and rebuild
        run: rm -rf target dist
        default: true            # what CI picks, where nobody is watching
      - label: Regenerate the stubs, in case they're stale
        run: cd ../proto && sh scripts/generate.sh`}</Pre>

        <SubHeading>The fields, in one place</SubHeading>
        <P>Top of the file — these apply to every step in it:</P>
        <FieldTable
          rows={[
            ["description", "What running this accomplishes. ciabatta list prints it."],
            ["owner", "Who to ask. Falls back to the sub-workspace's owner."],
            ["tags", "Labels for search and --filter tag:<name>. Steps inherit them."],
            ["requires", "Executables that must be on PATH, checked before the run starts."],
            ["needs", "Workflows in OTHER packages that must finish first."],
            ["background", "Workflows that must be RUNNING for this one to finish — a mock API, a database. Started first, gate nothing, stopped at the end."],
            ["REQUIRED_ENV", "Variables that must be set and non-empty, or the run is refused up front."],
            ["env_file", ".env path(s) relative to the package, sourced before the graph runs."],
            ["env", "Variables applied to every step here."],
            ["cache", "What this workflow's builds read, write and key on."],
            ["steps", "The steps. Order in the file doesn't matter — needs does."],
          ]}
        />
        <P>And inside a step:</P>
        <FieldTable
          rows={[
            ["name", "Unique; the target of needs, on_error and retry."],
            ["run / script", "An inline shell command, or a bash script path. Exactly one."],
            ["description", "One line. ciabatta list -v prints it, so nobody has to open the file."],
            ["needs", "Steps in THIS file that must succeed first. Cross-package dependencies go on the workflow's needs."],
            ["cwd", "Where the action runs, relative to the project root. Defaults to the package's own directory."],
            ["env", "Variables for this step alone, layered over the run's."],
            ["when / skip_if", "Conditions: VAR == value, VAR != value, VAR, !VAR. when must all hold; skip_if skips when any does."],
            ["timeout", "Wall-clock limit — \"90s\", \"10m\", \"1h30m\". Past it the step is killed and the rest of the graph carries on."],
            ["retries", "Extra attempts on failure, for transient errors. Default 0."],
            ["continue_on_error", "Don't fail the run: skip this step's dependents, carry on elsewhere, report at the end."],
            ["on_error", "Jump to this recovery node instead of aborting."],
            ["persistent", "A process that never exits — a dev server. Its dependents are released immediately and it OUTLIVES the run, as a watch session."],
            ["tags / owner", "Labels and ownership for this step alone."],
            ["kind", "The phase this belongs to. Cosmetic, except push and pull, which select the registry action."],
            ["cache", "Cache settings for this step alone, layered over the workflow's field by field."],
          ]}
        />
        <Alert severity="info" sx={{ my: 2, maxWidth: "78ch" }}>
          <strong>
            <C>persistent</C> or <C>background</C>?
          </strong> A <C>persistent: true</C> step is one you
          want left running — <C>ciabatta dev</C> leaves you a server to work against, and you
          stop it with <C>ciabatta watch --stop &lt;id&gt;</C>. A workflow in the{" "}
          <C>background:</C> array exists only to get this run through, and is stopped when the
          run ends.
        </Alert>

        <SubHeading>Publishing is a step too</SubHeading>
        <P>
          A step with <C>kind: push</C> moves an artifact to one of the registries declared in the
          root config. It sits on the graph and declares what it needs, so it cannot run before
          the artifact exists:
        </P>
        <Pre>{`  - name: publish
    kind: push
    registry: nexus
    artifact: dist/api.tgz
    publish_path: "team/api/{CIABATTA_COMMIT}/api.tgz"
    needs: [package]`}</Pre>

        <SubHeading>Checking it as you type</SubHeading>
        <P>
          The schemas that back all of this are served by this daemon and understood by VS Code
          and Zed — see <a href="#editors">Editors</a> for the two-minute setup.{" "}
          <C>ciabatta config reference</C> prints the same thing in a terminal.
        </P>
      </>
    ),
  },
  {
    id: "running",
    group: "Start here",
    title: "Run it, and watch it run",
    body: (
      <>
        <Pre>{`ciabatta build                       # every package that defines build, in dependency order
ciabatta build test                  # both, as ONE graph — shared dependencies run once
ciabatta build --graph               # the resolved graph; runs nothing
ciabatta dry-run build --diff        # what would rebuild, and why
ciabatta build --gui                 # watch it here
ciabatta build --tui                 # watch it in the terminal`}</Pre>
        <P>
          Any name that isn&apos;t one of ciabatta&apos;s own commands is a workflow name, which is
          why <C>ciabatta build</C> works. When a workflow&apos;s name collides with a command —{" "}
          <C>list</C>, <C>watch</C> — spell it <C>ciabatta workflow list</C>.
        </P>

        <SubHeading>Narrowing it down</SubHeading>
        <Pre>{`ciabatta test --filter tag:fast              # only steps tagged fast
ciabatta test --filter '!tag:flaky'         # everything except the flaky ones
ciabatta build --filter workspace:api       # one package's steps
ciabatta build --only api                   # start from api; its dependencies still come
ciabatta build --only api --isolated        # …and don't follow them at all`}</Pre>
        <P>
          Selectors are <C>tag:</C>, <C>workspace:</C> (alias <C>member:</C>), <C>kind:</C>,{" "}
          <C>owner:</C>, <C>step:</C>, or a bare word that searches all of them plus descriptions.
          A leading <C>!</C> excludes, and exclusions beat matches. Positive terms are OR&apos;d.
        </P>
        <Alert severity="warning" sx={{ my: 2, maxWidth: "78ch" }}>
          A filter <strong>prunes</strong> the graph rather than expanding a selection: the
          surviving steps run without the dependencies you filtered away, on the assumption those
          already happened. It is the fast debug loop, not how you build a fresh checkout.
          Ciabatta reports every dependency edge it cut, so this is never silent.
        </Alert>

        <SubHeading>Reading the Run page</SubHeading>
        <P>
          <Link to="/run">Run</Link> lists everything the daemon has, and opening one gives you
          the flowchart and the logs side by side. The graph reads left to right:
        </P>
        <Bullets
          items={[
            <>
              <strong>Each column is a wave.</strong> An arrow means &quot;comes after&quot;.
              Edges that skip columns are routed down lanes of their own, so a line crossing a
              node never means the two are connected.
            </>,
            <>
              <strong>The icon on a node is its status</strong> — not started, running, succeeded,
              failed, skipped. A dashed border is a recovery branch, which only runs if something
              fails.
            </>,
            <>
              <strong>Dashed edges are dependencies rather than order</strong>: the{" "}
              <strong>Environment</strong> and <strong>Files</strong> toggles draw the variables
              and file sets feeding each step, in columns of their own on the left.
            </>,
            <>
              <strong>Click a node to focus it.</strong> A step lights what it waits for; a
              variable or file set lights every step that touches it — &quot;who reads
              DATABASE_URL?&quot; answered by dimming everything that doesn&apos;t.
            </>,
            <>
              <strong>Execution order</strong> numbers each node with its place in the sequence
              the engine actually takes.
            </>,
          ]}
        />

        <SubHeading>Recreate, re-run, and full screen</SubHeading>
        <Bullets
          items={[
            <>
              <strong>Recreate</strong> opens the run as the commands that reproduce it: the{" "}
              <C>cd</C> into each package, the variables each step sets, and the exact command the
              engine handed to the shell — with the step in flight marked, so it doubles as a
              position report while the run is going. There is a copy button.
            </>,
            <>
              <strong>Run again</strong> starts the same run once it has finished: same workflows,
              same filters, same flags. The graph is compiled fresh, so it picks up whatever
              changed on disk. Variables you typed into the missing-variable prompt are never
              stored, so it may ask for those again.
            </>,
            <>
              <strong>Full screen</strong> on the log pane gives the output the whole window —
              Escape comes back. Wrapping and follow-the-tail are next to it.
            </>,
          ]}
        />

        <SubHeading>History, and how long it&apos;s kept</SubHeading>
        <P>
          Runs and their logs are written to <C>~/.ciabatta/runs/</C>, so they survive restarting
          the daemon, rebooting, and upgrading ciabatta. A run interrupted by the daemon going
          away comes back marked <em>stopped</em> rather than pretending to still be running.
        </P>
        <P>
          <strong>Keep run logs</strong> on the Run page sets how long a finished run is kept —
          one day, a week (the default), thirty days, or forever. Shortening it deletes what it
          just made stale, immediately. The bin icon on a run deletes that one; a run still going
          has to be stopped first.
        </P>
      </>
    ),
  },
  {
    id: "tips",
    group: "Start here",
    title: "Common tips",
    body: (
      <>
        <P>The things that bite people, in the order they usually bite:</P>
        <Bullets
          items={[
            <>
              <strong>Cache paths are relative to the workspace root</strong>, not to the file
              you&apos;re writing them in. Let <C>ciabatta cache init</C> write them, then check
              with <C>ciabatta why &lt;step&gt; --all</C>.
            </>,
            <>
              <strong>Exclude your own output.</strong> A build that writes into a directory its
              own <C>inputs</C> match will invalidate itself every single run.
            </>,
            <>
              <strong>
                An incomplete <C>inputs</C> list is worse than none.
              </strong> Prove it with{" "}
              <C>ciabatta build --authoritative</C> before you rely on the cache in CI.
            </>,
            <>
              <strong>
                A step&apos;s <C>needs</C> names steps in the same file.
              </strong> Depending on
              another package is the workflow&apos;s <C>needs:</C>, or the package&apos;s{" "}
              <C>workspace.depends_on</C>. This is the most common &quot;why doesn&apos;t it wait
              for that?&quot;.
            </>,
            <>
              <strong>Declare the variables your output depends on</strong> in <C>cache.env</C>.
              The Run page flags every variable a step reads without declaring — that list is your
              to-do.
            </>,
            <>
              <strong>
                Commit <C>.env.example</C>, never <C>.env</C>.
              </strong> Point{" "}
              <C>workspace.env_default</C> at it and a fresh checkout generates the file on the
              first run instead of failing.
            </>,
            <>
              <strong>
                Use <C>REQUIRED_ENV</C> freely.
              </strong> Failing in one second with a name beats
              failing in fifteen minutes with a stack trace.
            </>,
            <>
              <strong>
                Put a <C>timeout</C> on anything that touches a network.
              </strong> A step that
              hangs holds the whole branch; a step that times out is killed, reported, and the
              rest of the graph carries on.
            </>,
            <>
              <strong>
                <C>--filter</C> prunes, it doesn&apos;t select.
              </strong> Great for the inner
              loop, wrong for a fresh checkout.
            </>,
            <>
              <strong>
                Steps run through <C>sh -c</C>, from their own package directory
              </strong> — so{" "}
              <C>./scripts/x.sh</C> means the one in that package. Set <C>cwd</C> to move it, and
              remember each step starts in a new shell: a <C>cd</C> in one doesn&apos;t carry to
              the next.
            </>,
            <>
              <strong>Name things for the filter you&apos;ll want.</strong> <C>tags: [fast]</C> on
              the cheap tests today is <C>--filter tag:fast</C> every day after.
            </>,
            <>
              <strong>
                Write the <C>description</C> and the <C>owner</C>.
              </strong> They are what{" "}
              <C>ciabatta list -s payments</C> searches, and the difference between a workflow
              somebody can delete safely and one nobody dares touch.
            </>,
            <>
              <strong>Stuck? Ask it.</strong> <C>ciabatta why &lt;target&gt;</C> for one step,{" "}
              <C>ciabatta dry-run &lt;workflow&gt; --diff</C> for the whole run, and{" "}
              <C>ciabatta config show</C> for what it thinks your configuration says.
            </>,
          ]}
        />
      </>
    ),
  },
  {
    id: "commands",
    group: "Reference",
    title: "Command reference",
    body: (
      <>
        <P>
          What ciabatta can run. You are usually reading this page <em>because</em> the next thing
          you want to do happens in a terminal, so the reference lives here as well as in{" "}
          <C>--help</C> — which remains the exhaustive list for any one command.
        </P>
        <CommandReference />
      </>
    ),
  },
  {
    id: "editors",
    group: "Reference",
    title: "Editors",
    body: <EditorSetup />,
  },
  {
    id: "app",
    group: "Reference",
    title: "The rest of the app",
    body: (
      <>
        <P>
          One daemon, one web app. Every view is a real URL you can bookmark or paste to a
          colleague on the same machine, and the daemon owns the work rather than the terminal
          that asked for it: a watch session, a run, and a serial capture all outlive the command
          that started them and the tab that is watching them.
        </P>
        <Bullets
          items={[
            <>
              <strong>
                <Link to="/">Dashboard</Link>
              </strong>{" "}
              — what this checkout is, what is running, and what changed recently.
            </>,
            <>
              <strong>
                <Link to="/workspace">Workspace</Link>
              </strong>{" "}
              — every package, its workflows, owners and dependencies, with the graph between
              them. The visual answer to <C>ciabatta list</C>, and where a workflow nobody has run
              for a month is flagged as stale.
            </>,
            <>
              <strong>
                <Link to="/cache">Cache</Link>
              </strong>{" "}
              — what is stored, what it saved you, and for a rebuild the diff that caused it. The
              Remote tab is the shared cache, if you have one.
            </>,
            <>
              <strong>
                <Link to="/watch">Watch</Link>
              </strong>{" "}
              — <C>ciabatta watch &lt;command&gt;</C> streams any long-running command here, so
              Ctrl-C detaches instead of killing it. Persistent steps end up here too.
            </>,
            <>
              <strong>
                <Link to="/analyze">Analyze</Link>
              </strong>{" "}
              — the codebase&apos;s own dependency graph from <C>ciabatta analyze</C>, optionally
              with known vulnerabilities from the OSV database.
            </>,
            <>
              <strong>
                <Link to="/ai">AI</Link>
              </strong>{" "}
              — the assistant&apos;s live mind map of this codebase. Set it up with{" "}
              <C>ciabatta ai setup</C>; talk to it with <C>ciabatta ai</C>.
            </>,
            <>
              <strong>
                <Link to="/todo">Todo</Link>
              </strong>{" "}
              — the task list. Global rather than per-checkout, but each task remembers the
              project it belongs to.
            </>,
            <>
              <strong>Project switcher</strong> (top bar) — everything except Todo is
              per-checkout, and this decides which one. Projects register themselves the first
              time you run a ciabatta command inside them, or with <C>ciabatta register</C>.
            </>,
            <>
              <strong>Health chip</strong> — polls <C>/api/health</C> every ten seconds. Red means
              the daemon is gone; a version that differs from what you just installed means an old
              daemon is still holding the port.
            </>,
          ]}
        />
        <Pre>{`ciabatta daemon serve            # run it in the foreground
ciabatta daemon serve --port 9000
ciabatta daemon status | logs    # is it there, and what has it been doing
ciabatta daemon stop             # ask it to exit`}</Pre>
        <P>
          It binds loopback and starts on demand — any ciabatta command probes for one and
          launches it if nothing answers. See <a href="#security">Tokens and access</a> before
          changing where it binds: this API can start processes.
        </P>
      </>
    ),
  },
  {
    id: "remote-cache",
    group: "Reference",
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
    group: "Reference",
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
    id: "api",
    group: "Reference",
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
    group: "Reference",
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
];

export function DocsPage() {
  const { data: health } = useHealth();

  return (
    <Box>
      <PageHeader
        title="Docs"
        description="Ciabatta cares about everything that goes into a step — the files it reads, the variables it reads, and what had to happen first. This is how to tell it: set a project up, declare those inputs, and run the thing. Shipped in the same binary as the app, so it always matches what you're running."
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
              {/* The one place the page changes character — from a walkthrough
                  to the manual behind it — says so out loud. */}
              {section.group !== SECTIONS[index - 1]?.group && (
                <Typography variant="overline" color="text.secondary">
                  {section.group}
                </Typography>
              )}
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
            <List dense disablePadding>
              {SECTIONS.map((section, index) => (
                <Fragment key={section.id}>
                  {section.group !== SECTIONS[index - 1]?.group && (
                    <Typography
                      variant="overline"
                      color="text.secondary"
                      sx={{ display: "block", mt: index === 0 ? 0 : 1.5 }}
                    >
                      {section.group}
                    </Typography>
                  )}
                  <ListItemButton
                    component="a"
                    href={`#${section.id}`}
                    sx={{ borderRadius: 1, py: 0.25 }}
                  >
                    <ListItemText
                      primary={section.title}
                      primaryTypographyProps={{ variant: "body2" }}
                    />
                  </ListItemButton>
                </Fragment>
              ))}
            </List>
          </Box>
        </Grid>
      </Grid>
    </Box>
  );
}
