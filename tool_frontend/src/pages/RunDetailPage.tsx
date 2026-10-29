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
  AlertTitle,
  Autocomplete,
  Box,
  LinearProgress,
  Button,
  Chip,
  Dialog,
  DialogActions,
  DialogContent,
  DialogTitle,
  InputAdornment,
  ListItemText,
  ListSubheader,
  Menu,
  MenuItem,
  Stack,
  Switch,
  Tab,
  Tabs,
  TextField,
  Tooltip,
  Typography,
} from "@mui/material";
import ArrowDropDownIcon from "@mui/icons-material/ArrowDropDown";
import LayersIcon from "@mui/icons-material/Layers";
import SearchIcon from "@mui/icons-material/Search";
import ArrowBackIcon from "@mui/icons-material/ArrowBack";
import FolderOpenIcon from "@mui/icons-material/FolderOpen";
import ManageSearchIcon from "@mui/icons-material/ManageSearch";
import StopIcon from "@mui/icons-material/Stop";
import ReplayIcon from "@mui/icons-material/Replay";
import TerminalIcon from "@mui/icons-material/Terminal";
import TuneIcon from "@mui/icons-material/Tune";
import ViewQuiltIcon from "@mui/icons-material/ViewQuilt";
import EditIcon from "@mui/icons-material/AccountTree";
import RequiredIcon from "@mui/icons-material/Emergency";
import OptionalIcon from "@mui/icons-material/HelpOutline";
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
  restoringLine,
  restoringPercent,
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
import { useWorkspace } from "../api/workspace";
import { LogView } from "../components/LogView";
import {
  BoxLayout,
  rowsFor,
  useBoxLayout,
  type BoxItem,
  type DefaultLayouts,
} from "../components/BoxLayout";
import { RecreateDrawer } from "../components/RecreateDrawer";
import { EnvPrompt } from "../components/EnvPrompt";
import { GraphEditor } from "../components/GraphEditor";
import {
  EnvList,
  EnvVarChip,
  StepEnvChips,
  envValueText,
  unsetProblem,
} from "../components/EnvVars";
import {
  GraphCanvas,
  ROUTED_EDGE,
  STEP_LAYOUT,
  STEP_LINE,
  STEP_NODE_WIDTH,
  StatusIcon,
  StepLine,
  StepNodeBody,
  executionOrder,
  layeredLayout,
  oneLine,
  routeKey,
  statusColour,
  statusLabel,
  stepNodeHeight,
  stepNodeStyle,
  type LayoutEdge,
} from "@ciabatta/graphs";
import {
  CacheIcon,
  CacheReportView,
  Prose,
  cacheLabel,
  cacheTone,
  wasReused,
} from "../components/CacheReport";
import { useInspectMode } from "../state/inspect";
import { CacheKeyFilesView } from "../components/CacheKeyFiles";
import { CACHE_FILES_ANCHOR, ShortList } from "../components/ShortList";
import { CacheFilePicker } from "../components/CacheFilePicker";
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
const isDependencyNode = (id: string) =>
  isFileNode(id) || envKeyOf(id) !== null || envGroupOf(id) !== null;

/**
 * More variables than this on the graph and they're drawn as their groups.
 *
 * Past a handful, one node per variable stops being a picture: the column runs
 * off the bottom of the canvas, every node fans a wire into every step, and the
 * steps — what the graph is for — end up a thin strip beside it. What the
 * workflow declared each variable as is the grouping that still says
 * something at a glance, and the search finds any one of them.
 */
const ENV_GROUP_LIMIT = 6;

/** What a crowded variable column folds into: what the workflow file declared
 *  each variable as, and the ones it never declared either way. */
type EnvGroup = "required" | "optional" | "undeclared";
const ENV_GROUPS: EnvGroup[] = ["required", "optional", "undeclared"];
const envGroupNodeId = (group: EnvGroup) => `envgroup::${group}`;
const envGroupOf = (id: string): EnvGroup | null =>
  id.startsWith("envgroup::") ? (id.slice("envgroup::".length) as EnvGroup) : null;
const envGroupOfVariable = (variable: EnvVar): EnvGroup => envRole(variable) ?? "undeclared";

/** What each group is called on the canvas, and what hovering it explains. */
const ENV_GROUP_INFO: Record<EnvGroup, { label: string; why: string }> = {
  required: { label: "required", why: "required_env — the run refuses to start without them" },
  optional: { label: "optional", why: "env — valued from .env, and allowed to be unset" },
  undeclared: { label: "undeclared", why: "read by a step, but the workflow file doesn't list them" },
};

/** The node id a workflow file's stacked steps take, and back. */
const stackNodeId = (key: string) => `stack::${key}`;
const stackKeyOf = (id: string): string | null =>
  id.startsWith("stack::") ? id.slice("stack::".length) : null;

/**
 * Workflow files whose steps are drawn as one stacked node.
 *
 * A monorepo graph is a graph of workflow files: `api:build` waits for
 * `common:build`. The steps inside one file are its own business, and drawing
 * every one of them makes a ten-package graph a hundred nodes wide. So a file
 * with several steps is one node, its steps stacked like cards, with the one
 * on top chosen from a pop-out.
 */
interface Stacks {
  /** Each stack's steps, in the workflow's order, keyed by stack key. */
  members: Map<string, string[]>;
  /** The stack a step was folded into. */
  of: Map<string, string>;
  /** The step shown on top of each stack. */
  top: Map<string, string>;
}

const NO_STACKS: Stacks = { members: new Map(), of: new Map(), top: new Map() };

/** Height of the strip across the top of a stacked node that opens its steps. */
const STACK_HEADER = 18;

/** How far the graph fades what the focused node doesn't reach. */
const DIMMED = 0.18;

/** The shared step node's geometry, renamed for this file's many uses. */
const NODE_WIDTH = STEP_NODE_WIDTH;
const LINE = STEP_LINE;

/** A variable or file-set node: two lines, and its chrome. */
const DEPENDENCY_HEIGHT = 46;

/** How tall a node is drawn — what `layeredLayout` centres its wires on. */
function nodeHeight(
  id: string,
  byName: Map<string, StepView>,
  inspect: boolean,
  stacks: Stacks = NO_STACKS,
): number {
  const stack = stackKeyOf(id);
  if (stack !== null) {
    // As tall as the step on top, plus the strip that opens the others.
    return nodeHeight(stacks.top.get(stack) ?? "", byName, inspect) + STACK_HEADER;
  }
  const step = byName.get(id);
  if (!step) return DEPENDENCY_HEIGHT;
  return stepNodeHeight(
    Boolean(step.workspace),
    (step.background || step.after ? 1 : 0) +
      // Inspect mode's cache line, there for every step whether or not the
      // cache has spoken yet — so the node doesn't grow when it does.
      (inspect && !step.recover ? 1 : 0),
  );
}

/**
 * Where the run page's boxes go until somebody moves them: graph and logs side
 * by side, the same height so they line up, with the details and the
 * environment full width underneath. Watching a step run means watching two
 * things — which node is lit, and what it is printing — so they start next to
 * each other. Below `md` there's one column, and everything stacks.
 */
