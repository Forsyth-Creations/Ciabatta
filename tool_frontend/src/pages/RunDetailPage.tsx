/**
 * A live run: the step flowchart, per-step logs, and the fix-it prompt
 * when a recovery node is waiting on a decision.
 *
 * State arrives over SSE. The flowchart is react-flow with a layered layout —
 * an edge means "comes after", so depth is the meaningful axis.
 */

import { useEffect, useMemo, useState } from "react";
import {
  Alert,
  Box,
  Button,
  Chip,
  Dialog,
  DialogActions,
  DialogContent,
  DialogTitle,
  FormControlLabel,
  Stack,
  Switch,
  Tab,
  Tabs,
  Tooltip,
  Typography,
} from "@mui/material";
import ArrowBackIcon from "@mui/icons-material/ArrowBack";
import ManageSearchIcon from "@mui/icons-material/ManageSearch";
import StopIcon from "@mui/icons-material/Stop";
import ReplayIcon from "@mui/icons-material/Replay";
import TerminalIcon from "@mui/icons-material/Terminal";
import { IconButton } from "@mui/material";
import { useTheme } from "@mui/material/styles";
import type { Theme } from "@mui/material/styles";
import { Link, useNavigate, useParams } from "@tanstack/react-router";
import { type Edge, type Node } from "@xyflow/react";

import { streamUrl } from "../api/client";
import {
  clockTime,
  elapsedOf,
  missingEnvFrom,
  useChoose,
  useNow,
  useRerunRun,
  useStopRun,
  undeclaredEnv,
  type WorkflowView,
  type RunState,
  type StepStatus,
  type StepView,
  type TargetDeps,
  type Timed,
} from "../api/run";
import { humanizeBytes } from "../api/cache";
import type { EnvVar } from "../api/types";
import { LogView } from "../components/LogView";
import { GraphCanvas } from "../components/GraphCanvas";
import { RecreateDrawer } from "../components/RecreateDrawer";
import { EnvPrompt } from "../components/EnvPrompt";
import {
  EnvPanel,
  EnvVarChip,
  StepEnvChips,
  envValueText,
  unsetProblem,
} from "../components/EnvVars";
import {
  ROUTED_EDGE,
  executionOrder,
  layeredLayout,
  routeKey,
  type LayoutEdge,
} from "../components/layout";
import { StatusIcon, statusColour, statusLabel } from "../components/StatusIcon";
import {
  CacheIcon,
  CacheReportView,
  Prose,
  cacheLabel,
  cacheTone,
  wasReused,
} from "../components/CacheReport";
import { useInspectMode } from "../state/inspect";
import { ErrorNote, Loading } from "../components/Page";
import { monoFontStack } from "../theme";

/** The node id a variable takes on the flowchart. Namespaced so a variable
 *  called the same thing as a step can't collide with it. */
const envNodeId = (key: string) => `env::${key}`;

/** The variable a node id names, or null when the node is a step. */
const envKeyOf = (id: string): string | null =>
  id.startsWith("env::") ? id.slice("env::".length) : null;

/** Node ids for the file sets a target reads and writes, namespaced the same
 *  way so a step named `in` can't collide with one. */
const inputNodeId = (key: string) => `in::${key}`;
const outputNodeId = (step: string) => `out::${step}`;

/** Whether a node id names a file set rather than a step. */
const isFileNode = (id: string) => id.startsWith("in::") || id.startsWith("out::");

/** The step a `writes` node hangs off, or null when the id isn't one. */
const outputStepOf = (id: string): string | null =>
  id.startsWith("out::") ? id.slice("out::".length) : null;

/**
 * Whether a node is a *dependency* — a variable or a file set — rather than a
 * step.
 *
 * These are the nodes with something to say and nowhere to go: they have no
 * logs of their own, so clicking one focuses the graph on what it touches
 * instead of opening it.
 */
const isDependencyNode = (id: string) => isFileNode(id) || envKeyOf(id) !== null;

/** How far the graph fades what the focused node doesn't reach. */
const DIMMED = 0.18;

/**
 * How wide a step node is drawn.
 *
 * Fixed rather than fitted, because the layout has to know: it places columns a
 * set distance apart, and a node that grows past that distance reaches into the
 * lane the next column's edges arrive through — which is how a line ends up
 * drawn across the middle of a node.
 */
const NODE_WIDTH = 210;

/**
 * The lines a step node is built from, each a fixed height.
 *
 * Fixed because the layout has to know each node's height before react-flow
 * has drawn it: handles hang off the middle of a node, and a wire routed to
 * where the layout *thought* the middle was arrives a few pixels off it and
 * kinks. And fixed per node rather than measured, so a node doesn't grow when
 * its step finishes and gains a time — which shoved the rest of its column
 * down mid-run.
 */
const LINE = { name: 16, small: 14 } as const;
/** A step node's border and padding, top and bottom together. */
const STEP_CHROME = 2 * 2 + 6 * 2;
/** A variable or file-set node: two lines, and its chrome. */
const DEPENDENCY_HEIGHT = 46;

/** How tall a node is drawn — what `layeredLayout` centres its wires on. */
function nodeHeight(id: string, byName: Map<string, StepView>, inspect: boolean): number {
  const step = byName.get(id);
  if (!step) return DEPENDENCY_HEIGHT;
  return (
    STEP_CHROME +
    LINE.name +
    // The timing line is always there, so a step's node is the same height
    // before it runs as after.
    LINE.small +
    (step.workspace ? LINE.small : 0) +
    (step.background ? LINE.small : 0) +
    // Inspect mode's cache line, there for every step whether or not the
    // cache has spoken yet — for the same reason the timing line is.
    (inspect && !step.recover ? LINE.small : 0)
  );
}

/** Height of the graph and log panes, which are the same so they line up when
 *  they sit side by side. */
const PANE_HEIGHT = 460;

/**
 * What one click on a dependency node lights up.
 *
 * "Who reads DATABASE_URL?", "which steps rebuild when these sources change?",
 * "what produced this artifact?" — three questions with the same shape, and on
 * a wide graph the edges alone don't answer any of them. Focusing dims
 * everything the node doesn't reach, which leaves the answer as the only thing
 * still lit.
 */
interface Focus {
  /** The focused node's id, or null when the whole graph is lit. */
  id: string | null;
  /** Whether a node stays lit: the focused node itself, and what it reaches. */
  lit: (id: string) => boolean;
}

/** No focus: every node is lit, which is the graph's resting state. */
const NO_FOCUS: Focus = { id: null, lit: () => true };

/**
 * "started 14:03:20 · finished 14:03:22" — for a tooltip on something whose
 * label already says how long it took. Null when it never started.
 */
function timeline(timed: Timed): string | null {
  if (!timed.started_at) return null;
  const started = `started ${clockTime(timed.started_at)}`;
  return timed.finished_at ? `${started} · finished ${clockTime(timed.finished_at)}` : started;
}

/** The header's " · started 14:03:20 · took 2m05s", ticking while it runs. */
function RunClock({ timing, live }: { timing: Timed; live: boolean }) {
  const now = useNow(live);
  const took = elapsedOf(timing, now);
  return (
    <>
      {` · started ${clockTime(timing.started_at!)}`}
      {took && (timing.finished_at ? ` · took ${took}` : ` · running ${took}`)}
      {timing.finished_at && ` · finished ${clockTime(timing.finished_at)}`}
    </>
  );
}

