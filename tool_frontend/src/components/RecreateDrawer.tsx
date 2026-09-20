/**
 * "Recreate": the run, written out as the commands that would reproduce it.
 *
 * The flowchart says what the shape of a run is; it doesn't say what the run
 * *did*. That question — "what would I type to do this by hand?" — comes up
 * constantly and used to be answered by opening several workflow files and
 * mentally compiling them: which package is this step from, which directory
 * does its `cwd` resolve to, was that a `run:` or a `script:`, and what order do
 * the waves actually go in. The daemon already knows all four, so it can just
 * say.
 *
 * Two things this is careful about:
 *
 * * **The `cd`s are real.** A step's directory is its sub-workspace's, resolved
 *   against the *monorepo* root rather than wherever you happen to be, and that
 *   is the single most common way a hand-run command differs from what the
 *   engine ran. So the sequence starts by `cd`-ing to the root and changes
 *   directory whenever the run does — no more, so the list reads as a session
 *   somebody actually typed.
 *
 * * **It shows where the executor is.** While the run is going, the step in
 *   flight is marked and the ones behind it carry their verdicts, which turns
 *   this from a transcript into a position report: what has happened, what is
 *   happening, what is still to come.
 */

import { useMemo, useState } from "react";
import {
  Alert,
  Box,
  Button,
  Chip,
  Divider,
  Drawer,
  IconButton,
  Stack,
  Tooltip,
  Typography,
} from "@mui/material";
import CloseIcon from "@mui/icons-material/Close";
import ContentCopyIcon from "@mui/icons-material/ContentCopy";
import CheckIcon from "@mui/icons-material/Check";

import type { RunState, StepView, WorkflowView } from "../api/run";
import { executionOrder } from "./layout";
import { StatusIcon } from "./StatusIcon";
import { monoFontStack } from "../theme";

/**
 * The shell variable the root is held in.
 *
 * A monorepo root is a long absolute path, and repeating it in front of every
 * `cd` turned each one into three wrapped lines of noise around the two words
 * that mattered. This is what a person would write, and it stays paste-able.
 */
const ROOT = "$ROOT";

/** One line of the reproduction: a heading, a directory change, or a command. */
interface Line {
  kind: "heading" | "cd" | "command" | "note";
  text: string;
  /** The step this line belongs to, when it is one. */
  step?: StepView;
}

/**
 * The CLI invocation that starts this run from a terminal.
 *
 * The long `ciabatta workflow <name>` form rather than the bare `ciabatta
 * <name>` everyone actually types: they do the same thing, but the bare form is
 * only unambiguous while the workflow isn't named after one of ciabatta's own
 * commands, and a workflow called `list` or `watch` is nobody's mistake to
 * discover from a page that told them to type it.
 */
export function runCommand(state: RunState): string {
  const run = state.run;
  const parts = ["ciabatta", "workflow", ...run.workflows];
  for (const term of run.filter ?? []) parts.push("--filter", quote(term));
  for (const member of run.only ?? []) parts.push("--only", quote(member));
  if (run.isolated) parts.push("--isolated");
  if (state.dry_run) parts.push("--dry-run");
  return parts.join(" ");
}