const RUN_LAYOUT: DefaultLayouts = {
  lg: [
    { i: "graph", x: 0, y: 0, w: 6, h: rowsFor(500), minW: 3, minH: 6 },
    { i: "logs", x: 6, y: 0, w: 6, h: rowsFor(500), minW: 3, minH: 6 },
    { i: "details", x: 0, y: rowsFor(500), w: 12, h: rowsFor(160), minW: 3, minH: 3 },
    { i: "env", x: 0, y: rowsFor(660), w: 12, h: rowsFor(320), minW: 3, minH: 3 },
  ],
  md: [
    { i: "graph", x: 0, y: 0, w: 12, h: rowsFor(460), minH: 6 },
    { i: "logs", x: 0, y: rowsFor(460), w: 12, h: rowsFor(400), minH: 6 },
    { i: "details", x: 0, y: rowsFor(860), w: 12, h: rowsFor(160), minH: 3 },
    { i: "env", x: 0, y: rowsFor(1020), w: 12, h: rowsFor(320), minH: 3 },
  ],
  sm: [
    { i: "graph", x: 0, y: 0, w: 1, h: rowsFor(400), minH: 6 },
    { i: "logs", x: 0, y: rowsFor(400), w: 1, h: rowsFor(360), minH: 6 },
    { i: "details", x: 0, y: rowsFor(760), w: 1, h: rowsFor(200), minH: 3 },
    { i: "env", x: 0, y: rowsFor(960), w: 1, h: rowsFor(320), minH: 3 },
  ],
};
/** Bumped when the boxes change enough that an old saved layout won't fit. */
const RUN_LAYOUT_KEY = "ciabatta-run-layout-v1";

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
  const [editing, setEditing] = useState(false);
  // Held here rather than in the workflow panel, so the header can offer to
  // reset it — and so switching workflow tabs keeps the arrangement.
  const layout = useBoxLayout(RUN_LAYOUT_KEY, RUN_LAYOUT);

  if (error) return <ErrorNote error={new Error(error)} />;
  if (!state) return <Loading label="Connecting to the run…" />;

  const workflow = state.workflows[workflowIndex];
  const status = state.run.status ?? (state.done ? "success" : "running");
  // The workflow files behind this run's steps, for the graph editor: each
  // step's cache id is `<member>:<workflow>:<step>`.
  const stepStatus: Record<string, string> = {};
  for (const step of workflow?.steps ?? []) {
    if (step.cache_id) stepStatus[step.cache_id] = step.status;
  }
  const units = [
    ...new Set(
      Object.keys(stepStatus).map((id) => id.slice(0, id.lastIndexOf(":"))),
    ),
  ];
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
      {/* One header row: what the run is and how it went on the left, what
          you can do with it on the right. The phases sit with the status
          rather than on a row of their own — they're part of "how it went". */}
      <Stack
        direction="row"
        alignItems="center"
        columnGap={1}
        rowGap={1}
        sx={{ mb: 1.5 }}
        flexWrap="wrap"
        useFlexGap
      >
        <IconButton
          component={Link}
          to="/run"
          size="small"
          aria-label="Back to runs"
          sx={{ ml: -0.5 }}
        >
          <ArrowBackIcon fontSize="small" />
        </IconButton>
        <Box sx={{ minWidth: 0 }}>
          <Stack direction="row" alignItems="center" spacing={1}>
            <Typography variant="h2" component="h1" noWrap>
              Run #{id}
            </Typography>
            <Chip
              size="small"
              variant="outlined"
              icon={<StatusIcon status={status} title={null} />}
              label={statusLabel(status)}
            />
          </Stack>
          <Typography variant="caption" color="text.secondary" component="div" noWrap>
            {state.run.workflows.join(", ")}
            {state.dry_run && " · dry run"}
            {timing && <RunClock timing={timing} live={!state.done} />}
            {(state.run.filter ?? []).length > 0 &&
              ` · filtered: ${state.run.filter.join(" ")}`}
            {state.run.env_profile && ` · profile: ${state.run.env_profile}`}
          </Typography>
        </Box>
        {workflow && <PhaseStrip workflow={workflow} />}
        <Box sx={{ flexGrow: 1 }} />
        {(state.run.args ?? []).length > 0 && (
          <Tooltip
            title={`Custom arguments, given after \`...\` for this run only: ${state.run.args!.join(" ")}. They're listed with the run's environment below.`}
          >
            <Chip
              size="small"
              color="warning"
              variant="outlined"
              label={`⚑ ${state.run.args!.join(" ")}`}
              sx={{ maxWidth: 320, fontFamily: monoFontStack }}
            />
          </Tooltip>
        )}
        <InspectToggle />
        <Tooltip
          title={
            units.length
              ? "Prune steps and change what they need, then save it to the workflow files."
              : "This run's steps don't come from workflow files, so there's nothing to edit."
          }
        >
          <Box component="span">
            <Button
              size="small"
              startIcon={<EditIcon />}
              variant={editing ? "contained" : "text"}
              disabled={units.length === 0}
              onClick={() => setEditing((on) => !on)}
            >
              Edit graph
            </Button>
          </Box>
        </Tooltip>
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
        {layout.customised && (
          <Tooltip title="Put the graph, logs, details and environment back where they started.">
            <Button size="small" startIcon={<ViewQuiltIcon />} onClick={layout.reset}>
              Reset layout
            </Button>
          </Tooltip>
        )}
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
          sx={{ mb: 1.5, minHeight: 36, "& .MuiTab-root": { minHeight: 36, py: 0.5 } }}
        >
          {state.workflows.map((r) => (
            <Tab key={r.name} label={r.name} />
          ))}
        </Tabs>
      )}

      {workflow && editing && (
        <GraphEditor
          project={state.run.project}
          units={units}
          status={stepStatus}
          onClose={() => setEditing(false)}
        />
      )}

      {workflow && !editing && (
        <WorkflowPanel
          runId={id}
          project={state.run.project}
          workflow={workflow}
          selectedStep={selectedStep}
          onSelectStep={setSelectedStep}
          layout={layout}
        />
      )}
    </>
  );
}