export function RunDetailPage() {
  const { runId } = useParams({ from: "/run/$runId" });
  const id = Number(runId);
  const navigate = useNavigate();

  const { state, error } = useRunStream(id);
  const [workflowIndex, setWorkflowIndex] = useState(0);
  const [selectedStep, setSelectedStep] = useState<string | null>(null);
  const [recreating, setRecreating] = useState(false);
  const rerun = useRerunRun();
  const [needsEnv, setNeedsEnv] = useState<string[] | null>(null);

  if (error) return <ErrorNote error={new Error(error)} />;
  if (!state) return <Loading label="Connecting to the run…" />;

  const workflow = state.workflows[workflowIndex];
  const status = state.run.status ?? (state.done ? "success" : "running");
  const timing = state.run.started_at ? state.run : null;

  const again = (env?: Record<string, string>) =>
    rerun.mutate(
      { id, env },
      {
        onSuccess: (next) => {
          setNeedsEnv(null);
          navigate({ to: "/run/$runId", params: { runId: String(next.id) } });
        },
        // The variables somebody typed to start this run were deliberately not
        // written to its record, so a re-run may have to ask for them again.
        onError: (error) => setNeedsEnv(missingEnvFrom(error)),
      },
    );

  return (
    <>
      <Stack
        direction="row"
        alignItems="center"
        spacing={1.5}
        sx={{ mb: 2 }}
        flexWrap="wrap"
        useFlexGap
      >
        <IconButton component={Link} to="/run" size="small" aria-label="Back to runs">
          <ArrowBackIcon />
        </IconButton>
        <Box sx={{ flexGrow: 1, minWidth: 0 }}>
          <Typography variant="h1">Run #{id}</Typography>
          <Typography variant="caption" color="text.secondary">
            {state.run.workflows.join(", ")}
            {state.dry_run && " · dry run"}
            {timing && <RunClock timing={timing} live={!state.done} />}
            {(state.run.filter ?? []).length > 0 &&
              ` · filtered: ${state.run.filter.join(" ")}`}
          </Typography>
        </Box>
        <Chip
          size="small"
          variant="outlined"
          icon={<StatusIcon status={status} title={null} />}
          label={statusLabel(status)}
        />
        <InspectToggle />
        <Tooltip title="Show the exact commands this run executed, in order, with the directory each one runs from — and where it has got to.">
          <Button size="small" startIcon={<TerminalIcon />} onClick={() => setRecreating(true)}>
            Recreate
          </Button>
        </Tooltip>
        <Tooltip
          title={
            state.done
              ? "Start this run again, with the same workflows, filters and flags. The graph is compiled fresh, so it picks up whatever has changed on disk."
              : "Wait for this run to finish, or stop it, before starting it again."
          }
        >
          {/* A disabled button swallows hover, so the tooltip needs a live
              wrapper to hang off. */}
          <Box component="span">
            <Button
              size="small"
              startIcon={<ReplayIcon />}
              disabled={!state.done || rerun.isPending}
              onClick={() => again()}
            >
              Run again
            </Button>
          </Box>
        </Tooltip>
        {!state.done && <StopRunButton id={id} />}
      </Stack>

      {rerun.error && !needsEnv && <ErrorNote error={rerun.error} />}

      {needsEnv && (
        <EnvPrompt
          key={needsEnv.join(",")}
          variables={needsEnv}
          initial={{}}
          pending={rerun.isPending}
          onCancel={() => setNeedsEnv(null)}
          onSubmit={(values) => again(values)}
        />
      )}

      <RecreateDrawer open={recreating} onClose={() => setRecreating(false)} state={state} />

      {state.workflows.length > 1 && (
        <Tabs
          value={workflowIndex}
          onChange={(_, next) => {
            setWorkflowIndex(next);
            setSelectedStep(null);
          }}
          sx={{ mb: 2 }}
        >
          {state.workflows.map((r) => (
            <Tab key={r.name} label={r.name} />
          ))}
        </Tabs>
      )}

      {workflow && (
        <WorkflowPanel
          runId={id}
          workflow={workflow}
          selectedStep={selectedStep}
          onSelectStep={setSelectedStep}
        />
      )}
    </>
  );
}

function WorkflowPanel({
  runId,
  workflow,
  selectedStep,
  onSelectStep,
}: {
  runId: number;
  workflow: WorkflowView;
  selectedStep: string | null;
  onSelectStep: (name: string | null) => void;
}) {
  const theme = useTheme();
  const choose = useChoose(runId);
  const { on: inspect } = useInspectMode();
  const [showOrder, setShowOrder] = useState(false);
  // Variables are dependencies, so the graph draws them like every other
  // dependency. The toggle is for the graphs where they'd crowd out the steps.
  const [showEnv, setShowEnv] = useState(true);
  // Files are the other two dependencies — what a target reads, and what it
  // writes. Off by default: on a monorepo graph they double the node count, and
  // unlike variables they're only what you want when the question is caching.
  const [showFiles, setShowFiles] = useState(false);
  // The dependency node the graph is focused on — a variable, a set of inputs,
  // or a step's outputs. Held as a node id so all three focus the same way.
  const [focused, setFocused] = useState<string | null>(null);

  const { nodes, edges } = useMemo(
    () => buildFlow(workflow, theme, showOrder, showEnv, showFiles, focused, inspect),
    [workflow, theme, showOrder, showEnv, showFiles, focused, inspect],
  );
  const step = workflow.steps.find((s) => s.name === selectedStep);
  // Ticks only while the workflow is running, so a running phase or step
  // shows its time so far; a finished one's times are fixed.
  const now = useNow(workflow.status === "running");

  // Clicking a node focuses the graph on it. For a step that also opens its
  // logs and details; for a dependency — a variable, or a set of files read or
  // written — it lights the steps that touch it. Clicking the same node again
  // puts the whole graph back.
  const clickNode = (id: string) => {
    const again = focused === id;
    setFocused(again ? null : id);
    onSelectStep(again || isDependencyNode(id) ? null : id);
  };
  const clearSelection = () => {
    setFocused(null);
    onSelectStep(null);
  };

  return (
    <>
      {/* The run's phases. Each chip's text changes as the run goes, but the
          strip itself is always there and always one row. */}
      <Stack direction="row" spacing={1} sx={{ mb: 1.5 }} flexWrap="wrap" useFlexGap>
        {workflow.stages.map((stage) => {
          // A phase that fell through to its default did nothing, and "0.0s"
          // next to it would only suggest it did something quickly.
          const took = stage.status === "skipped" ? null : elapsedOf(stage, now);
          return (
            <Tooltip key={stage.name} title={timeline(stage) ?? "Not reached"}>
              <Chip
                size="small"
                variant="outlined"
                color={stageColor(stage.status)}
                label={`${stage.name}: ${stage.status}${took ? ` · ${took}` : ""}`}
              />
            </Tooltip>
          );
        })}
      </Stack>

      {workflow.error && (
        <Alert severity="error" sx={{ mb: 2 }}>
          {workflow.error}
        </Alert>
      )}

      {workflow.pending && (
        <Alert severity="warning" sx={{ mb: 2 }}>
          <Typography sx={{ mb: 1 }}>{workflow.pending.message}</Typography>
          <Stack direction="row" spacing={1} flexWrap="wrap" useFlexGap>
            {workflow.pending.options.map((option, index) => (
              <Button
                key={option}
                size="small"
                variant="contained"
                disabled={choose.isPending}
                onClick={() =>
                  choose.mutate({
                    workflow: workflow.name,
                    step: workflow.pending!.step,
                    option: index,
                  })
                }
              >
                {option}
              </Button>
            ))}
          </Stack>
        </Alert>
      )}

      {choose.error && <ErrorNote error={choose.error} />}

      {inspect && (
        <InspectSummary
          workflow={workflow}
          onSelect={(name) => {
            setFocused(name);
            onSelectStep(name);
          }}
        />
      )}

      {/*
        Graph and logs side by side once there is width for both, stacked below
        that. Watching a step run means watching two things — which node is
        lit, and what it is printing — and stacked panes put one of them off
        the bottom of the screen at exactly the moment both matter. The
        breakpoint is `lg` because the graph needs real width before splitting
        it helps: narrower than that, a half-width flowchart is worse than a
        full-width one above the logs.

        Both panes are fixed: a header of fixed height, then a body of fixed
        height. Selecting a step changes what they show, never where they are —
        what a click adds goes in the inspector underneath, so the log you were
        reading doesn't jump down the page to make room for it.
      */}
      <Box
        sx={{
          display: "grid",
          columnGap: 2,
          rowGap: 1,
          gridTemplateColumns: { xs: "1fr", lg: "minmax(0, 1fr) minmax(0, 1fr)" },
        }}
      >
        <Box sx={{ minWidth: 0 }}>
          <PaneHeader title="Graph">
            <GraphToggle
              label="Environment"
              hint="Draw each environment variable as what it is — a dependency, feeding into every step that reads it. Values come from the run's resolved environment."
              checked={showEnv}
              onChange={(checked) => {
                setShowEnv(checked);
                // Focusing a node and then hiding it would dim the graph with
                // nothing left lit to explain why.
                if (!checked && focused !== null && envKeyOf(focused) !== null) setFocused(null);
              }}
            />
            <GraphToggle
              label="Files"
              hint="Draw the files each target reads and writes as nodes of their own: inputs feeding in from the left, outputs produced on the right. These are the file sets the cache keys on, so this is the graph the caching decision is actually made from."
              checked={showFiles}
              onChange={(checked) => {
                setShowFiles(checked);
                if (!checked && focused !== null && isFileNode(focused)) setFocused(null);
              }}
            />
            <GraphToggle
              label="Order"
              hint="Number each node with its place in the run's sequence. Recovery steps aren't numbered — they only run if something fails."
              checked={showOrder}
              onChange={setShowOrder}
            />
          </PaneHeader>
          <GraphCanvas
            nodes={nodes}
            edges={edges}
            height={PANE_HEIGHT}
            // Turning the environment column on and off changes the graph's
            // extent, so the view has to be re-fitted around it.
            fitKey={
              `${showEnv ? "env" : ""}${showFiles ? "+files" : ""}${inspect ? "+inspect" : ""}` ||
              "steps-only"
            }
            onNodeClick={(_, node) => clickNode(node.id)}
            onPaneClick={() => setFocused(null)}
            nodeColor={(node) =>
              inspect && byNameHas(workflow, node.id)
                ? cacheTone(workflow.steps.find((s) => s.name === node.id)?.cache, theme)
                : statusColor(node.data?.status as StepStatus, theme)
            }
          />
        </Box>

        <Box sx={{ minWidth: 0 }}>
          <PaneHeader title={step ? `${step.name} logs` : "Workflow logs"}>
            {step && (
              <Button size="small" onClick={clearSelection}>
                All logs
              </Button>
            )}
          </PaneHeader>
          <LogView
            // Keyed by what is being shown, so switching steps starts the new
            // log at its end rather than inheriting the old one's scroll.
            key={step ? step.name : "__workflow__"}
            lines={step ? step.logs : workflow.logs}
            dropped={(step ? step.dropped_logs : workflow.dropped_logs) ?? 0}
            height={PANE_HEIGHT}
            title={step ? `${step.name} — run #${runId}` : `${workflow.name} — run #${runId}`}
          />
        </Box>
      </Box>

      <Inspector
        workflow={workflow}
        step={step ?? null}
        focused={focused}
        now={now}
        inspect={inspect}
        onClear={clearSelection}
      />

      <Box sx={{ mt: 2 }}>
        <EnvPanel report={workflow.env} title="Environment this run started with" />
      </Box>
    </>
  );
}