/** Quote a term for a shell, only when it needs it. */
function quote(value: string): string {
  return /^[\w:.\-/=]+$/.test(value) ? value : `'${value.replace(/'/g, `'\\''`)}'`;
}

/**
 * The sequence one workflow ran, as lines.
 *
 * The order mirrors the engine rather than the file: background targets are
 * started before the first wave, the rest run in wave order (longest-path
 * depth, declaration order within a wave), and recovery nodes are listed apart
 * at the end because they only run if something fails.
 */
function workflowLines(workflow: WorkflowView): Line[] {
  const lines: Line[] = [];
  const edges = workflow.edges
    .filter((edge) => edge.kind === "needs")
    .map((edge) => ({ source: edge.from, target: edge.to }));

  const background = workflow.steps.filter((step) => step.background && !step.recover);
  const recovery = workflow.steps.filter((step) => step.recover);
  const main = workflow.steps.filter((step) => !step.background && !step.recover);

  const order = executionOrder(
    main.map((step) => step.name),
    edges,
  );
  const sequenced = [...main].sort(
    (a, b) => (order.get(a.name) ?? 0) - (order.get(b.name) ?? 0),
  );

  // The directory the previous command left us in, so a `cd` is only written
  // when the run actually changes directory.
  let at: string | null = null;
  const emit = (step: StepView) => {
    const dir = step.cwd ? `${ROOT}/${step.cwd}` : ROOT;
    if (dir !== at) {
      lines.push({ kind: "cd", text: `cd "${dir}"` });
      at = dir;
    }
    for (const [key, value] of Object.entries(step.env ?? {})) {
      lines.push({ kind: "note", text: `export ${key}=${quote(value)}` });
    }
    lines.push({
      kind: "command",
      text: step.shell ?? step.action ?? "# (nothing to run)",
      step,
    });
  };

  if (background.length > 0) {
    lines.push({
      kind: "heading",
      text: "started first, in the background — nothing waits for these",
    });
    background.forEach(emit);
  }

  if (sequenced.length > 0) {
    lines.push({ kind: "heading", text: "the run, in the order the engine takes it" });
    sequenced.forEach(emit);
  }

  if (recovery.length > 0) {
    lines.push({ kind: "heading", text: "only if a step fails — its on_error branch" });
    recovery.forEach((step) => {
      if (step.shell || step.action) emit(step);
      else
        lines.push({
          kind: "note",
          text: `# ${step.name}: asks which fix to run, then re-runs the step that failed`,
        });
    });
  }

  return lines;
}

/** The whole sequence as plain text, for the clipboard. */
function asScript(state: RunState): string {
  const out: string[] = [
    `# ${runCommand(state)}`,
    "",
    `ROOT=${quote(state.run.root)}`,
    `cd "${ROOT}"`,
  ];
  for (const workflow of state.workflows) {
    if (state.workflows.length > 1) out.push(`\n# ── ${workflow.name} ──`);
    for (const line of workflowLines(workflow)) {
      out.push(line.kind === "heading" ? `\n# ${line.text}` : line.text);
    }
    out.push("");
  }
  return out.join("\n");
}

export function RecreateDrawer({
  open,
  onClose,
  state,
}: {
  open: boolean;
  onClose: () => void;
  state: RunState;
}) {
  const [copied, setCopied] = useState(false);
  const script = useMemo(() => asScript(state), [state]);

  const copy = async () => {
    try {
      await navigator.clipboard.writeText(script);
      setCopied(true);
      setTimeout(() => setCopied(false), 1600);
    } catch {
      // Clipboard access can be refused; the text is on screen either way.
    }
  };

  return (
    <Drawer
      anchor="right"
      open={open}
      onClose={onClose}
      slotProps={{
        paper: {
          sx: {
            // The theme's drawer is the navigation rail, which borders on its
            // right. This one arrives from the other side.
            borderRight: "none",
            borderLeft: 1,
            borderColor: "divider",
            width: { xs: "100%", sm: 560, lg: 680 },
            // The app bar is deliberately above every drawer in this app, so a
            // drawer that started at the top of the window would have its own
            // header hidden behind it.
            pt: { xs: 7, sm: 8 },
          },
        },
      }}
    >
      <Stack direction="row" alignItems="center" spacing={1} sx={{ p: 2, pb: 1 }}>
        <Box sx={{ flexGrow: 1, minWidth: 0 }}>
          <Typography variant="h3">Recreate</Typography>
          <Typography variant="caption" color="text.secondary">
            Run #{state.run.id}, as the commands that reproduce it
          </Typography>
        </Box>
        <Tooltip title={copied ? "Copied" : "Copy the whole sequence"}>
          <Button
            size="small"
            startIcon={copied ? <CheckIcon /> : <ContentCopyIcon />}
            onClick={copy}
          >
            {copied ? "Copied" : "Copy"}
          </Button>
        </Tooltip>
        <IconButton size="small" onClick={onClose} aria-label="Close">
          <CloseIcon />
        </IconButton>
      </Stack>
      <Divider />

      <Box sx={{ p: 2, overflowY: "auto" }}>
        <Typography variant="body2" color="text.secondary" sx={{ mb: 1.5 }}>
          Every step in the order the engine takes it, from the directory it
          runs in. One command does the same thing — <code>ciabatta{" "}
          {state.run.workflows[0] ?? "build"}</code> is the short way to write
          it:
        </Typography>
        <CommandLine text={runCommand(state)} />

        {!state.done && (
          <Alert severity="info" sx={{ my: 2 }}>
            This run is still going — the highlighted step is the one in flight.
          </Alert>
        )}

        <Box sx={{ mt: 2 }}>
          <RootLine root={state.run.root} />
          {state.workflows.map((workflow) => (
            <Box key={workflow.name} sx={{ mb: 3 }}>
              {state.workflows.length > 1 && (
                <Chip size="small" label={workflow.name} sx={{ mt: 2, mb: 1 }} />
              )}
              {workflowLines(workflow).map((line, index) => (
                <LineRow key={index} line={line} />
              ))}
            </Box>
          ))}
        </Box>
      </Box>
    </Drawer>
  );
}