function WorkflowPanel({
  runId,
  project,
  workflow,
  selectedStep,
  onSelectStep,
  layout,
}: {
  runId: number;
  project: string;
  workflow: WorkflowView;
  selectedStep: string | null;
  onSelectStep: (name: string | null) => void;
  layout: ReturnType<typeof useBoxLayout>;
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
  // A workflow file's steps stacked into one node. On by default: it's what
  // keeps a monorepo graph a graph of packages rather than of every command.
  const [stacked, setStacked] = useState(true);
  // The step each stack was last turned to, by stack key.
  const [chosen, setChosen] = useState<Record<string, string>>({});

  const stacks = useMemo(
    () => (stacked ? workflowStacks(workflow, chosen, selectedStep) : NO_STACKS),
    [stacked, workflow, chosen, selectedStep],
  );
  const pickInStack = (stack: string, name: string) => {
    setChosen((current) => ({ ...current, [stack]: name }));
    setFocused(name);
    onSelectStep(name);
  };
  // The variables the graph would draw, and whether there are enough of them
  // to be drawn as their groups — the search is for exactly that case.
  const drawnVariables = workflow.env.vars.filter((v) => v.steps.length > 0);
  const grouped = showEnv && drawnVariables.length > ENV_GROUP_LIMIT;

  const { nodes, edges } = useMemo(
    () =>
      buildFlow(workflow, theme, showOrder, showEnv, showFiles, focused, inspect, stacks, pickInStack),
    // `pickInStack` only closes over setters and `onSelectStep`, which are stable.
    [workflow, theme, showOrder, showEnv, showFiles, focused, inspect, stacks],
  );
  const step = workflow.steps.find((s) => s.name === selectedStep);
  // Ticks only while the workflow is running, so a running phase or step
  // shows its time so far; a finished one's times are fixed.
  const now = useNow(workflow.status === "running");

  // Clicking a node focuses the graph on it. For a step that also opens its
  // logs and details; for a dependency — a variable, or a set of files read or
  // written — it lights the steps that touch it. Clicking the same node again
  // puts the whole graph back.
  const clickNode = (clicked: string) => {
    // A stack stands for the step on top of it.
    const stack = stackKeyOf(clicked);
    const id = stack === null ? clicked : (stacks.top.get(stack) ?? clicked);
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
      {(workflow.warnings ?? []).map((warning) => (
        <Alert
          key={warning}
          severity="warning"
          variant="filled"
          sx={{ mb: 2, fontSize: 15, "& .MuiAlert-icon": { fontSize: 28 } }}
        >
          <AlertTitle sx={{ fontWeight: 700 }}>Version mismatch with the remote cache</AlertTitle>
          {warning}
        </Alert>
      ))}

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

      {/* Answered in the app-wide dialog; noted here so the run says why
          it's sitting still. */}
      {(workflow.inputs ?? []).map((input) => (
        <Alert key={input.id} severity="info" sx={{ mb: 2 }}>
          <strong>{input.step}</strong> is waiting for input: {input.prompt}
        </Alert>
      ))}

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
        Every part of the page is a box you can move and resize; see
        `BoxLayout`. Selecting a step changes what the boxes show, never where
        they are — so the log you were reading doesn't jump to make room.
      */}
      <BoxLayout
        layouts={layout.layouts}
        onChange={layout.onChange}
        onFit={layout.fit}
        boxes={[
          {
            id: "graph",
            title: "Graph",
            body: "fill",
            actions: (
              <>
                {grouped && (
                  <VariableSearch
                    variables={drawnVariables}
                    onPick={(key) => {
                      setFocused(envNodeId(key));
                      onSelectStep(null);
                    }}
                  />
                )}
                <GraphViewMenu>
                <GraphToggle
                  label="Environment"
                  hint="Draw each environment variable as what it is — a dependency, feeding into every step that reads it. Values come from the run's resolved environment."
                  checked={showEnv}
                  onChange={(checked) => {
                    setShowEnv(checked);
                    // Focusing a node and then hiding it would dim the graph with
                    // nothing left lit to explain why.
                    if (
                      !checked &&
                      focused !== null &&
                      (envKeyOf(focused) !== null || envGroupOf(focused) !== null)
                    )
                      setFocused(null);
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
                  label="Stack"
                  hint="Draw each workflow file with several steps as one node, its steps stacked like cards. The strip across the top of the stack opens a list to choose the step shown. Off draws every step on its own."
                  checked={stacked}
                  onChange={setStacked}
                />
                <GraphToggle
                  label="Order"
                  hint="Number each node with its place in the run's sequence. Recovery steps aren't numbered — they only run if something fails."
                  checked={showOrder}
                  onChange={setShowOrder}
                />
                </GraphViewMenu>
              </>
            ),
            children: (
              <GraphCanvas
                nodes={nodes}
                edges={edges}
                height="100%"
                // Turning the environment column on and off changes the graph's
                // extent, so the view has to be re-fitted around it.
                fitKey={
                  `${showEnv ? "env" : ""}${grouped ? "+grouped" : ""}${showFiles ? "+files" : ""}${
                    stacked ? "+stacked" : ""
                  }${inspect ? "+inspect" : ""}` || "steps-only"
                }
                onNodeClick={(_, node) => clickNode(node.id)}
                onPaneClick={() => setFocused(null)}
                nodeColor={(node) =>
                  inspect && byNameHas(workflow, node.id)
                    ? cacheTone(workflow.steps.find((s) => s.name === node.id)?.cache, theme)
                    : statusColor(node.data?.status as StepStatus, theme)
                }
              />
            ),
          },
          {
            id: "logs",
            title: step ? `${step.name} logs` : "Workflow logs",
            body: "fill",
            actions: step && (
              <Button size="small" onClick={clearSelection}>
                All logs
              </Button>
            ),
            children: (
              <LogView
                // Keyed by what is being shown, so switching steps starts the new
                // log at its end rather than inheriting the old one's scroll.
                key={step ? step.name : "__workflow__"}
                lines={step ? step.logs : workflow.logs}
                dropped={(step ? step.dropped_logs : workflow.dropped_logs) ?? 0}
                fill
                title={step ? `${step.name} — run #${runId}` : `${workflow.name} — run #${runId}`}
              />
            ),
          },
          {
            id: "details",
            title: step ? step.name : "Details",
            body: "scroll",
            // As tall as what was clicked has to say: a hint line when nothing
            // is, a step's whole story when one is.
            fit: true,
            children: (
              <Inspector
                project={project}
                workflow={workflow}
                step={step ?? null}
                focused={focused}
                now={now}
                inspect={inspect}
                onClear={clearSelection}
                onFocus={setFocused}
              />
            ),
          },
          {
            id: "env",
            title: "Environment",
            body: "scroll",
            children: <EnvList report={workflow.env} />,
          },
        ] satisfies BoxItem[]}
      />
    </>
  );
}

/**
 * The graph's display switches, behind one button. Four switches in a row
 * took most of the header, for settings changed once in a while rather than
 * read on every glance.
 */
function GraphViewMenu({ children }: { children: React.ReactNode }) {
  const [anchor, setAnchor] = useState<HTMLElement | null>(null);
  return (
    <>
      <Tooltip title="What the graph draws">
        <Button
          size="small"
          startIcon={<TuneIcon fontSize="small" />}
          onClick={(event) => setAnchor(event.currentTarget)}
          aria-haspopup="menu"
          aria-expanded={anchor !== null}
        >
          View
        </Button>
      </Tooltip>
      <Menu
        anchorEl={anchor}
        open={anchor !== null}
        onClose={() => setAnchor(null)}
        anchorOrigin={{ vertical: "bottom", horizontal: "right" }}
        transformOrigin={{ vertical: "top", horizontal: "right" }}
        slotProps={{ paper: { sx: { minWidth: 220 } } }}
      >
        {children}
      </Menu>
    </>
  );
}

/** A switch in the graph's view menu. The whole row toggles it. */
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
    <Tooltip title={hint} placement="left">
      <MenuItem
        dense
        role="menuitemcheckbox"
        aria-checked={checked}
        onClick={() => onChange(!checked)}
      >
        <ListItemText primary={label} />
        <Switch size="small" edge="end" checked={checked} tabIndex={-1} />
      </MenuItem>
    </Tooltip>
  );
}

/**
 * The workflow's phases, in the run's header beside its status. Each is a
 * small chip — name, and how long it took — coloured by how it went; the
 * tooltip has the rest. A phase that fell through to its default did nothing,
 * so it's dimmed and given no time: "0.0s" would only suggest it did
 * something quickly.
 */
function PhaseStrip({ workflow }: { workflow: WorkflowView }) {
  // Ticks only while the workflow is running, so a running phase shows its
  // time so far; a finished one's times are fixed.
  const now = useNow(workflow.status === "running");
  return (
    <Stack direction="row" spacing={0.5} flexWrap="wrap" useFlexGap>
      {workflow.stages.map((stage) => {
        const skipped = stage.status === "skipped";
        const took = skipped ? null : elapsedOf(stage, now);
        const when = timeline(stage);
        return (
          <Tooltip
            key={stage.name}
            title={`${stage.name}: ${stage.status}${when ? ` — ${when}` : skipped ? "" : " — not reached"}`}
          >
            <Chip
              size="small"
              variant="outlined"
              color={stageColor(stage.status)}
              label={took ? `${stage.name} · ${took}` : stage.name}
              sx={{ height: 22, fontSize: 12, opacity: skipped ? 0.55 : 1 }}
            />
          </Tooltip>
        );
      })}
    </Stack>
  );
}

/**
 * Everything about what was clicked, in its own box.
 *
 * This used to be spread across three places: a step's details pushed in
 * above its logs, a "what it waits for" note pushed in under the graph, and a
 * "show all logs" button pushed in under the logs. Each click moved the panes
 * a different distance, so the line you were reading jumped. Now the panes
 * stay put and this one place fills in — always present, so even the first
 * click only changes what it says.
 */
function Inspector({
  project,
  workflow,
  step,
  focused,
  now,
  inspect,
  onClear,
  onFocus,
}: {
  project: string;
  workflow: WorkflowView;
  step: StepView | null;
  focused: string | null;
  now: number;
  inspect: boolean;
  onClear: () => void;
  onFocus: (id: string) => void;
}) {
  const body = step ? (
    <StepInspector
      project={project}
      workflow={workflow}
      step={step}
      now={now}
      inspect={inspect}
      onClear={onClear}
    />
  ) : focused !== null && isDependencyNode(focused) ? (
    envGroupOf(focused) !== null ? (
      <EnvGroupNote
        // Keyed by group, so switching groups starts the search afresh.
        key={focused}
        group={envGroupOf(focused)!}
        variables={workflow.env.vars.filter(
          (v) => v.steps.length > 0 && envGroupOfVariable(v) === envGroupOf(focused),
        )}
        onPick={(key) => onFocus(envNodeId(key))}
        onClear={onClear}
      />
    ) : (
      <FocusNote workflow={workflow} focused={focused} onClear={onClear} />
    )
  ) : null;

  return (
    // The box around it is the frame; this is only the content.
    <Box sx={{ display: "flex", flexDirection: "column" }}>
      {body ?? (
        <Typography variant="caption" color="text.secondary">
          Click a step to see its logs, what it waits for and why it ran. Click a
          variable or a file set to light up the steps that read it — or a group of
          variables to search it. A stacked node is one workflow file: its top strip
          picks the step shown.
          {inspect && " Cache inspect mode is on: each step says what the cache made of it."}
        </Typography>
      )}
    </Box>
  );
}