/** How tall a pane's header is — the same for both, so the panes line up. */
const PANE_HEADER = 36;

/**
 * A pane's title row: one line, a fixed height, controls on the right.
 *
 * Fixed so the two panes stay level whatever their headers say. The log pane's
 * title changes with every step selected; if its header grew or shrank, the
 * log underneath would move with it.
 */
function PaneHeader({ title, children }: { title: string; children?: React.ReactNode }) {
  return (
    <Stack
      direction="row"
      alignItems="center"
      spacing={1.5}
      sx={{ height: PANE_HEADER, mb: 0.5, minWidth: 0 }}
    >
      <Typography variant="h3" noWrap sx={{ flexGrow: 1, minWidth: 0 }} title={title}>
        {title}
      </Typography>
      {children}
    </Stack>
  );
}

/** A switch in the graph's header. */
function GraphToggle({
  label,
  hint,
  checked,
  onChange,
}: {
  label: string;
  hint: string;
  checked: boolean;
  onChange: (checked: boolean) => void;
}) {
  return (
    <Tooltip title={hint}>
      <FormControlLabel
        control={
          <Switch size="small" checked={checked} onChange={(_, value) => onChange(value)} />
        }
        label={
          <Typography variant="caption" color="text.secondary" noWrap>
            {label}
          </Typography>
        }
        sx={{ mr: 0, flexShrink: 0 }}
      />
    </Tooltip>
  );
}

/**
 * Everything about what was clicked, under the graph and the logs.
 *
 * This used to be spread across three places: a step's details pushed in
 * above its logs, a "what it waits for" note pushed in under the graph, and a
 * "show all logs" button pushed in under the logs. Each click moved the panes
 * a different distance, so the line you were reading jumped. Now the panes
 * stay put and this one place fills in — always present, so even the first
 * click only changes what it says.
 */
function Inspector({
  workflow,
  step,
  focused,
  now,
  inspect,
  onClear,
}: {
  workflow: WorkflowView;
  step: StepView | null;
  focused: string | null;
  now: number;
  inspect: boolean;
  onClear: () => void;
}) {
  const body = step ? (
    <StepInspector workflow={workflow} step={step} now={now} inspect={inspect} onClear={onClear} />
  ) : focused !== null && isDependencyNode(focused) ? (
    <FocusNote workflow={workflow} focused={focused} onClear={onClear} />
  ) : null;

  return (
    <Box
      sx={{
        mt: 1.5,
        px: 1.5,
        py: 1,
        minHeight: 48,
        border: 1,
        borderColor: "divider",
        borderRadius: 1,
        display: "flex",
        flexDirection: "column",
        justifyContent: "center",
      }}
    >
      {body ?? (
        <Typography variant="caption" color="text.secondary">
          Click a step to see its logs, what it waits for and why it ran. Click a
          variable or a file set to light up the steps that read it.
          {inspect && " Cache inspect mode is on: each step says what the cache made of it."}
        </Typography>
      )}
    </Box>
  );
}

/** A selected step: what it is, what it waits for, and its command. */
function StepInspector({
  workflow,
  step,
  now,
  inspect,
  onClear,
}: {
  workflow: WorkflowView;
  step: StepView;
  now: number;
  inspect: boolean;
  onClear: () => void;
}) {
  const upstream = [...dependencyClosure(workflow, step.name)].filter(
    (id) => id !== step.name && !isDependencyNode(id),
  );
  return (
    <Stack spacing={0.75}>
      <Stack direction="row" spacing={1} alignItems="center" sx={{ minWidth: 0 }}>
        <StatusIcon status={step.status} />
        <Typography
          variant="body2"
          sx={{ fontFamily: monoFontStack, fontWeight: 600, minWidth: 0 }}
          noWrap
          title={step.name}
        >
          {step.name}
        </Typography>
        <Typography variant="caption" color="text.secondary" noWrap sx={{ flexGrow: 1, minWidth: 0 }}>
          {upstream.length === 0
            ? "depends on nothing — it can start immediately"
            : `waits for ${upstream.length} step${upstream.length === 1 ? "" : "s"}: ${upstream.join(", ")}`}
        </Typography>
        <Button size="small" onClick={onClear} sx={{ flexShrink: 0 }}>
          Clear
        </Button>
      </Stack>
      {/* The commands are listed under "runs" when the step is a target; only
          a step without one (a recovery branch) needs its action said here. */}
      {step.action && (step.deps?.commands.length ?? 0) === 0 && (
        <Typography
          variant="caption"
          color="text.secondary"
          sx={{ display: "block", fontFamily: monoFontStack, wordBreak: "break-all" }}
        >
          $ {step.action}
        </Typography>
      )}
      <StepDetails
        step={step}
        now={now}
        env={workflow.env.vars.filter((variable) => variable.steps.includes(step.name))}
      />
      <StepCacheSection step={step} inspect={inspect} />
    </Stack>
  );
}

/**
 * The cache's side of a selected step: compact normally, everything in inspect
 * mode — the key, the fingerprints of what it keyed on upstream, every file
 * that moved.
 */
function StepCacheSection({ step, inspect }: { step: StepView; inspect: boolean }) {
  if (step.recover) return null;
  if (!step.cache) {
    if (!inspect) return null;
    return (
      <Alert severity="info" icon={<ManageSearchIcon fontSize="small" />} sx={{ py: 0.25 }}>
        <Typography variant="caption">
          The cache wasn't consulted for this step
          {step.status === "pending"
            ? " yet — it hasn't been reached."
            : step.status === "skipped"
              ? " — a condition skipped it, or the run stopped before it."
              : ". Caching is off for this run: it was a dry run, started with --authoritative, or the project has no cache configured."}
        </Typography>
      </Alert>
    );
  }
  return (
    <Box
      sx={{
        mt: 0.5,
        p: 1.25,
        border: 1,
        borderColor: inspect ? "warning.main" : "divider",
        borderRadius: 1,
      }}
    >
      <Typography variant="overline" color="text.secondary" sx={{ lineHeight: 1.5 }}>
        Cache
      </Typography>
      <CacheReportView report={step.cache} detailed={inspect} />
    </Box>
  );
}