function RootLine({ root }: { root: string }) {
  return (
    <>
      <Typography variant="caption" color="text.secondary">
        Start from the workspace root — every step&apos;s directory is relative to
        it, not to wherever your shell happens to be.
      </Typography>
      <CommandLine text={`ROOT=${root}\ncd "$ROOT"`} />
    </>
  );
}

function CommandLine({ text }: { text: string }) {
  return (
    <Box
      component="pre"
      sx={{
        fontFamily: monoFontStack,
        fontSize: 12.5,
        m: 0,
        my: 0.5,
        p: 1,
        borderRadius: 1,
        border: 1,
        borderColor: "divider",
        bgcolor: "action.hover",
        whiteSpace: "pre-wrap",
        overflowWrap: "anywhere",
      }}
    >
      {text}
    </Box>
  );
}

function LineRow({ line }: { line: Line }) {
  if (line.kind === "heading") {
    return (
      <Typography
        variant="overline"
        color="text.secondary"
        sx={{ display: "block", mt: 2, mb: 0.5 }}
      >
        {line.text}
      </Typography>
    );
  }

  const running = line.step?.status === "running";

  return (
    <Stack
      direction="row"
      spacing={1}
      alignItems="flex-start"
      sx={{
        py: 0.4,
        px: 0.75,
        borderRadius: 1,
        // The step in flight is the one thing on this list worth finding at a
        // glance, so it is the only thing given a background.
        bgcolor: running ? "action.selected" : undefined,
        borderLeft: running ? 2 : 0,
        borderColor: "warning.main",
      }}
    >
      <Box sx={{ width: 18, flexShrink: 0, pt: 0.25 }}>
        {line.step ? <StatusIcon status={line.step.status} /> : null}
      </Box>
      <Box
        sx={{
          fontFamily: monoFontStack,
          fontSize: 12.5,
          minWidth: 0,
          overflowWrap: "anywhere",
          whiteSpace: "pre-wrap",
          color:
            line.kind === "command"
              ? "text.primary"
              : line.kind === "cd"
                ? "info.main"
                : "text.secondary",
          fontWeight: line.kind === "command" ? 500 : 400,
        }}
      >
        {line.text}
        {line.step && (
          <Box component="span" sx={{ color: "text.secondary", fontWeight: 400 }}>
            {"  # "}
            {line.step.name}
          </Box>
        )}
      </Box>
    </Stack>
  );
}