/** A selected step: what it is, what it waits for, and its command. */
function StepInspector({
  project,
  workflow,
  step,
  now,
  inspect,
  onClear,
}: {
  project: string;
  workflow: WorkflowView;
  step: StepView;
  now: number;
  inspect: boolean;
  onClear: () => void;
}) {
  const upstream = [...dependencyClosure(workflow, step.name)].filter(
    (id) => id !== step.name && !isDependencyNode(id),
  );
  // Already fetched elsewhere in the app, so this just reuses the cached
  // catalogue. Fails outright on a project that isn't a monorepo, in which
  // case `step.workspace` is null anyway and nothing below tries to show it.
  const { data: catalogue } = useWorkspace(project);
  const memberPath = catalogue?.members.find((m) => m.name === step.workspace)?.path;
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
      {/* The step only names its sub-workspace; this says where that lives,
          relative to the monorepo root, right under its name — the first
          thing you'd want to know before going to fix it. */}
      {step.workspace && (
        <Stack direction="row" spacing={0.5} alignItems="center" sx={{ minWidth: 0 }}>
          <FolderOpenIcon sx={{ fontSize: 15, color: "text.secondary", flexShrink: 0 }} />
          <Typography
            variant="body2"
            color="text.secondary"
            noWrap
            sx={{ fontFamily: monoFontStack, fontWeight: 600, minWidth: 0 }}
            title={memberPath ? `${memberPath} (${step.workspace})` : step.workspace}
          >
            {memberPath ? (memberPath === "." ? "./ (the repo root)" : `./${memberPath}`) : step.workspace}
          </Typography>
        </Stack>
      )}
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
      {inspect && !step.recover && step.deps?.name && (
        <PickFilesButton
          project={project}
          step={step}
          earlier={workflow.steps.filter((s) => upstream.includes(s.name))}
        />
      )}
      {step.status === "processing" && <ProcessingPanel />}
      {step.restoring && <RestoringPanel step={step} />}
      <StepCacheSection project={project} step={step} inspect={inspect} />
    </Stack>
  );
}

/**
 * Inspect mode's way to fix a step's file lists: browse the checkout, tick
 * what it reads and writes, and save that into its workflow file.
 */
function PickFilesButton({
  project,
  step,
  earlier,
}: {
  project: string;
  step: StepView;
  earlier: StepView[];
}) {
  const [open, setOpen] = useState(false);
  const unmatched = step.deps.unmatched_inputs?.length ?? 0;
  return (
    <Box>
      <Button
        size="small"
        variant={unmatched > 0 || step.deps.input_files === 0 ? "contained" : "outlined"}
        color="warning"
        startIcon={<FolderOpenIcon />}
        onClick={() => setOpen(true)}
      >
        Choose input &amp; output files…
      </Button>
      {/* Mounted only while open, so it starts from the step's config each time. */}
      {open && (
        <CacheFilePicker
          project={project}
          step={step}
          earlier={earlier}
          open
          onClose={() => setOpen(false)}
        />
      )}
    </Box>
  );
}

/**
 * A step the cache is still deciding about, said where its details are. On a
 * big Cargo workspace that takes a moment — the closure is resolved and every
 * input hashed — and a step sitting at "not started" through it reads as a
 * run that hung.
 */
function ProcessingPanel() {
  return (
    <Alert severity="info" sx={{ py: 0.5, "& .MuiAlert-message": { width: "100%" } }}>
      <Typography variant="caption" sx={{ display: "block" }}>
        Processing — working out whether this step can be reused: hashing its inputs, resolving
        its Cargo closure, checking its outputs…
      </Typography>
      <LinearProgress sx={{ mt: 0.75, borderRadius: 1 }} />
    </Alert>
  );
}

/** The node's detail line while the cache decides: a word and a bar, in the
 *  one line the node has for it. */
function ProcessingLine() {
  return (
    <Box component="span" sx={{ display: "flex", alignItems: "center", gap: 0.75, minWidth: 0 }}>
      processing…
      <LinearProgress color="info" sx={{ flexGrow: 1, height: 3, borderRadius: 1, minWidth: 24 }} />
    </Box>
  );
}

/**
 * A step whose outputs are coming down from the remote cache, said where the
 * step's details are — so a long transfer reads as a download in progress,
 * not as a run that stopped answering.
 */
function RestoringPanel({ step }: { step: StepView }) {
  const restoring = step.restoring!;
  const percent = restoringPercent(restoring);
  return (
    <Alert severity="info" sx={{ py: 0.5, "& .MuiAlert-message": { width: "100%" } }}>
      <Typography variant="caption" sx={{ display: "block" }}>
        {restoring.phase === "checking"
          ? `Asking the remote cache at ${restoring.server} whether it has this build…`
          : `Restoring from the remote cache at ${restoring.server}: ${restoringLine(restoring, humanizeBytes)}`}
      </Typography>
      <LinearProgress
        variant={percent === null ? "indeterminate" : "determinate"}
        value={percent ?? undefined}
        sx={{ mt: 0.75, borderRadius: 1 }}
      />
      {restoring.current && (
        <Typography
          variant="caption"
          color="text.secondary"
          sx={{ display: "block", mt: 0.5, fontFamily: monoFontStack, wordBreak: "break-all" }}
        >
          {restoring.current}
        </Typography>
      )}
    </Alert>
  );
}

/**
 * The cache's side of a selected step: compact normally, everything in inspect
 * mode — the key, the fingerprints of what it keyed on upstream, every file
 * that moved.
 */