/**
 * Inspect mode's overview: how the cache did across the whole run, and the
 * steps that could have been reused but weren't, each with what to change.
 *
 * Ordered by what's worth fixing first. A step held back by an upstream is the
 * most expensive kind of miss — it repeats on every run and drags everything
 * behind it along — so those lead.
 */
function InspectSummary({
  workflow,
  onSelect,
}: {
  workflow: WorkflowView;
  onSelect: (name: string) => void;
}) {
  const steps = workflow.steps.filter((s) => !s.recover);
  const reused = steps.filter((s) => wasReused(s.cache));
  const ran = steps.filter((s) => s.cache?.outcome === "rebuild");
  const uncached = steps.filter((s) => s.cache?.outcome === "uncached");
  const silent = steps.filter((s) => !s.cache);

  // Everything holding something else back, with who it holds back.
  const blockers = new Map<string, string[]>();
  for (const s of steps) {
    for (const upstream of s.cache?.blocked_by ?? []) {
      blockers.set(upstream, [...(blockers.get(upstream) ?? []), s.name]);
    }
  }

  // The rest of what ran with something to say. Blocked steps and their
  // blockers are left out: the alert above already says it, once, per blocker.
  const attention = ran.filter(
    (s) =>
      (s.cache?.hints.length ?? 0) > 0 &&
      (s.cache?.blocked_by.length ?? 0) === 0 &&
      !blockers.has(s.name),
  );

  return (
    <Box
      sx={{
        mb: 1.5,
        p: 1.5,
        border: 1,
        borderColor: "warning.main",
        borderRadius: 1,
        bgcolor: (t) => `${t.palette.warning.main}0d`,
      }}
    >
      <Stack direction="row" spacing={1} alignItems="center" flexWrap="wrap" useFlexGap sx={{ mb: 1 }}>
        <ManageSearchIcon fontSize="small" sx={{ color: "warning.main" }} />
        <Typography variant="subtitle2" sx={{ mr: 1 }}>
          Cache inspect
        </Typography>
        <Chip size="small" color="success" variant="outlined" label={`${reused.length} reused`} />
        <Chip size="small" color="warning" variant="outlined" label={`${ran.length} ran`} />
        {uncached.length > 0 && (
          <Chip size="small" variant="outlined" label={`${uncached.length} not cached`} />
        )}
        {silent.length > 0 && (
          <Tooltip title="Steps the cache never looked at: not reached yet, skipped by a condition, or caching was off for this run.">
            <Chip size="small" variant="outlined" label={`${silent.length} not consulted`} />
          </Tooltip>
        )}
      </Stack>

      {steps.length > 0 && silent.length === steps.length && (
        <Typography variant="caption" color="text.secondary" sx={{ display: "block" }}>
          The cache wasn't consulted for any step in this run. Dry runs and{" "}
          <code>--authoritative</code> runs bypass it, and a project needs{" "}
          <code>cache.enabled</code> with <code>cache.inputs</code> before it's used at all.
        </Typography>
      )}

      {blockers.size > 0 && (
        <Alert severity="error" icon={false} sx={{ py: 0.25, mb: 1 }}>
          <Typography variant="caption" sx={{ display: "block", fontWeight: 600 }}>
            Steps holding others back from their cache
          </Typography>
          {[...blockers].map(([upstream, held]) => (
            <Typography key={upstream} variant="caption" sx={{ display: "block" }}>
              <Box
                component="button"
                type="button"
                onClick={() => onSelect(upstream)}
                sx={linkButton}
              >
                {upstream}
              </Box>{" "}
              declares no <code>cache.outputs</code>, so {held.length} step
              {held.length === 1 ? "" : "s"} after it ({held.join(", ")}) rebuild every run. Give it{" "}
              <code>cache.outputs</code>, or <code>cache.no_outputs: true</code> if it writes nothing
              they read.
            </Typography>
          ))}
        </Alert>
      )}

      {attention.length > 0 && (
        <Stack spacing={0.5}>
          {attention.slice(0, 8).map((s) => (
            <Stack key={s.name} direction="row" spacing={1} alignItems="baseline">
              <Box component="button" type="button" onClick={() => onSelect(s.name)} sx={linkButton}>
                {s.name}
              </Box>
              <Typography variant="caption" color="text.secondary" sx={{ minWidth: 0 }}>
                {cacheLabel(s.cache)} — <Prose text={s.cache!.hints[0]} />
              </Typography>
            </Stack>
          ))}
          {attention.length > 8 && (
            <Typography variant="caption" color="text.secondary">
              … and {attention.length - 8} more. Click a node for its details.
            </Typography>
          )}
        </Stack>
      )}
    </Box>
  );
}

/** A step name you can click, styled as a link rather than a button. */
const linkButton = {
  p: 0,
  border: 0,
  background: "none",
  cursor: "pointer",
  color: "primary.main",
  fontFamily: monoFontStack,
  fontSize: 12,
  fontWeight: 600,
  flexShrink: 0,
  "&:hover": { textDecoration: "underline" },
} as const;

function byNameHas(workflow: WorkflowView, id: string): boolean {
  return workflow.steps.some((s) => s.name === id);
}

/**
 * A workflow-graph node's label: a status glyph, the sub-workspace it came
 * from, and the step's own name.
 *
 * The status used to be carried by the node's border colour alone, which asked
 * the reader to hold a legend in their head and gave a red/green pair to people
 * who can't tell them apart. The glyph says which of the five states this is on
 * its own; the border still carries the same colour, so the graph still reads
 * at a glance from across the room.
 *
 * `order` is the step's place in the run sequence, shown as a leading badge when
 * the order toggle is on. Null both when the toggle is off and for the recovery
 * steps that have no place in the sequence.
 */
function NodeLabel({
  step,
  order,
  inspect,
}: {
  step: StepView;
  order: number | null;
  inspect: boolean;
}) {
  // The id is "<workspace>:<step>" (or "<workspace>:<workflow>:<step>"), and
  // repeating the workspace in both lines just wastes the node's width.
  const short =
    step.workspace && step.name.startsWith(`${step.workspace}:`)
      ? step.name.slice(step.workspace.length + 1)
      : step.name;

  return (
    <Stack direction="row" spacing={0.75} alignItems="center" sx={{ minWidth: 0, width: "100%" }}>
      {order !== null && (
        <Box
          sx={{
            flexShrink: 0,
            minWidth: 18,
            height: 18,
            px: 0.5,
            borderRadius: 9,
            display: "flex",
            alignItems: "center",
            justifyContent: "center",
            bgcolor: "action.selected",
            color: "text.secondary",
            fontFamily: monoFontStack,
            fontSize: 10,
            fontWeight: 700,
            lineHeight: 1,
          }}
        >
          {order}
        </Box>
      )}
      {/* No tooltip: the node has its own hover, and two would fight. */}
      <StatusIcon status={step.status} title={null} />
      {/* Every line is one line high, cut off rather than wrapped: the node's
          height is fixed (see `nodeHeight`), and the full name is in the
          title and the step panel. */}
      <Box sx={{ minWidth: 0, flexGrow: 1, textAlign: "left" }} title={step.name}>
        {step.workspace && (
          <Box sx={{ ...oneLine(LINE.small), fontSize: 10, opacity: 0.75, fontFamily: monoFontStack }}>
            {step.workspace}
          </Box>
        )}
        <Box sx={{ ...oneLine(LINE.name), fontWeight: 600 }}>{short}</Box>
        <Box sx={{ ...oneLine(LINE.small), fontSize: 10, opacity: 0.7, fontFamily: monoFontStack }}>
          {/* A step served from the cache is "skipped" to the engine, but
              that word belongs to steps a condition left out. */}
          {wasReused(step.cache)
            ? `from cache${step.finished_at ? ` · ${clockTime(step.finished_at)}` : ""}`
            : step.started_at && step.finished_at
              ? `${elapsedOf(step, 0)} · ${clockTime(step.finished_at)}`
              : statusLabel(step.status).toLowerCase()}
        </Box>
        {step.background && (
          <Box sx={{ ...oneLine(LINE.small), fontSize: 10, opacity: 0.7 }}>
            background · nothing waits for it
          </Box>
        )}
        {inspect && !step.recover && <InspectLine step={step} />}
      </Box>
      {/* The node was served from the cache rather than run: say so where
          the eye already is, and let a click on it explain. */}
      {step.cache && wasReused(step.cache) && <CacheIcon report={step.cache} />}
    </Stack>
  );
}

