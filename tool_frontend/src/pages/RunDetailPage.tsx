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
  missingEnvFrom,
  useChoose,
  useRerunRun,
  useStopRun,
  undeclaredEnv,
  type WorkflowView,
  type RunState,
  type StepStatus,
  type StepView,
  type TargetDeps,
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
} from "../components/EnvVars";
import {
  ORTHOGONAL_EDGE,
  executionOrder,
  isWaypoint,
  layeredLayout,
  routeSegments,
} from "../components/layout";
import { StatusIcon, statusColour, statusLabel } from "../components/StatusIcon";
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
    () => buildFlow(workflow, theme, showOrder, showEnv, showFiles, focused),
    [workflow, theme, showOrder, showEnv, showFiles, focused],
  );
  const step = workflow.steps.find((s) => s.name === selectedStep);

  return (
    <>
      <Stack
        direction="row"
        spacing={1}
        sx={{ mb: 1.5 }}
        flexWrap="wrap"
        useFlexGap
        alignItems="center"
      >
        {workflow.stages.map((stage) => (
          <Chip
            key={stage.name}
            size="small"
            variant="outlined"
            color={stageColor(stage.status)}
            label={`${stage.name}: ${stage.status}`}
          />
        ))}
        <Box sx={{ flexGrow: 1 }} />
        <Tooltip title="Draw each environment variable as what it is — a dependency, feeding into every step that reads it. Values come from the run's resolved environment.">
          <FormControlLabel
            control={
              <Switch
                size="small"
                checked={showEnv}
                onChange={(_, checked) => {
                  setShowEnv(checked);
                  // Focusing a node and then hiding it would dim the graph with
                  // nothing left lit to explain why.
                  if (!checked && focused !== null && envKeyOf(focused) !== null) {
                    setFocused(null);
                  }
                }}
              />
            }
            label={
              <Typography variant="caption" color="text.secondary">
                Environment
              </Typography>
            }
            sx={{ mr: 0 }}
          />
        </Tooltip>
        <Tooltip title="Draw the files each target reads and writes as nodes of their own: inputs feeding in from the left, outputs produced on the right. These are the file sets the cache keys on, so this is the graph the caching decision is actually made from.">
          <FormControlLabel
            control={
              <Switch
                size="small"
                checked={showFiles}
                onChange={(_, checked) => {
                  setShowFiles(checked);
                  if (!checked && focused !== null && isFileNode(focused)) {
                    setFocused(null);
                  }
                }}
              />
            }
            label={
              <Typography variant="caption" color="text.secondary">
                Files
              </Typography>
            }
            sx={{ mr: 0 }}
          />
        </Tooltip>
        <Tooltip title="Number each node with its place in the run's sequence. Recovery steps aren't numbered — they only run if something fails.">
          <FormControlLabel
            control={
              <Switch
                size="small"
                checked={showOrder}
                onChange={(_, checked) => setShowOrder(checked)}
              />
            }
            label={
              <Typography variant="caption" color="text.secondary">
                Execution order
              </Typography>
            }
            sx={{ mr: 0 }}
          />
        </Tooltip>
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

      {/*
        Graph and logs side by side once there is width for both, stacked below
        that. Watching a step run means watching two things — which node is
        lit, and what it is printing — and stacked panes put one of them off
        the bottom of the screen at exactly the moment both matter. The
        breakpoint is `lg` because the graph needs real width before splitting
        it helps: narrower than that, a half-width flowchart is worse than a
        full-width one above the logs.
      */}
      <Box
        sx={{
          display: "grid",
          gap: 2,
          alignItems: "stretch",
          gridTemplateColumns: { xs: "1fr", lg: "minmax(0, 1fr) minmax(0, 1fr)" },
        }}
      >
        <Box sx={{ minWidth: 0, display: "flex", flexDirection: "column", gap: 1 }}>
          <GraphCanvas
            nodes={nodes}
            edges={edges}
            height={PANE_HEIGHT}
            // Turning the environment column on and off changes the graph's
            // extent, so the view has to be re-fitted around it.
            fitKey={`${showEnv ? "env" : ""}${showFiles ? "+files" : ""}` || "steps-only"}
            // Clicking a node focuses the graph on what it depends on. For a
            // step that means the chain it waits for; for a dependency node — a
            // variable, or a set of files read or written — it means the steps
            // that touch it. Clicking the same node again, or the canvas, puts
            // the whole graph back.
            onNodeClick={(_, node) => {
              // A waypoint is drawn inert, but nothing downstream should depend
              // on that: it names no step, and focusing it would dim the graph
              // with nothing lit to explain why.
              if (isWaypoint(node.id)) return;
              setFocused((current) => (current === node.id ? null : node.id));
              if (!isDependencyNode(node.id)) onSelectStep(node.id);
              else onSelectStep(null);
            }}
            onPaneClick={() => setFocused(null)}
            nodeColor={(node) => statusColor(node.data?.status as StepStatus, theme)}
          />

          {focused !== null && (
            <FocusNote
              workflow={workflow}
              focused={focused}
              onClear={() => setFocused(null)}
            />
          )}
        </Box>

        <Box sx={{ minWidth: 0, display: "flex", flexDirection: "column", minHeight: 0 }}>
          <Typography variant="h3" sx={{ mb: 1 }}>
            {step ? `${step.name} logs` : "Workflow logs"}
          </Typography>
          {step && (
            <StepDetails
              step={step}
              env={workflow.env.vars.filter((variable) => variable.steps.includes(step.name))}
            />
          )}
          {step?.action && (
            <Typography
              variant="caption"
              color="text.secondary"
              sx={{ display: "block", mb: 1, fontFamily: monoFontStack }}
            >
              {step.action}
            </Typography>
          )}
          <LogView
            // Keyed by what is being shown, so switching steps starts the new
            // log at its end rather than inheriting the old one's scroll.
            key={step ? step.name : "__workflow__"}
            lines={step ? step.logs : workflow.logs}
            dropped={(step ? step.dropped_logs : workflow.dropped_logs) ?? 0}
            height={PANE_HEIGHT}
            title={step ? `${step.name} — run #${runId}` : `${workflow.name} — run #${runId}`}
          />
          {step && (
            <Button
              size="small"
              sx={{ mt: 1, alignSelf: "flex-start" }}
              onClick={() => onSelectStep(null)}
            >
              Show all workflow logs
            </Button>
          )}
        </Box>
      </Box>

      <Box sx={{ mt: 2 }}>
        <EnvPanel report={workflow.env} title="Environment this run started with" />
      </Box>
    </>
  );
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
function NodeLabel({ step, order }: { step: StepView; order: number | null }) {
  // The id is "<workspace>:<step>" (or "<workspace>:<workflow>:<step>"), and
  // repeating the workspace in both lines just wastes the node's width.
  const short =
    step.workspace && step.name.startsWith(`${step.workspace}:`)
      ? step.name.slice(step.workspace.length + 1)
      : step.name;

  return (
    <Stack direction="row" spacing={0.75} alignItems="center" sx={{ minWidth: 0 }}>
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
      <Box sx={{ minWidth: 0, textAlign: "left", overflowWrap: "anywhere" }}>
        {step.workspace && (
          <Box sx={{ fontSize: 10, opacity: 0.75, fontFamily: monoFontStack }}>
            {step.workspace}
          </Box>
        )}
        <Box sx={{ fontWeight: 600 }}>{short}</Box>
        {step.background && (
          <Box sx={{ fontSize: 10, opacity: 0.7 }}>background · nothing waits for it</Box>
        )}
      </Box>
    </Stack>
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
function StepDetails({ step, env }: { step: StepView; env: EnvVar[] }) {
  const badges: string[] = [];
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
    <Stack spacing={1} sx={{ mb: 1 }}>
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

  // A focused *step* says what it waits for, which is the answer the dimming
  // is drawing. Held separately from the dependency-node cases below because
  // it is a different sentence about a different kind of thing.
  const step = isDependencyNode(focused)
    ? null
    : (workflow.steps.find((s) => s.name === focused) ?? null);

  const key = envKeyOf(focused);
  const variable = key === null ? null : (workflow.env.vars.find((v) => v.key === key) ?? null);
  const writer = outputStepOf(focused);
  const producer = writer === null ? null : (workflow.steps.find((s) => s.name === writer) ?? null);
  const group = groups.get(focused) ?? null;

  if (step) {
    const upstream = [...dependencyClosure(workflow, focused)].filter(
      (id) => id !== focused && !isDependencyNode(id),
    );
    return (
      <Stack
        direction="row"
        spacing={1}
        sx={{ mt: 1 }}
        alignItems="center"
        flexWrap="wrap"
        useFlexGap
      >
        <Chip
          size="small"
          variant="outlined"
          color="secondary"
          label={step.name}
          sx={{ fontFamily: monoFontStack, maxWidth: 420 }}
        />
        <Typography variant="caption" color="text.secondary">
          {upstream.length === 0
            ? "depends on nothing — it can start immediately"
            : `waits for ${upstream.length} step${upstream.length === 1 ? "" : "s"}: ${upstream.join(", ")}`}
        </Typography>
        <Button size="small" onClick={onClear}>
          Clear
        </Button>
      </Stack>
    );
  }

  // The run's shape can change under a focus — a workflow recompiles, a step
  // is filtered out. A node that isn't there any more has nothing to say.
  if (!variable && !producer && !group) return null;

  const subject = variable
    ? null
    : producer
      ? producer.deps.outputs.join(", ")
      : group!.deps.inputs.join(", ");

  const detail = variable
    ? variable.steps.length > 0
      ? `read by ${variable.steps.join(", ")}`
      : "no step reads this"
    : producer
      ? `${producer.deps.output_files} file(s), ${humanizeBytes(
          producer.deps.output_bytes,
        )} — written by ${producer.name}`
      : `${group!.deps.input_files} file(s), ${humanizeBytes(
          group!.deps.input_bytes,
        )} — read by ${group!.steps.join(", ")}`;

  return (
    <Stack
      direction="row"
      spacing={1}
      sx={{ mt: 1 }}
      alignItems="center"
      flexWrap="wrap"
      useFlexGap
    >
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

  const { nodes: positioned, routes } = layeredLayout(
    ids,
    [...orderEdges, ...envEdges, ...inputEdges, ...outputEdges],
    (id) => nodeData(id, byName, groups, sequence, showOrder, workflow),
    {
      // A column has to be wider than the widest node it can hold, or a long
      // step name grows its node into the next column and the edges arriving
      // there run over it. `NODE_WIDTH` caps the node; this leaves a gap the
      // wires can turn in.
      columnWidth: NODE_WIDTH + 130,
      rowHeight: 84,
      nodeHeight: 52,
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

  const nodes: Node[] = positioned.map((node) => {
    // A waypoint is a bend in a wire: it arrives already styled to be
    // invisible, and painting a border on it would draw a box in mid-air.
    if (isWaypoint(node.id)) return node;
    return { ...node, style: nodeStyle(node.id, byName, theme, focus) };
  });

  // ── the edges ────────────────────────────────────────────────────────────
  const edges: Edge[] = workflow.edges.flatMap((edge, index) => {
    // An edge survives the dimming only if both ends did — so a focused node's
    // chain keeps the order between its steps, and everything else recedes.
    const on = focus.lit(edge.from) && focus.lit(edge.to);
    const stroke =
      edge.kind === "error"
        ? theme.palette.error.main
        : edge.kind === "retry"
          ? theme.palette.warning.main
          : // `divider` is a hairline meant to separate panels, and on a graph
            // with fifty edges it reads as grey noise rather than as fifty
            // statements about what waits for what. This is the same neutral
            // held to a contrast you can actually trace with your eye.
            theme.palette.text.secondary;

    // A `needs` edge that skips columns is drawn as the chain of segments the
    // layout reserved a lane for, rather than as one stroke over whatever
    // happens to be in between. Everything else is a single segment.
    return routeSegments(routes, edge.from, edge.to).map((segment, part) => ({
      ...ORTHOGONAL_EDGE,
      id: `${edge.from}->${edge.to}-${index}-${part}`,
      source: segment.source,
      target: segment.target,
      // The label belongs on the first segment, where the edge leaves the step
      // it is a statement about.
      label: edge.kind === "needs" || !segment.first ? undefined : edge.kind,
      // react-flow's label is white-on-white in dark mode; these follow the
      // page instead.
      labelStyle: { fill: theme.palette.text.secondary, fontSize: 10 },
      labelBgStyle: { fill: theme.palette.background.paper },
      labelBgPadding: [4, 2] as [number, number],
      labelBgBorderRadius: 4,
      animated: byName.get(edge.from)?.status === "running",
      // Lifted above its neighbours while it is part of what you asked about.
      zIndex: on && focus.id !== null ? 1 : 0,
      // One arrowhead, at the end: a marker on each segment would plant arrows
      // in the empty space where the lane bends.
      markerEnd: segment.last ? { ...ORTHOGONAL_EDGE.markerEnd, color: stroke } : undefined,
      style: {
        stroke,
        strokeWidth: on && focus.id !== null ? 2 : 1.4,
        strokeDasharray: edge.kind === "needs" ? undefined : "5 4",
        opacity: on ? 1 : DIMMED,
      },
    }));
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
    return routeSegments(routes, source, target).map((segment, part) => ({
      ...ORTHOGONAL_EDGE,
      id: `dep:${source}->${target}-${part}`,
      source: segment.source,
      target: segment.target,
      animated: picked || (extra.animated ?? false),
      markerEnd: segment.last ? { ...ORTHOGONAL_EDGE.markerEnd, color: colour } : undefined,
      style: {
        stroke: colour,
        strokeDasharray: "2 4",
        strokeWidth: picked ? 2 : 1,
        opacity: lit ? 0.75 : DIMMED,
      },
    }));
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
  return variable.origin === "unset" ? theme.palette.error.main : theme.palette.secondary.main;
}

/** What each kind of node puts on the canvas. */
function nodeData(
  id: string,
  byName: Map<string, StepView>,
  groups: Map<string, { deps: TargetDeps; steps: string[] }>,
  sequence: Map<string, number>,
  showOrder: boolean,
  workflow: WorkflowView,
): Record<string, unknown> {
  const step = byName.get(id);
  if (step) {
    return {
      label: <NodeLabel step={step} order={showOrder ? (sequence.get(id) ?? null) : null} />,
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
        picked ? theme.palette.secondary.main : statusColor(step.status, theme)
      }`,
      fontSize: 12,
      padding: "6px 12px",
      minWidth: 150,
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
    padding: "5px 10px",
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
    <Box sx={{ textAlign: "left" }}>
      <Typography variant="caption" sx={{ display: "block", color: "text.secondary" }}>
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
          overflow: "hidden",
          textOverflow: "ellipsis",
          whiteSpace: "nowrap",
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
    <Box sx={{ fontFamily: monoFontStack, lineHeight: 1.35 }}>
      <Box sx={{ fontWeight: 700 }}>{variable.key}</Box>
      <Box
        sx={{
          opacity: 0.75,
          fontStyle: variable.value === null ? "italic" : "normal",
          maxWidth: 200,
          overflow: "hidden",
          textOverflow: "ellipsis",
          whiteSpace: "nowrap",
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