function StepCacheSection({
  project,
  step,
  inspect,
}: {
  project: string;
  step: StepView;
  inspect: boolean;
}) {
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
      {inspect && step.cache.key && (
        <Box
          id={CACHE_FILES_ANCHOR}
          sx={{ mt: 1.25, pt: 1, borderTop: 1, borderColor: "divider", scrollMarginTop: 16 }}
        >
          <CacheKeyFilesView
            project={project}
            cacheKey={step.cache.key}
            live={step.status === "running" || step.status === "pending"}
          />
        </Box>
      )}
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
  return (
    <StepNodeBody
      name={step.name}
      workspace={step.workspace}
      status={step.status}
      order={order}
      detail={
        // A step served from the cache is "skipped" to the engine, but that
        // word belongs to steps a condition left out.
        step.status === "processing"
          ? <ProcessingLine />
          : step.restoring
          ? restoringLine(step.restoring, humanizeBytes)
          : wasReused(step.cache)
          ? `from cache${step.finished_at ? ` · ${clockTime(step.finished_at)}` : ""}`
          : step.started_at && step.finished_at
            ? `${elapsedOf(step, 0)} · ${clockTime(step.finished_at)}`
            : statusLabel(step.status).toLowerCase()
      }
      extra={
        <>
          {step.background && (
            <StepLine mono={false}>background · nothing waits for it</StepLine>
          )}
          {step.after && <StepLine mono={false}>after · runs whichever way the run went</StepLine>}
          {inspect && !step.recover && <InspectLine step={step} />}
        </>
      }
      // The node was served from the cache rather than run: say so where the
      // eye already is, and let a click on it explain.
      adornment={step.cache && wasReused(step.cache) && <CacheIcon report={step.cache} />}
    />
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
      {step.cache?.key && (
        <Box component="span" sx={{ fontFamily: monoFontStack, fontWeight: 400, opacity: 0.75 }}>
          {" · "}
          {step.cache.key.slice(0, 8)}
        </Box>
      )}
    </Box>
  );
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
  else if (step.after) badges.push("↪ after");
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
          <Tooltip title="The env files this step's workflow names (env_file). The run searches them ahead of every workspace's .env, and every step shares what the search finds.">
            <Typography variant="caption" color="text.secondary">
              env files
            </Typography>
          </Tooltip>
          <Typography variant="caption" sx={{ fontFamily: monoFontStack, wordBreak: "break-all" }}>
            {step.env_files.join(", ")}
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
          deps.inputs.length === 0 ? (
            "no input files declared"
          ) : (
            <>
              {deps.input_files} file(s), {humanizeBytes(deps.input_bytes)} —{" "}
              <ShortList items={deps.inputs} />
            </>
          )
        }
        title={deps.exclude.length > 0 ? `excluding ${deps.exclude.join(", ")}` : undefined}
      />
      {deps.cargo && deps.cargo.length > 0 && (
        <DepRow
          label="cargo"
          value={<ShortList items={deps.cargo} />}
          title="Packages cache.cargo resolved this target's files from"
        />
      )}
      <DepRow
        label="writes"
        value={
          deps.outputs.length === 0 ? (
            "no output files declared, so nothing could be restored"
          ) : (
            <>
              {deps.output_files} file(s), {humanizeBytes(deps.output_bytes)} —{" "}
              <ShortList items={deps.outputs} />
            </>
          )
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
      {deps.cached && deps.undeclared_reads && deps.undeclared_reads.length > 0 && (
        <Typography variant="caption" color="warning.main">
          ⚠ rustc reports the last build reading <ShortList items={deps.undeclared_reads} />, outside
          its cache inputs, so changing {deps.undeclared_reads.length === 1 ? "it" : "them"}{" "}
          would not invalidate this target.
        </Typography>
      )}
      {deps.outputs_in_inputs && deps.outputs_in_inputs.length > 0 && (
        <Typography variant="caption" color="warning.main">
          ⚠ {deps.outputs_in_inputs.length} file(s) match both its inputs and its outputs, so
          they are treated as outputs only and left out of the key:{" "}
          <ShortList items={deps.outputs_in_inputs} />. Narrow cache.inputs, or add them to
          exclude.
        </Typography>
      )}
      {(deps.unmatched_inputs ?? []).map((miss) => (
        <Typography key={miss.glob} variant="caption" color="warning.main">
          ⚠ input <code>{miss.glob}</code> <Prose text={miss.reason} />.
        </Typography>
      ))}
      {deps.cached && undeclared.length > 0 && (
        <Typography variant="caption" color="warning.main">
          ⚠ tests {undeclared.join(", ")} in a condition without declaring{" "}
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
      }${variable.reason ? ` · ${variable.reason}` : ""}`
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
  value: React.ReactNode;
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
  const group = envGroupOf(id);
  if (group !== null) {
    return new Set(
      workflow.env.vars
        .filter((variable) => envGroupOfVariable(variable) === group)
        .flatMap((variable) => variable.steps),
    );
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
    if (variable.steps.some((name) => lit.has(name))) {
      lit.add(envNodeId(variable.key));
      lit.add(envGroupNodeId(envGroupOfVariable(variable)));
    }
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
  stacks: Stacks,
  onPick: (stack: string, step: string) => void,
): { nodes: Node[]; edges: Edge[] } {
  const byName = new Map(workflow.steps.map((s) => [s.name, s]));

  // Where a step is drawn: its own node, or the stack its workflow file
  // became. Every edge goes through this, so a wire into any step of a stack
  // arrives at the stack.
  const nodeOf = (step: string) => {
    const stack = stacks.of.get(step);
    return stack === undefined ? step : stackNodeId(stack);
  };
  // The steps a node stands for: a stack's members, or the node itself.
  const membersOf = (id: string) => {
    const stack = stackKeyOf(id);
    return stack === null ? [id] : (stacks.members.get(stack) ?? []);
  };
  const stepIds = [...new Set(workflow.steps.map((s) => nodeOf(s.name)))];

  // Input sets are grouped once, here, so the focus and the drawing agree
  // about which steps share a node.
  const groups = inputGroups(workflow);

  const stepFocus: Focus =
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
  // The same focus, said of what is drawn: a stack is lit when any step in it
  // is, and is the focused node when the focused step is one of its own.
  const focus: Focus = {
    id: stepFocus.id === null ? null : nodeOf(stepFocus.id),
    lit: (id) => membersOf(id).some((member) => stepFocus.lit(member)),
  };

  // Every edge between two drawn nodes, once. Edges inside a stack are the
  // file's own order and aren't drawn; two steps of one stack waiting on the
  // same step upstream are one wire.
  const viewEdges: { from: string; to: string; kind: WorkflowView["edges"][number]["kind"] }[] = [];
  const seenEdges = new Set<string>();
  for (const edge of workflow.edges) {
    const from = nodeOf(edge.from);
    const to = nodeOf(edge.to);
    const key = `${from}\u0000${to}\u0000${edge.kind}`;
    if (from === to || seenEdges.has(key)) continue;
    seenEdges.add(key);
    viewEdges.push({ from, to, kind: edge.kind });
  }

  // Only `needs` edges define run order; error/retry branches are annotations
  // on top and would distort the layout if they drove depth.
  const orderEdges = viewEdges
    .filter((e) => e.kind === "needs")
    .map((e) => ({ source: e.from, target: e.to }));

  // One wire per drawn pair, for the dependency families too.
  const unique = <T extends { source: string; target: string }>(list: T[]): T[] => {
    const seen = new Set<string>();
    return list.filter((e) => {
      const key = `${e.source}\u0000${e.target}`;
      if (seen.has(key)) return false;
      seen.add(key);
      return true;
    });
  };

  // ── the dependency nodes, when they're on ────────────────────────────────
  // A variable nothing reads (a `.env` line no step uses) isn't drawn: it has
  // no edge to justify a node. The panel below the graph still lists it.
  const present = new Set(workflow.steps.map((s) => s.name));
  const shown = showEnv ? workflow.env.vars.filter((v) => v.steps.length > 0) : [];
  // Too many to draw one by one: fold them into their groups. The one being
  // looked at — picked from a group or from the search — still gets a node of
  // its own, so focusing it shows its wires rather than its group's.
  const grouping = shown.length > ENV_GROUP_LIMIT;
  const pinned = focused === null ? null : envKeyOf(focused);
  const variables = grouping ? shown.filter((v) => v.key === pinned) : shown;
  const envGroups = new Map<EnvGroup, EnvVar[]>();
  if (grouping) {
    for (const group of ENV_GROUPS) {
      const members = shown.filter((v) => envGroupOfVariable(v) === group && v.key !== pinned);
      if (members.length > 0) envGroups.set(group, members);
    }
  }
  const envEdges = unique(
    variables.flatMap((variable) =>
      variable.steps
        // A variable's step list comes from the resolved run, so guard against
        // an edge into a node this view isn't drawing.
        .filter((step) => present.has(step))
        .map((step) => ({ source: envNodeId(variable.key), target: nodeOf(step) })),
    ),
  );
  const groupEdges = unique(
    [...envGroups].flatMap(([group, members]) =>
      members
        .flatMap((variable) => variable.steps)
        .filter((step) => present.has(step))
        .map((step) => ({ source: envGroupNodeId(group), target: nodeOf(step) })),
    ),
  );

  const inputs = showFiles ? [...groups.values()] : [];
  const inputEdges = unique(
    inputs.flatMap((group) =>
      group.steps.map((step) => ({ source: inputNodeId(group.steps[0]), target: nodeOf(step) })),
    ),
  );
  const producers = showFiles
    ? workflow.steps.filter((step) => step.deps?.name && step.deps.outputs.length > 0)
    : [];
  const outputEdges = producers.map((step) => ({
    source: nodeOf(step.name),
    target: outputNodeId(step.name),
  }));

  // Recovery steps are left out of the numbering: the engine only enters them
  // from a failed step's `on_error`, so they have no position in the sequence
  // the run takes when everything works. Background tasks are left out for the
  // opposite reason — they always run, but nothing waits for them, so they hold
  // no position in the sequence either. Numbered by step, not by node: a stack
  // shows the number of the step on top of it.
  const sequence = showOrder
    ? executionOrder(
        workflow.steps.filter((s) => !s.recover).map((s) => s.name),
        workflow.edges
          .filter((e) => e.kind === "needs")
          .map((e) => ({ source: e.from, target: e.to })),
        { exclude: (id) => byName.get(id)?.background ?? false },
      )
    : new Map<string, number>();

  const ids = [
    ...stepIds,
    ...variables.map((variable) => envNodeId(variable.key)),
    ...[...envGroups.keys()].map(envGroupNodeId),
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
    ...viewEdges
      .filter((e) => e.kind !== "needs")
      .map((e) => ({
        source: e.from,
        target: e.to,
        group: e.kind,
        rank: e.kind !== "retry",
      })),
    ...envEdges.map((e) => ({ ...e, group: "env" })),
    ...groupEdges.map((e) => ({ ...e, group: "env" })),
    ...inputEdges.map((e) => ({ ...e, group: "in" })),
    ...outputEdges.map((e) => ({ ...e, group: "out" })),
  ];

  const { nodes: positioned, routes } = layeredLayout(
    ids,
    layoutEdges,
    (id) =>
      nodeData(id, byName, groups, sequence, showOrder, workflow, inspect, {
        stacks,
        envGroups,
        onPick,
      }),
    {
      // The shared run-graph spacing: columns wide enough for a capped node
      // plus a gap the wires can turn in.
      ...STEP_LAYOUT,
      heightOf: (id) => nodeHeight(id, byName, inspect, stacks),
      // Variables and input files are inputs to the run, not steps of it, so
      // they take columns of their own to the left — variables outside the
      // files, so the two kinds read as two columns rather than one pile.
      // Outputs need no pin: they hang off the step that writes them, one
      // column along, which is exactly where the depth puts them.
      // Variables go outside the input files when both are on, and take that
      // column themselves when the files are off — an empty column between the
      // inputs and the run is just a gap to scroll past.
      pin: (id) =>
        envKeyOf(id) !== null || envGroupOf(id) !== null
          ? showFiles
            ? -2
            : -1
          : id.startsWith("in::")
            ? -1
            : undefined,
      // Background tasks sit in a row underneath rather than in a column,
      // because a column would claim they gate what follows them. They don't.
      bottom: (id) => byName.get(id)?.background ?? false,
      // Workflows sharing a `wave_grouping` start in the same column. A stack
      // is its workflow file, so it lines up as one.
      align: (id) => {
        const step = byName.get(membersOf(id)[0]);
        const unit = step?.cache_id?.slice(0, step.cache_id.lastIndexOf(":"));
        return step?.wave_group && unit ? { group: step.wave_group, unit } : undefined;
      },
    },
  );

  const nodes: Node[] = positioned.map((node) => ({
    ...node,
    style: nodeStyle(
      node.id,
      byName,
      theme,
      focus,
      inspect,
      node.data?.variable as EnvVar | undefined,
      stacks,
      envGroups,
    ),
  }));
  const route = (source: string, target: string) => ({
    route: routes.get(routeKey(source, target)),
  });

  // ── the edges ────────────────────────────────────────────────────────────
  const edges: Edge[] = viewEdges.map((edge) => {
    // An edge survives the dimming only if both ends did — so a focused node's
    // chain keeps the order between its steps, and everything else recedes.
    const on = focus.lit(edge.from) && focus.lit(edge.to);
    // Inspect mode: the edge along which an upstream step held this one back
    // from its cache. That's the relationship being asked about, so it's the
    // one drawn loudest.
    const upstream = membersOf(edge.from);
    const blocking =
      inspect &&
      edge.kind === "needs" &&
      membersOf(edge.to).some((to) =>
        (byName.get(to)?.cache?.blocked_by ?? []).some((from) => upstream.includes(from)),
      );
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
      id: `${edge.from}->${edge.to}-${edge.kind}`,
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
      animated: upstream.some((name) => byName.get(name)?.status === "running"),
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
  for (const [group, members] of envGroups) {
    const colour = envGroupColour(group, members, theme);
    for (const edge of groupEdges.filter((e) => e.source === envGroupNodeId(group))) {
      edges.push(...dependencyEdge(edge.source, edge.target, colour));
    }
  }
  for (const edge of inputEdges) {
    edges.push(...dependencyEdge(edge.source, edge.target, theme.palette.info.main));
  }
  for (const step of producers) {
    edges.push(
      ...dependencyEdge(nodeOf(step.name), outputNodeId(step.name), theme.palette.success.main, {
        // A finished step's outputs are on disk; a running one's are being
        // written as you watch.
        animated: step.status === "running",
      }),
    );
  }

  return { nodes, edges };
}

/**
 * Which workflow files to stack, and the step on top of each.
 *
 * A workflow file is a sub-workspace's contribution to the graph, so its steps
 * are the ones sharing a `workspace`. Recovery steps and background tasks stay
 * nodes of their own: one is a branch a failure jumps to, the other gates
 * nothing, and folding either into the stack would hide the wire that says so.
 *
 * Nothing is stacked when the graph is a single workflow file — the stack
 * would be the whole graph, one node with everything behind a menu.
 *
 * On top: the step that's been picked (`selected`, then the last choice made
 * from the stack's own list), else the one worth looking at — running, then
 * failed, then the first still to run, then the last.
 */
function workflowStacks(
  workflow: WorkflowView,
  chosen: Record<string, string>,
  selected: string | null,
): Stacks {
  const files = new Map<string, StepView[]>();
  for (const step of workflow.steps) {
    if (step.recover || step.background) continue;
    const key = step.workspace ?? "";
    files.set(key, [...(files.get(key) ?? []), step]);
  }
  if (files.size < 2) return NO_STACKS;

  const stacks: Stacks = { members: new Map(), of: new Map(), top: new Map() };
  for (const [key, steps] of files) {
    if (steps.length < 2) continue;
    const names = steps.map((s) => s.name);
    stacks.members.set(key, names);
    for (const name of names) stacks.of.set(name, key);
    const top =
      [selected, chosen[key]].find((name) => name && names.includes(name)) ??
      steps.find((s) => ["running", "processing", "restoring"].includes(s.status))?.name ??
      steps.find((s) => s.status === "failed")?.name ??
      steps.find((s) => s.status === "pending")?.name ??
      names[names.length - 1];
    stacks.top.set(key, top);
  }
  return stacks;
}

/**
 * What a stack's border says: the worst thing happening in it. A failure in a
 * step underneath must not hide behind a successful one on top.
 */
function stackStatus(steps: StepView[], top: StepView): StepStatus {
  for (const status of ["failed", "running", "processing", "restoring", "stopped"] as StepStatus[]) {
    if (steps.some((s) => s.status === status)) return status;
  }
  return top.status;
}

/**
 * What a workflow file says about a variable: `required_env` makes it
 * required, its `env` list makes it optional. Anything else is a variable the
 * steps test in a condition, which the workflow never declared either way.
 */
type EnvRole = "required" | "optional" | null;

function envRole(variable: EnvVar): EnvRole {
  if (variable.required) return "required";
  if (variable.declared) return "optional";
  return null;
}

/**
 * The colour a variable is drawn in: unset-and-required is a problem, and
 * otherwise what the workflow declared it as decides — so required and
 * optional variables can be told apart at a glance, before reading a label.
 */
function envColour(variable: EnvVar, theme: Theme): string {
  if (unsetProblem(variable)) return theme.palette.error.main;
  // Gold for required and sky for optional: two hues that can't be mistaken
  // for each other, in either theme. (The theme's `secondary` is the same gold
  // as `warning`, so it can't stand for a third kind of variable here.)
  const role = envRole(variable);
  if (role === "required") return theme.palette.warning.main;
  if (role === "optional") return theme.palette.info.main;
  // A custom argument: typed for this run only.
  if (variable.origin === "argument") return theme.palette.primary.light;
  // An undeclared variable nobody set is a fact, not a fault: drawn quietly.
  if (variable.origin === "unset") return theme.palette.text.disabled;
  // Read by a step without the workflow declaring it either way: neutral.
  return theme.palette.text.secondary;
}

/** A group of variables' colour: its members' role, or red if any of them is
 *  a required variable nothing set. */
function envGroupColour(group: EnvGroup, variables: EnvVar[], theme: Theme): string {
  if (variables.some(unsetProblem)) return theme.palette.error.main;
  if (group === "required") return theme.palette.warning.main;
  if (group === "optional") return theme.palette.info.main;
  return theme.palette.text.secondary;
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
  view: {
    stacks: Stacks;
    envGroups: Map<EnvGroup, EnvVar[]>;
    onPick: (stack: string, step: string) => void;
  },
): Record<string, unknown> {
  const stack = stackKeyOf(id);
  if (stack !== null) {
    const steps = (view.stacks.members.get(stack) ?? [])
      .map((name) => byName.get(name))
      .filter((s): s is StepView => s !== undefined);
    const top = byName.get(view.stacks.top.get(stack) ?? "") ?? steps[0];
    return {
      label: (
        <StackNodeLabel
          steps={steps}
          top={top}
          order={showOrder ? (sequence.get(top.name) ?? null) : null}
          inspect={inspect}
          onPick={(name) => view.onPick(stack, name)}
        />
      ),
      status: stackStatus(steps, top),
    };
  }

  const envGroup = envGroupOf(id);
  if (envGroup !== null) {
    return {
      label: <EnvGroupLabel group={envGroup} variables={view.envGroups.get(envGroup) ?? []} />,
      status: "pending" as StepStatus,
    };
  }

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
      variable,
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
  variable?: EnvVar,
  stacks: Stacks = NO_STACKS,
  envGroups: Map<EnvGroup, EnvVar[]> = new Map(),
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
    height: nodeHeight(id, byName, inspect, stacks),
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

  const stack = stackKeyOf(id);
  if (stack !== null) {
    const steps = (stacks.members.get(stack) ?? [])
      .map((name) => byName.get(name))
      .filter((s): s is StepView => s !== undefined);
    const top = byName.get(stacks.top.get(stack) ?? "") ?? steps[0];
    const border =
      inspect && !top.recover
        ? cacheTone(top.cache, theme)
        : top.after
          ? afterColor(stackStatus(steps, top), theme)
          : statusColor(stackStatus(steps, top), theme);
    const base = stepNodeStyle({
      theme,
      height: common.height,
      opacity: common.opacity,
      picked,
      border,
    });
    // The cards underneath: offset outlines behind the node, one for a stack
    // of two and two for anything deeper — enough to read as a pile without
    // claiming to count it. The count is in the strip across the top.
    const card = (offset: number) => [
      `${offset}px ${offset}px 0 -2px ${theme.palette.background.paper}`,
      `${offset}px ${offset}px 0 0 ${border}99`,
    ];
    return {
      ...base,
      flexDirection: "column",
      alignItems: "stretch",
      padding: "0 12px 6px",
      boxShadow: [
        ...(picked ? [`0 0 0 3px ${theme.palette.secondary.main}55`] : []),
        ...card(4),
        ...(steps.length > 2 ? card(8) : []),
      ].join(", "),
      overflow: "visible",
    };
  }

  const group = envGroupOf(id);
  if (group !== null) {
    const colour = envGroupColour(group, envGroups.get(group) ?? [], theme);
    return {
      ...common,
      border: `${picked || group === "required" ? 2 : 1}px ${
        picked || group === "required" ? "solid" : "dashed"
      } ${colour}`,
      borderRadius: 18,
      fontSize: 11,
      padding: "0 10px",
      cursor: "pointer",
    };
  }

  const step = byName.get(id);
  if (step) {
    return stepNodeStyle({
      theme,
      height: common.height,
      opacity: common.opacity,
      picked,
      dashed: step.recover,
      border:
        // In inspect mode the border is the cache's verdict, not the step's:
        // the status is still the glyph beside the name.
        inspect && !step.recover
          ? cacheTone(step.cache, theme)
          : step.after
            ? afterColor(step.status, theme)
            : // Reused is as good an outcome as ran-and-passed.
              wasReused(step.cache)
              ? theme.palette.success.main
              : statusColor(step.status, theme),
    });
  }

  // Dependency nodes: dashed like the edges that leave them, because a file set
  // or a variable is a precondition rather than something that ran. The
  // selected one goes solid — it's the subject now, not an aside.
  const colour = isFileNode(id)
    ? id.startsWith("in::")
      ? theme.palette.info.main
      : theme.palette.success.main
    : variable
      ? envColour(variable, theme)
      : theme.palette.secondary.main;
  // A required variable is drawn solid and heavier: the run can't start
  // without it, so it isn't an aside the way an optional one is.
  const required = variable !== undefined && envRole(variable) === "required";

  return {
    ...common,
    border: `${picked || required ? 2 : 1}px ${picked || required ? "solid" : "dashed"} ${colour}`,
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

/** What a role is called on a node, and what hovering it explains. */
const ENV_ROLE: Record<"required" | "optional", { tag: string; why: string }> = {
  required: {
    tag: "required",
    why: "required_env — the run refuses to start without it",
  },
  optional: {
    tag: "optional",
    why: "env — the workflow uses it; its value comes from .env and may be unset",
  },
};

/**
 * A variable on the canvas: its name, and underneath it the value — with a
 * glyph and a tag in the variable's colour when the workflow file declared it,
 * `✱` for required and `?` for optional.
 */
function EnvNodeLabel({ variable }: { variable: EnvVar }) {
  const theme = useTheme();
  const role = envRole(variable);
  const colour = envColour(variable, theme);
  const Icon = role === "required" ? RequiredIcon : OptionalIcon;
  const title = [
    variable.key,
    role && ENV_ROLE[role].why,
    variable.purpose,
  ]
    .filter(Boolean)
    .join(" — ");
  return (
    <Box sx={{ fontFamily: monoFontStack, minWidth: 0, flexGrow: 1 }} title={title}>
      <Box sx={{ display: "flex", alignItems: "center", gap: 0.5, minWidth: 0 }}>
        {role && <Icon sx={{ fontSize: 12, color: colour, flexShrink: 0 }} />}
        <Box sx={{ ...oneLine(LINE.name), fontWeight: 700, minWidth: 0 }}>{variable.key}</Box>
      </Box>
      <Box sx={{ display: "flex", alignItems: "baseline", gap: 0.75, minWidth: 0 }}>
        <Box
          sx={{
            ...oneLine(LINE.name),
            opacity: 0.75,
            minWidth: 0,
            fontStyle: variable.value === null ? "italic" : "normal",
          }}
        >
          {envValueText(variable)}
        </Box>
        {role && (
          <Box
            component="span"
            sx={{
              ml: "auto",
              flexShrink: 0,
              fontSize: 9,
              fontWeight: 700,
              letterSpacing: "0.04em",
              textTransform: "uppercase",
              color: colour,
            }}
          >
            {ENV_ROLE[role].tag}
          </Box>
        )}
      </Box>
    </Box>
  );
}

/**
 * A group of variables on the canvas: how many, of which kind, and the first
 * few names — enough to recognise the group without opening it.
 */
function EnvGroupLabel({ group, variables }: { group: EnvGroup; variables: EnvVar[] }) {
  const theme = useTheme();
  const colour = envGroupColour(group, variables, theme);
  const Icon = group === "required" ? RequiredIcon : OptionalIcon;
  const unset = variables.filter(unsetProblem).length;
  return (
    <Box
      sx={{ fontFamily: monoFontStack, minWidth: 0, flexGrow: 1 }}
      title={`${variables.length} ${ENV_GROUP_INFO[group].label} variables — ${
        ENV_GROUP_INFO[group].why
      }. Click to search them.\n\n${variables.map((v) => v.key).join("\n")}`}
    >
      <Box sx={{ display: "flex", alignItems: "center", gap: 0.5, minWidth: 0 }}>
        {group !== "undeclared" && <Icon sx={{ fontSize: 12, color: colour, flexShrink: 0 }} />}
        <Box sx={{ ...oneLine(LINE.name), fontWeight: 700, minWidth: 0 }}>
          {variables.length} {ENV_GROUP_INFO[group].label}
        </Box>
        <SearchIcon sx={{ fontSize: 13, ml: "auto", opacity: 0.6, flexShrink: 0 }} />
      </Box>
      <Box sx={{ ...oneLine(LINE.name), opacity: 0.75, minWidth: 0 }}>
        {unset > 0 ? `${unset} unset · ` : ""}
        {variables.map((v) => v.key).join(", ")}
      </Box>
    </Box>
  );
}

/**
 * A stacked workflow file: a strip across the top saying which of its steps
 * is showing, which opens the list of all of them, and the step on top drawn
 * the way any step is.
 *
 * Clicks inside the strip and its menu stop at the menu. The menu is portalled
 * out of the node, but React still bubbles its events through the node, and a
 * click reaching react-flow would also select the stack.
 */
function StackNodeLabel({
  steps,
  top,
  order,
  inspect,
  onPick,
}: {
  steps: StepView[];
  top: StepView;
  order: number | null;
  inspect: boolean;
  onPick: (name: string) => void;
}) {
  const [anchor, setAnchor] = useState<HTMLElement | null>(null);
  const stop = (event: React.SyntheticEvent) => event.stopPropagation();
  const index = steps.findIndex((s) => s.name === top.name);
  return (
    <Box sx={{ display: "flex", flexDirection: "column", minWidth: 0, flexGrow: 1 }}>
      <Box
        component="button"
        type="button"
        className="nodrag nopan"
        title="Choose the step shown on top of this workflow file"
        onClick={(event: React.MouseEvent<HTMLElement>) => {
          stop(event);
          setAnchor(event.currentTarget);
        }}
        sx={{
          all: "unset",
          boxSizing: "border-box",
          height: STACK_HEADER,
          display: "flex",
          alignItems: "center",
          gap: 0.5,
          mx: -1.5,
          px: 1.5,
          fontSize: 10,
          color: "text.secondary",
          borderBottom: 1,
          borderColor: "divider",
          cursor: "pointer",
          flexShrink: 0,
          "&:hover": { color: "text.primary", bgcolor: "action.hover" },
        }}
      >
        <LayersIcon sx={{ fontSize: 12 }} />
        <Box component="span" sx={{ fontWeight: 600 }}>
          step {index + 1} of {steps.length}
        </Box>
        <ArrowDropDownIcon sx={{ fontSize: 16, ml: "auto" }} />
      </Box>
      <Box sx={{ display: "flex", alignItems: "center", flexGrow: 1, minWidth: 0 }}>
        <NodeLabel step={top} order={order} inspect={inspect} />
      </Box>
      <Menu
        anchorEl={anchor}
        open={anchor !== null}
        onClose={() => setAnchor(null)}
        onClick={stop}
        onMouseDown={stop}
      >
        <ListSubheader sx={{ lineHeight: "32px" }}>
          {top.workspace ? `${top.workspace} · ` : ""}
          {steps.length} steps, in order
        </ListSubheader>
        {steps.map((step, i) => (
          <MenuItem
            key={step.name}
            selected={step.name === top.name}
            dense
            onClick={() => {
              setAnchor(null);
              onPick(step.name);
            }}
          >
            <Stack direction="row" spacing={1} alignItems="center" sx={{ minWidth: 0 }}>
              <Typography variant="caption" color="text.secondary" sx={{ width: 16 }}>
                {i + 1}
              </Typography>
              <StatusIcon status={step.status} />
              <Typography variant="body2" sx={{ fontFamily: monoFontStack }}>
                {step.name}
              </Typography>
              {step.description && (
                <Typography variant="caption" color="text.secondary" noWrap sx={{ maxWidth: 260 }}>
                  {step.description}
                </Typography>
              )}
            </Stack>
          </MenuItem>
        ))}
      </Menu>
    </Box>
  );
}

/**
 * The graph header's variable search, shown when the variables are drawn as
 * their groups. Picking one draws it on its own and lights the steps it
 * feeds — the same as picking it from its group.
 */
function VariableSearch({
  variables,
  onPick,
}: {
  variables: EnvVar[];
  onPick: (key: string) => void;
}) {
  const options = [...variables].sort(
    (a, b) =>
      ENV_GROUPS.indexOf(envGroupOfVariable(a)) - ENV_GROUPS.indexOf(envGroupOfVariable(b)) ||
      a.key.localeCompare(b.key),
  );
  return (
    <Autocomplete
      size="small"
      options={options}
      groupBy={(v) => ENV_GROUP_INFO[envGroupOfVariable(v)].label}
      getOptionLabel={(v) => v.key}
      value={null}
      blurOnSelect
      clearOnBlur
      onChange={(_, v) => v && onPick(v.key)}
      renderOption={(props, v) => (
        <li {...props} key={v.key}>
          <Box sx={{ minWidth: 0 }}>
            <Typography variant="body2" sx={{ fontFamily: monoFontStack }} noWrap>
              {v.key}
            </Typography>
            <Typography variant="caption" color="text.secondary" noWrap component="div">
              {envValueText(v)}
            </Typography>
          </Box>
        </li>
      )}
      renderInput={(params) => (
        <TextField
          {...params}
          placeholder={`Search ${variables.length} variables`}
          InputProps={{
            ...params.InputProps,
            startAdornment: (
              <InputAdornment position="start">
                <SearchIcon sx={{ fontSize: 16 }} />
              </InputAdornment>
            ),
          }}
        />
      )}
      sx={{ width: 200, flexShrink: 0, "& .MuiInputBase-root": { py: 0, fontSize: 12 } }}
    />
  );
}

/**
 * A focused group of variables: every one in it, searchable. Picking one draws
 * it as its own node, so its wires — not the group's — are the lit ones.
 */
function EnvGroupNote({
  group,
  variables,
  onPick,
  onClear,
}: {
  group: EnvGroup;
  variables: EnvVar[];
  onPick: (key: string) => void;
  onClear: () => void;
}) {
  const [query, setQuery] = useState("");
  const needle = query.trim().toLowerCase();
  const matches = variables.filter(
    (v) =>
      needle === "" ||
      v.key.toLowerCase().includes(needle) ||
      envValueText(v).toLowerCase().includes(needle),
  );
  return (
    <Stack spacing={1} sx={{ py: 0.5 }}>
      <Stack direction="row" spacing={1} alignItems="center" flexWrap="wrap" useFlexGap>
        <Typography variant="body2" sx={{ fontWeight: 600 }}>
          {variables.length} {ENV_GROUP_INFO[group].label} variables
        </Typography>
        <Typography variant="caption" color="text.secondary" sx={{ flexGrow: 1 }}>
          {ENV_GROUP_INFO[group].why}. Click one to draw it on its own.
        </Typography>
        <TextField
          size="small"
          autoFocus
          placeholder="Search by name or value"
          value={query}
          onChange={(event) => setQuery(event.target.value)}
          InputProps={{
            startAdornment: (
              <InputAdornment position="start">
                <SearchIcon sx={{ fontSize: 16 }} />
              </InputAdornment>
            ),
          }}
          sx={{ width: 240, "& .MuiInputBase-root": { fontSize: 12 } }}
        />
        <Button size="small" onClick={onClear}>
          Clear
        </Button>
      </Stack>
      <Stack direction="row" spacing={0.75} flexWrap="wrap" useFlexGap>
        {matches.map((variable) => (
          <Box
            key={variable.key}
            component="span"
            onClick={() => onPick(variable.key)}
            sx={{ cursor: "pointer" }}
          >
            <EnvVarChip variable={variable} />
          </Box>
        ))}
        {matches.length === 0 && (
          <Typography variant="caption" color="text.secondary">
            No {ENV_GROUP_INFO[group].label} variable matches “{query}”.
          </Typography>
        )}
      </Stack>
    </Stack>
  );
}

/** The colour a step's status is drawn in — the graph's borders, the minimap. */
function statusColor(status: StepStatus | string, theme: Theme): string {
  return statusColour(status, theme);
}

/**
 * An after action's border: blue, so the clean-up at the end of a run reads
 * as its own kind of step — except while it runs or once it fails, which are
 * worth the status colour's attention.
 */
function afterColor(status: StepStatus | string, theme: Theme): string {
  return status === "running" || status === "failed"
    ? statusColor(status, theme)
    : theme.palette.info.main;
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