/** Inspect mode's line on a node: what the cache made of the step. */
function InspectLine({ step }: { step: StepView }) {
  const theme = useTheme();
  return (
    <Box
      sx={{
        ...oneLine(LINE.small),
        fontSize: 10,
        fontWeight: 600,
        color: cacheTone(step.cache, theme),
      }}
    >
      {cacheLabel(step.cache)}
    </Box>
  );
}

/** One line of text at exactly `height`, ending in an ellipsis if it's long. */
function oneLine(height: number) {
  return {
    height,
    lineHeight: `${height}px`,
    overflow: "hidden",
    whiteSpace: "nowrap",
    textOverflow: "ellipsis",
  } as const;
}

/**
 * Cache inspect mode's switch, in the run's own header: it changes what this
 * page's graph says, so it lives on this page. Remembered between runs — the
 * next run is usually the one that checks whether the fix worked.
 */
function InspectToggle() {
  const inspect = useInspectMode();
  return (
    <Tooltip
      title={
        inspect.on
          ? "Cache inspect mode is on — every step says what the cache decided and why. Click to turn it off."
          : "Cache inspect mode: show what the cache decided for every step, and why a step that should have been reused wasn't."
      }
    >
      <Button
        size="small"
        startIcon={<ManageSearchIcon />}
        onClick={inspect.toggle}
        aria-pressed={inspect.on}
        color={inspect.on ? "warning" : "primary"}
        variant={inspect.on ? "outlined" : "text"}
      >
        Inspect cache
      </Button>
    </Tooltip>
  );
}

/**
 * Stop a run in flight.
 *
 * Confirmed rather than immediate: a run is side-effecting shell work, and
 * stopping one halfway can leave a migration applied and the deploy that
 * follows it not — which is a worse position than either finishing or never
 * starting. The dialog says what will happen rather than asking "are you sure",
 * because "are you sure" tells nobody anything they didn't already know.
 */
function StopRunButton({ id }: { id: number }) {
  const stop = useStopRun();
  const [asking, setAsking] = useState(false);

  return (
    <>
      <Tooltip title="Stop this run">
        <span>
          <Button
            size="small"
            color="error"
            variant="outlined"
            startIcon={<StopIcon />}
            onClick={() => setAsking(true)}
            disabled={stop.isPending}
          >
            Stop
          </Button>
        </span>
      </Tooltip>

      <Dialog open={asking} onClose={() => setAsking(false)}>
        <DialogTitle>Stop run #{id}?</DialogTitle>
        <DialogContent>
          <Typography variant="body2" sx={{ mb: 1.5 }}>
            The step running now is killed, nothing further is started, and any
            background tasks this run started are stopped.
          </Typography>
          <Typography variant="body2" color="text.secondary">
            Whatever has already happened stays happened — a step that wrote files or
            published an artifact is not undone. The logs remain readable.
          </Typography>
          {stop.error && <ErrorNote error={stop.error} />}
        </DialogContent>
        <DialogActions>
          <Button onClick={() => setAsking(false)}>Keep running</Button>
          <Button
            color="error"
            variant="contained"
            disabled={stop.isPending}
            onClick={() => stop.mutate(id, { onSuccess: () => setAsking(false) })}
          >
            Stop it
          </Button>
        </DialogActions>
      </Dialog>
    </>
  );
}

/** What a selected step is, beyond its command: where it's from, how it
 *  behaves, and the variables it depends on. */
function StepDetails({ step, env, now }: { step: StepView; env: EnvVar[]; now: number }) {
  const badges: string[] = [];
  const took = elapsedOf(step, now);
  if (took) badges.push(step.finished_at ? `took ${took}` : `running ${took}`);
  if (step.finished_at) badges.push(`finished ${clockTime(step.finished_at)}`);
  if (step.push) badges.push("push");
  else if (step.kind) badges.push(step.kind);
  if (step.background) badges.push("⚡ background");
  else if (step.persistent) badges.push("persistent");
  if (step.timeout) badges.push(`timeout ${step.timeout}`);
  if (step.requires.length > 0) badges.push(`needs ${step.requires.join(", ")}`);

  // The variables this step reads, with the values the run resolved for them —
  // the step's own `[env]` table is shown separately, since it overrides them.
  const own = new Set(Object.keys(step.env));
  const reads = env.filter((variable) => !own.has(variable.key));

  // The dependency block is worth showing on its own, so "nothing to say" now
  // means nothing to say about *any* of it.
  const bare =
    !step.workspace &&
    !step.description &&
    badges.length === 0 &&
    reads.length === 0 &&
    own.size === 0 &&
    (step.env_files?.length ?? 0) === 0 &&
    !step.deps?.name;
  if (bare) return null;

  return (
    <Stack spacing={1}>
      <Stack direction="row" spacing={1} flexWrap="wrap" useFlexGap alignItems="center">
        {step.workspace && <Chip size="small" color="secondary" label={step.workspace} />}
        {badges.map((badge) => (
          <Chip key={badge} size="small" variant="outlined" label={badge} />
        ))}
        {step.description && (
          <Typography variant="caption" color="text.secondary">
            {step.description}
          </Typography>
        )}
        {step.owner && (
          <Typography variant="caption" color="text.secondary">
            · {step.owner}
          </Typography>
        )}
      </Stack>

      {(reads.length > 0 || own.size > 0) && (
        <Stack direction="row" spacing={0.75} flexWrap="wrap" useFlexGap alignItems="center">
          <Typography variant="caption" color="text.secondary">
            environment
          </Typography>
          <StepEnvChips env={step.env} />
          {reads.map((variable) => (
            <EnvVarChip key={variable.key} variable={variable} />
          ))}
        </Stack>
      )}

      {(step.env_files?.length ?? 0) > 0 && (
        <Stack direction="row" spacing={0.75} flexWrap="wrap" useFlexGap alignItems="baseline">
          <Tooltip title="The .env files this step resolves through, outermost first. Its own workspace's file answers first; anything that file doesn't set falls back outward.">
            <Typography variant="caption" color="text.secondary">
              env files
            </Typography>
          </Tooltip>
          <Typography variant="caption" sx={{ fontFamily: monoFontStack, wordBreak: "break-all" }}>
            {step.env_files.join(" → ")}
          </Typography>
        </Stack>
      )}

      <TargetDependencies deps={step.deps} />
    </Stack>
  );
}

/**
 * What this target is defined by: the files it reads, the files it writes, the
 * variables it keys on, the commands it runs, and the targets it needs.
 *
 * All five in one block, in that order, because the question they answer is one
 * question — "why did this run?" — and answering it from five places is how
 * people end up assuming the cache is broken.
 */
function TargetDependencies({ deps }: { deps: TargetDeps }) {
  // A recovery node has no build, so the daemon sends an empty target. There is
  // nothing true to say about it.
  if (!deps || !deps.name) return null;

  const undeclared = undeclaredEnv(deps);

  return (
    <Stack spacing={0.5} sx={{ mt: 0.5 }}>
      <DepRow
        label="depends on"
        value={deps.needs.length > 0 ? deps.needs.join(", ") : "nothing — it can start immediately"}
      />
      <DepRow
        label="reads"
        value={
          deps.inputs.length === 0
            ? "no input files declared"
            : `${deps.input_files} file(s), ${humanizeBytes(deps.input_bytes)} — ${deps.inputs.join(", ")}`
        }
        title={deps.exclude.length > 0 ? `excluding ${deps.exclude.join(", ")}` : undefined}
      />
      <DepRow
        label="writes"
        value={
          deps.outputs.length === 0
            ? "no output files declared, so nothing could be restored"
            : `${deps.output_files} file(s), ${humanizeBytes(deps.output_bytes)} — ${deps.outputs.join(", ")}`
        }
      />
      <DepRow
        label="keys on"
        value={deps.env.length > 0 ? deps.env.join(", ") : "no variables"}
        title="Variables folded into this target's cache key"
      />
      {deps.commands.length > 0 && <DepRow label="runs" value={deps.commands.join(" ; ")} mono />}
      {!deps.cached && deps.why_uncached && (
        <DepRow label="not cached" value={deps.why_uncached} />
      )}
      {deps.cached && undeclared.length > 0 && (
        <Typography variant="caption" color="warning.main">
          ⚠ reads {undeclared.join(", ")} without declaring{" "}
          {undeclared.length === 1 ? "it" : "them"} in cache.env, so changing{" "}
          {undeclared.length === 1 ? "it" : "them"} would not invalidate this target.
        </Typography>
      )}
    </Stack>
  );
}

/**
 * What the graph is focused on, said in words underneath it.
 *
 * The dimming shows *which* nodes a dependency reaches; this says what the
 * dependency is — the globs, what they currently match, and the steps on the
 * other end of the edges — because a node label truncated to fit the canvas
 * can't. It's also where Clear lives, so getting the whole graph back doesn't
 * depend on remembering which node was clicked.
 */
function FocusNote({
  workflow,
  focused,
  onClear,
}: {
  workflow: WorkflowView;
  focused: string;
  onClear: () => void;
}) {
  const groups = useMemo(() => inputGroups(workflow), [workflow]);

  const key = envKeyOf(focused);
  const variable = key === null ? null : (workflow.env.vars.find((v) => v.key === key) ?? null);
  const writer = outputStepOf(focused);
  const producer = writer === null ? null : (workflow.steps.find((s) => s.name === writer) ?? null);
  const group = groups.get(focused) ?? null;

  // The run's shape can change under a focus — a workflow recompiles, a step
  // is filtered out. A node that isn't there any more has nothing to say.
  if (!variable && !producer && !group) return null;

  const subject = variable
    ? null
    : producer
      ? producer.deps.outputs.join(", ")
      : group!.deps.inputs.join(", ");

  const detail = variable
    ? `${variable.purpose ? `${variable.purpose} — ` : ""}${
        variable.steps.length > 0 ? `read by ${variable.steps.join(", ")}` : "no step reads this"
      }`
    : producer
      ? `${producer.deps.output_files} file(s), ${humanizeBytes(
          producer.deps.output_bytes,
        )} — written by ${producer.name}`
      : `${group!.deps.input_files} file(s), ${humanizeBytes(
          group!.deps.input_bytes,
        )} — read by ${group!.steps.join(", ")}`;

  return (
    <Stack direction="row" spacing={1} alignItems="center" flexWrap="wrap" useFlexGap>
      {variable ? (
        <EnvVarChip variable={variable} />
      ) : (
        <Chip
          size="small"
          variant="outlined"
          color={producer ? "success" : "info"}
          label={subject}
          title={subject ?? undefined}
          sx={{ fontFamily: monoFontStack, maxWidth: 420 }}
        />
      )}
      <Typography variant="caption" color="text.secondary">
        {detail}
      </Typography>
      <Button size="small" onClick={onClear}>
        Clear
      </Button>
    </Stack>
  );
}

function DepRow({
  label,
  value,
  title,
  mono,
}: {
  label: string;
  value: string;
  title?: string;
  mono?: boolean;
}) {
  const row = (
    <Stack direction="row" spacing={1} alignItems="baseline">
      <Typography
        variant="caption"
        color="text.secondary"
        sx={{ minWidth: 78, flexShrink: 0, textAlign: "right" }}
      >
        {label}
      </Typography>
      <Typography
        variant="caption"
        sx={{
          color: "text.primary",
          fontFamily: mono ? monoFontStack : undefined,
          wordBreak: "break-word",
        }}
      >
        {value}
      </Typography>
    </Stack>
  );
  return title ? <Tooltip title={title}>{row}</Tooltip> : row;
}

/**
 * The steps a dependency node's own edges reach — the ones that stay lit when
 * it is focused.
 *
 * A variable reaches every step that reads it; a set of inputs reaches every
 * step that shares the declaration (they rebuild together, which is the whole
 * reason they share a node); a set of outputs reaches the one step that writes
 * it. Nothing here walks past those edges: a graph that lit up steps it hasn't
 * drawn a line to would be inventing a relationship.
 */
function litSteps(
  workflow: WorkflowView,
  id: string,
  groups: Map<string, { deps: TargetDeps; steps: string[] }>,
): Set<string> {
  const key = envKeyOf(id);
  if (key !== null) {
    return new Set(workflow.env.vars.find((variable) => variable.key === key)?.steps ?? []);
  }
  const writer = outputStepOf(id);
  if (writer !== null) return new Set([writer]);
  return new Set(groups.get(id)?.steps ?? []);
}

/**
 * Everything a step depends on: the steps it waits for, transitively, and the
 * variables and file sets those steps read.
 *
 * *Transitively* is the point. A step's own `needs` are already drawn as the
 * edges touching it, and reading them off the graph is easy while the graph is
 * small. The question that gets hard on a monorepo graph — the one the dimming
 * answers — is "what does this actually wait for", whose answer is four
 * packages deep and reachable only by tracing edges backwards by eye across a
 * canvas wide enough to need scrolling.
 *
 * Ancestors only, never descendants: a step is not dependent on what comes
 * after it, and lighting both directions would make every node in a chain look
 * like every other node's dependency.
 */
function dependencyClosure(workflow: WorkflowView, start: string): Set<string> {
  // Predecessors by `needs` alone. Error and retry branches are where a run
  // goes when something fails, not what a step waits for.
  const parents = new Map<string, string[]>();
  for (const edge of workflow.edges) {
    if (edge.kind !== "needs") continue;
    const list = parents.get(edge.to) ?? [];
    list.push(edge.from);
    parents.set(edge.to, list);
  }

  const lit = new Set<string>([start]);
  const queue = [start];
  while (queue.length > 0) {
    const current = queue.pop()!;
    for (const parent of parents.get(current) ?? []) {
      // The guard is also the cycle break: a malformed workflow can have one,
      // and the graph should still render.
      if (lit.has(parent)) continue;
      lit.add(parent);
      queue.push(parent);
    }
  }

  // The dependency nodes those steps hang off, so turning on Environment or
  // Files while a step is focused shows what the chain reads rather than
  // dimming all of it. `litSteps` is the same relation read the other way, so
  // the two directions cannot disagree about which edges exist.
  for (const variable of workflow.env.vars) {
    if (variable.steps.some((name) => lit.has(name))) lit.add(envNodeId(variable.key));
  }
  for (const [id, group] of inputGroups(workflow)) {
    if (group.steps.some((name) => lit.has(name))) lit.add(id);
  }
  for (const name of [...lit]) {
    if (!isDependencyNode(name)) lit.add(outputNodeId(name));
  }

  return lit;
}

/**
 * The whole flowchart: the step DAG, and — when they're switched on — the
 * dependency columns feeding it.
 *
 * Everything drawn goes through **one** layout call, which is the point. The
 * dependency nodes used to be positioned separately, in a column parked off to
 * the left, and their edges were then drawn straight from there to whichever
 * step read them — across every column in between, over the top of whatever
 * nodes were in the way. A variable feeding a step four waves in produced a
 * line through four nodes it had nothing to do with, and a reader has no way to
 * tell that line from the ones that mean something.
 *
 * Pinning them to negative columns keeps the shape that was right about the old
 * arrangement — inputs arrive from the side, steps keep their waves — while
 * putting them inside the layering, so their edges are routed down lanes of
 * their own like every other edge that skips a column.
 */
function buildFlow(
  workflow: WorkflowView,
  theme: Theme,
  showOrder: boolean,
  showEnv: boolean,
  showFiles: boolean,
  focused: string | null,
  inspect: boolean,
): { nodes: Node[]; edges: Edge[] } {
  const stepIds = workflow.steps.map((s) => s.name);
  const byName = new Map(workflow.steps.map((s) => [s.name, s]));

  // Input sets are grouped once, here, so the focus and the drawing agree
  // about which steps share a node.
  const groups = inputGroups(workflow);

  const focus: Focus =
    focused === null
      ? NO_FOCUS
      : (() => {
          // A dependency node lights what it feeds; a step lights what feeds
          // it. Two directions, because the two kinds of node are asked
          // opposite questions — "who reads DATABASE_URL?" of a variable, and
          // "what does this wait for?" of a step.
          const reached = isDependencyNode(focused)
            ? litSteps(workflow, focused, groups)
            : dependencyClosure(workflow, focused);
          return { id: focused, lit: (id: string) => id === focused || reached.has(id) };
        })();

  // Only `needs` edges define run order; error/retry branches are annotations
  // on top and would distort the layout if they drove depth.
  const orderEdges = workflow.edges
    .filter((e) => e.kind === "needs")
    .map((e) => ({ source: e.from, target: e.to }));

  // ── the dependency nodes, when they're on ────────────────────────────────
  // A variable nothing reads (a `.env` line no step uses) isn't drawn: it has
  // no edge to justify a node. The panel below the graph still lists it.
  const present = new Set(stepIds);
  const variables = showEnv ? workflow.env.vars.filter((v) => v.steps.length > 0) : [];
  const envEdges = variables.flatMap((variable) =>
    variable.steps
      // A variable's step list comes from the resolved run, so guard against an
      // edge into a node this view isn't drawing.
      .filter((step) => present.has(step))
      .map((step) => ({ source: envNodeId(variable.key), target: step })),
  );

  const inputs = showFiles ? [...groups.values()] : [];
  const inputEdges = inputs.flatMap((group) =>
    group.steps.map((step) => ({ source: inputNodeId(group.steps[0]), target: step })),
  );
  const producers = showFiles
    ? workflow.steps.filter((step) => step.deps?.name && step.deps.outputs.length > 0)
    : [];
  const outputEdges = producers.map((step) => ({
    source: step.name,
    target: outputNodeId(step.name),
  }));

  // Recovery steps are left out of the numbering: the engine only enters them
  // from a failed step's `on_error`, so they have no position in the sequence
  // the run takes when everything works. Background tasks are left out for the
  // opposite reason — they always run, but nothing waits for them, so they hold
  // no position in the sequence either.
  const sequence = showOrder
    ? executionOrder(
        workflow.steps.filter((s) => !s.recover).map((s) => s.name),
        orderEdges,
        { exclude: (id) => byName.get(id)?.background ?? false },
      )
    : new Map<string, number>();

  const ids = [
    ...stepIds,
    ...variables.map((variable) => envNodeId(variable.key)),
    ...inputs.map((group) => inputNodeId(group.steps[0])),
    ...producers.map((step) => outputNodeId(step.name)),
  ];

  // What the layout routes. Each family is its own group, so wires only share
  // a trunk with wires drawn the same way; solid `needs` wires may also share
  // the last run into a step they converge on (see `LayoutEdge.merge`).
  //
  // `error` edges rank: a recovery step goes a column after the step that
  // falls into it, which is where it happens. `retry` edges don't — they point
  // back at the step they re-run, and ranking them would make a cycle.
  const layoutEdges: LayoutEdge[] = [
    ...orderEdges.map((e) => ({ ...e, group: "needs", merge: true })),
    ...workflow.edges
      .filter((e) => e.kind !== "needs")
      .map((e) => ({
        source: e.from,
        target: e.to,
        group: e.kind,
        rank: e.kind !== "retry",
      })),
    ...envEdges.map((e) => ({ ...e, group: "env" })),
    ...inputEdges.map((e) => ({ ...e, group: "in" })),
    ...outputEdges.map((e) => ({ ...e, group: "out" })),
  ];

  const { nodes: positioned, routes } = layeredLayout(
    ids,
    layoutEdges,
    (id) => nodeData(id, byName, groups, sequence, showOrder, workflow, inspect),
    {
      // A column has to be wider than the widest node it can hold, or a long
      // step name grows its node into the next column and the edges arriving
      // there run over it. `NODE_WIDTH` caps the node; this leaves a gap the
      // wires can turn in.
      columnWidth: NODE_WIDTH + 130,
      nodeWidth: NODE_WIDTH,
      rowGap: 26,
      heightOf: (id) => nodeHeight(id, byName, inspect),
      // Variables and input files are inputs to the run, not steps of it, so
      // they take columns of their own to the left — variables outside the
      // files, so the two kinds read as two columns rather than one pile.
      // Outputs need no pin: they hang off the step that writes them, one
      // column along, which is exactly where the depth puts them.
      // Variables go outside the input files when both are on, and take that
      // column themselves when the files are off — an empty column between the
      // inputs and the run is just a gap to scroll past.
      pin: (id) =>
        envKeyOf(id) !== null ? (showFiles ? -2 : -1) : id.startsWith("in::") ? -1 : undefined,
      // Background tasks sit in a row underneath rather than in a column,
      // because a column would claim they gate what follows them. They don't.
      bottom: (id) => byName.get(id)?.background ?? false,
    },
  );

  const nodes: Node[] = positioned.map((node) => ({
    ...node,
    style: nodeStyle(node.id, byName, theme, focus, inspect),
  }));
  const route = (source: string, target: string) => ({
    route: routes.get(routeKey(source, target)),
  });

  // ── the edges ────────────────────────────────────────────────────────────
  const edges: Edge[] = workflow.edges.map((edge, index) => {
    // An edge survives the dimming only if both ends did — so a focused node's
    // chain keeps the order between its steps, and everything else recedes.
    const on = focus.lit(edge.from) && focus.lit(edge.to);
    // Inspect mode: the edge along which an upstream step held this one back
    // from its cache. That's the relationship being asked about, so it's the
    // one drawn loudest.
    const blocking =
      inspect &&
      edge.kind === "needs" &&
      (byName.get(edge.to)?.cache?.blocked_by.includes(edge.from) ?? false);
    const stroke = blocking
      ? theme.palette.error.main
      : edge.kind === "error"
        ? theme.palette.error.main
        : edge.kind === "retry"
          ? theme.palette.warning.main
          : // `divider` is a hairline meant to separate panels, and on a graph
            // with fifty edges it reads as grey noise rather than as fifty
            // statements about what waits for what. This is the same neutral
            // held to a contrast you can actually trace with your eye.
            theme.palette.text.secondary;

    return {
      ...ROUTED_EDGE,
      id: `${edge.from}->${edge.to}-${index}`,
      source: edge.from,
      target: edge.to,
      data: route(edge.from, edge.to),
      label: blocking ? "blocks cache" : edge.kind === "needs" ? undefined : edge.kind,
      // react-flow's label is white-on-white in dark mode; these follow the
      // page instead.
      labelStyle: { fill: theme.palette.text.secondary, fontSize: 10 },
      labelBgStyle: { fill: theme.palette.background.paper },
      labelBgPadding: [4, 2] as [number, number],
      labelBgBorderRadius: 4,
      animated: byName.get(edge.from)?.status === "running",
      // Lifted above its neighbours while it is part of what you asked about.
      zIndex: on && focus.id !== null ? 1 : 0,
      markerEnd: { ...ROUTED_EDGE.markerEnd, color: stroke },
      style: {
        stroke,
        strokeWidth: blocking ? 2.5 : on && focus.id !== null ? 2 : 1.4,
        strokeDasharray: edge.kind === "needs" ? undefined : "5 4",
        opacity: on ? 1 : DIMMED,
      },
    };
  });

  // A dependency edge is dashed and thin: it is a precondition, not a step that
  // ran before this one. The focused node's own edges are the one thing on the
  // canvas that should be moving.
  const dependencyEdge = (
    source: string,
    target: string,
    colour: string,
    extra: { animated?: boolean } = {},
  ): Edge[] => {
    const lit = focus.lit(source) && focus.lit(target);
    const picked = focus.id === source || focus.id === target;
    return [
      {
        ...ROUTED_EDGE,
        id: `dep:${source}->${target}`,
        source,
        target,
        data: route(source, target),
        animated: picked || (extra.animated ?? false),
        zIndex: picked ? 1 : 0,
        markerEnd: { ...ROUTED_EDGE.markerEnd, color: colour },
        style: {
          stroke: colour,
          strokeDasharray: "2 4",
          strokeWidth: picked ? 2 : 1,
          opacity: lit ? 0.75 : DIMMED,
        },
      },
    ];
  };

  for (const variable of variables) {
    const colour = envColour(variable, theme);
    for (const edge of envEdges.filter((e) => e.source === envNodeId(variable.key))) {
      edges.push(...dependencyEdge(edge.source, edge.target, colour));
    }
  }
  for (const edge of inputEdges) {
    edges.push(...dependencyEdge(edge.source, edge.target, theme.palette.info.main));
  }
  for (const step of producers) {
    edges.push(
      ...dependencyEdge(step.name, outputNodeId(step.name), theme.palette.success.main, {
        // A finished step's outputs are on disk; a running one's are being
        // written as you watch.
        animated: step.status === "running",
      }),
    );
  }

  return { nodes, edges };
}

/** The colour a variable is drawn in: unset is a problem, the rest are inputs. */
function envColour(variable: EnvVar, theme: Theme): string {
  if (unsetProblem(variable)) return theme.palette.error.main;
  // An optional variable nobody set is a fact, not a fault: drawn quietly.
  if (variable.origin === "unset") return theme.palette.text.disabled;
  return theme.palette.secondary.main;
}

/** What each kind of node puts on the canvas. */
function nodeData(
  id: string,
  byName: Map<string, StepView>,
  groups: Map<string, { deps: TargetDeps; steps: string[] }>,
  sequence: Map<string, number>,
  showOrder: boolean,
  workflow: WorkflowView,
  inspect: boolean,
): Record<string, unknown> {
  const step = byName.get(id);
  if (step) {
    return {
      label: (
        <NodeLabel
          step={step}
          order={showOrder ? (sequence.get(id) ?? null) : null}
          inspect={inspect}
        />
      ),
      status: step.status,
    };
  }

  const key = envKeyOf(id);
  if (key !== null) {
    const variable = workflow.env.vars.find((v) => v.key === key);
    return {
      label: variable ? <EnvNodeLabel variable={variable} /> : key,
      status: "pending" as StepStatus,
    };
  }

  const group = groups.get(id);
  if (group) {
    return {
      label: <FileNodeLabel deps={group.deps} kind="reads" />,
      status: "pending" as StepStatus,
    };
  }

  const producer = outputStepOf(id);
  const deps = producer ? byName.get(producer)?.deps : undefined;
  return {
    label: deps ? <FileNodeLabel deps={deps} kind="writes" /> : id,
    status: "pending" as StepStatus,
  };
}

/** How each kind of node is drawn, including whether the focus has dimmed it. */
function nodeStyle(
  id: string,
  byName: Map<string, StepView>,
  theme: Theme,
  focus: Focus,
  inspect: boolean,
): React.CSSProperties {
  // The ring marks the node that was *clicked*, not everything the click lit
  // up. With a step's whole upstream chain lit, ringing all of it would say
  // every node in the chain was the subject of the question.
  const picked = focus.id === id;
  const common = {
    background: theme.palette.background.paper,
    color: theme.palette.text.primary,
    borderRadius: 8,
    // Bounded, so a node can't grow across the gap the wires route through. A
    // long name wraps inside the box instead of widening it.
    width: NODE_WIDTH,
    height: nodeHeight(id, byName, inspect),
    // Border-box, so a border thickening on focus eats into the node rather
    // than growing it off the height the wires were routed to.
    boxSizing: "border-box" as const,
    display: "flex",
    alignItems: "center",
    overflow: "hidden",
    textAlign: "left" as const,
    opacity: focus.lit(id) ? 1 : DIMMED,
    boxShadow: picked ? `0 0 0 3px ${theme.palette.secondary.main}55` : undefined,
  };

  const step = byName.get(id);
  if (step) {
    return {
      ...common,
      // Recovery nodes are dashed: they're branches you hope never run.
      border: `2px ${step.recover ? "dashed" : "solid"} ${
        picked
          ? theme.palette.secondary.main
          : // In inspect mode the border is the cache's verdict, not the
            // step's: the status is still the glyph beside the name.
            inspect && !step.recover
            ? cacheTone(step.cache, theme)
            : // Reused is as good an outcome as ran-and-passed.
              wasReused(step.cache)
              ? theme.palette.success.main
              : statusColor(step.status, theme)
      }`,
      fontSize: 12,
      padding: "6px 12px",
    };
  }

  // Dependency nodes: dashed like the edges that leave them, because a file set
  // or a variable is a precondition rather than something that ran. The
  // selected one goes solid — it's the subject now, not an aside.
  const colour = isFileNode(id)
    ? id.startsWith("in::")
      ? theme.palette.info.main
      : theme.palette.success.main
    : theme.palette.secondary.main;

  return {
    ...common,
    border: `${picked ? 2 : 1}px ${picked ? "solid" : "dashed"} ${colour}`,
    borderRadius: envKeyOf(id) !== null ? 18 : 8,
    fontSize: 11,
    padding: "0 10px",
    cursor: "pointer",
  };
}

/**
 * Steps sharing a directory and a set of input globs, grouped.
 *
 * In a monorepo every step of a package inherits that package's
 * `cache.inputs`, so one node per distinct set feeding several steps is both
 * smaller and truer than one node each — and the duplication it collapses is
 * real information: those steps rebuild together. Keyed by the node id the
 * group takes, so focusing one can find its members without regrouping.
 */
function inputGroups(workflow: WorkflowView): Map<string, { deps: TargetDeps; steps: string[] }> {
  const byDeclaration = new Map<string, { deps: TargetDeps; steps: string[] }>();
  for (const step of workflow.steps) {
    const deps = step.deps;
    if (!deps?.name || deps.inputs.length === 0) continue;
    const key = `${deps.dir}\u0000${deps.inputs.join("\u0000")}`;
    const group = byDeclaration.get(key);
    if (group) group.steps.push(step.name);
    else byDeclaration.set(key, { deps, steps: [step.name] });
  }
  // Re-keyed by node id now that each group's first step — the id it takes —
  // is known. Insertion order is preserved, so the column doesn't reshuffle.
  return new Map([...byDeclaration.values()].map((group) => [inputNodeId(group.steps[0]), group]));
}

/** A file set on the canvas: what it matches, and what that came to. */
function FileNodeLabel({ deps, kind }: { deps: TargetDeps; kind: "reads" | "writes" }) {
  const patterns = kind === "reads" ? deps.inputs : deps.outputs;
  const count = kind === "reads" ? deps.input_files : deps.output_files;
  const bytes = kind === "reads" ? deps.input_bytes : deps.output_bytes;

  return (
    <Box sx={{ textAlign: "left", minWidth: 0, flexGrow: 1 }}>
      <Typography
        variant="caption"
        sx={{ display: "block", color: "text.secondary", ...oneLine(LINE.name) }}
      >
        {kind === "reads" ? "reads" : "writes"} · {count} file{count === 1 ? "" : "s"} ·{" "}
        {humanizeBytes(bytes)}
      </Typography>
      <Typography
        variant="caption"
        sx={{
          display: "block",
          fontFamily: monoFontStack,
          // The globs are the declaration; a long list is truncated rather than
          // allowed to stretch the node across the canvas.
          ...oneLine(LINE.name),
        }}
        title={patterns.join(", ")}
      >
        {patterns.join(", ")}
      </Typography>
    </Box>
  );
}

/** A variable on the canvas: its name, and underneath it the value. */
function EnvNodeLabel({ variable }: { variable: EnvVar }) {
  return (
    <Box
      sx={{ fontFamily: monoFontStack, minWidth: 0, flexGrow: 1 }}
      title={variable.purpose ? `${variable.key} — ${variable.purpose}` : variable.key}
    >
      <Box sx={{ ...oneLine(LINE.name), fontWeight: 700 }}>{variable.key}</Box>
      <Box
        sx={{
          ...oneLine(LINE.name),
          opacity: 0.75,
          fontStyle: variable.value === null ? "italic" : "normal",
        }}
      >
        {envValueText(variable)}
      </Box>
    </Box>
  );
}

/** The colour a step's status is drawn in — the graph's borders, the minimap. */
function statusColor(status: StepStatus | string, theme: Theme): string {
  return statusColour(status, theme);
}

function stageColor(status: string): "success" | "error" | "warning" | "default" {
  switch (status) {
    case "success":
      return "success";
    case "failed":
      return "error";
    case "running":
      return "warning";
    // "stopped" falls through to neutral on purpose: somebody asked for it, so
    // it is not an error, and colouring it red would send them looking for one.
    default:
      return "default";
  }
}

/**
 * Subscribe to a run's SSE stream.
 *
 * Each frame is the complete run state rather than a delta — a run has tens
 * of steps, not thousands of log lines, so sending the whole thing is simpler
 * than reconciling patches and costs nothing measurable.
 */
function useRunStream(runId: number) {
  const [state, setState] = useState<RunState | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    setState(null);
    setError(null);

    const source = new EventSource(streamUrl(`/api/run/runs/${runId}/stream`));

    source.onmessage = (event) => {
      const next = JSON.parse(event.data) as RunState;
      setState(next);
      // The daemon closes the stream once the run is done; don't let
      // EventSource's auto-retry reopen it in a loop.
      if (next.done) source.close();
    };

    source.onerror = () => {
      setState((previous) => {
        if (previous?.done) source.close();
        return previous;
      });
      setError("Lost the connection to the daemon.");
    };

    return () => source.close();
  }, [runId]);

  return { state, error: state ? null : error };
}
