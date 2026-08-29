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
  FormControlLabel,
  Stack,
  Switch,
  Tab,
  Tabs,
  Tooltip,
  Typography,
} from "@mui/material";
import ArrowBackIcon from "@mui/icons-material/ArrowBack";
import { IconButton } from "@mui/material";
import { useTheme } from "@mui/material/styles";
import type { Theme } from "@mui/material/styles";
import { Link, useParams } from "@tanstack/react-router";
import { Position, type Edge, type Node } from "@xyflow/react";

import { streamUrl } from "../api/client";
import {
  useChoose,
  undeclaredEnv,
  type WorkflowView,
  type RunState,
  type StepStatus,
  type StepView,
  type TargetDeps,
} from "../api/run";
import { humanizeBytes } from "../api/cache";
import type { EnvVar } from "../api/types";
import { AnsiText } from "../components/AnsiText";
import { GraphCanvas } from "../components/GraphCanvas";
import {
  EnvPanel,
  EnvVarChip,
  StepEnvChips,
  envValueText,
} from "../components/EnvVars";
import { ORTHOGONAL_EDGE, executionOrder, layeredLayout } from "../components/layout";
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

  const { state, error } = useRunStream(id);
  const [workflowIndex, setWorkflowIndex] = useState(0);
  const [selectedStep, setSelectedStep] = useState<string | null>(null);

  if (error) return <ErrorNote error={new Error(error)} />;
  if (!state) return <Loading label="Connecting to the run…" />;

  const workflow = state.workflows[workflowIndex];

  return (
    <>
      <Stack direction="row" alignItems="center" spacing={1.5} sx={{ mb: 2 }}>
        <IconButton component={Link} to="/run" size="small" aria-label="Back to runs">
          <ArrowBackIcon />
        </IconButton>
        <Box sx={{ flexGrow: 1, minWidth: 0 }}>
          <Typography variant="h1">Run #{id}</Typography>
          <Typography variant="caption" color="text.secondary">
            {state.run.workflows.join(", ")}
            {state.dry_run && " · dry run"}
          </Typography>
        </Box>
        <Chip
          size="small"
          variant="outlined"
          color={state.done ? "default" : "success"}
          label={state.done ? "finished" : "running"}
        />
      </Stack>

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

      <GraphCanvas
        nodes={nodes}
        edges={edges}
        height={420}
        // Turning the environment column on and off changes the graph's
        // extent, so the view has to be re-fitted around it.
        fitKey={`${showEnv ? "env" : ""}${showFiles ? "+files" : ""}` || "steps-only"}
        // A dependency node — a variable, or a set of files read or written —
        // isn't a step and has no logs of its own. Clicking one focuses the
        // graph on the steps it touches instead; clicking it again (or picking
        // any step) puts the whole graph back.
        onNodeClick={(_, node) => {
          if (!isDependencyNode(node.id)) {
            setFocused(null);
            onSelectStep(node.id);
            return;
          }
          setFocused((current) => (current === node.id ? null : node.id));
          onSelectStep(null);
        }}
        nodeColor={(node) => statusColor(node.data?.status as StepStatus, theme)}
      />

      {focused !== null && (
        <FocusNote workflow={workflow} focused={focused} onClear={() => setFocused(null)} />
      )}

      <Box sx={{ mt: 2 }}>
        <EnvPanel report={workflow.env} title="Environment this run started with" />
      </Box>

      <Box sx={{ mt: 2 }}>
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
        <LogBox lines={step ? step.logs : workflow.logs} />
        {step && (
          <Button size="small" sx={{ mt: 1 }} onClick={() => onSelectStep(null)}>
            Show all workflow logs
          </Button>
        )}
      </Box>
    </>
  );
}

function LogBox({ lines }: { lines: string[] }) {
  return (
    <Box
      sx={{
        maxHeight: 320,
        overflow: "auto",
        p: 1.5,
        border: 1,
        borderColor: "divider",
        borderRadius: 1,
        bgcolor: "background.default",
        fontFamily: monoFontStack,
        fontSize: 12.5,
        whiteSpace: "pre-wrap",
      }}
    >
      {lines.length === 0 ? (
        <Typography variant="body2" color="text.secondary">
          No output yet.
        </Typography>
      ) : (
        // Steps are asked for colour (the daemon sets FORCE_COLOR, since a pipe
        // would otherwise turn it off) and tools that cache their logs — turbo,
        // for one — replay escapes whatever the current environment says. Either
        // way the lines arrive carrying SGR, and printed raw they are worse than
        // no colour at all.
        lines.map((line, index) => (
          <div key={index}>
            <AnsiText
              text={line}
              fallbackColor={line.startsWith("[stderr]") ? "error.main" : undefined}
            />
          </div>
        ))
      )}
    </Box>
  );
}

/** Build the react-flow graph for one workflow's step DAG. */
/**
 * A workflow-graph node's label: its sub-workspace above, its step name below.
 * Plain runs keep the bare name, since there's only one package involved.
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

  const name = step.workspace ? (
    <>
      <Box sx={{ fontSize: 10, opacity: 0.75, fontFamily: monoFontStack }}>{step.workspace}</Box>
      <Box sx={{ fontWeight: 600 }}>{short}</Box>
    </>
  ) : (
    short
  );

  if (order === null) return <>{name}</>;

  return (
    <Stack direction="row" spacing={0.75} alignItems="center" justifyContent="center">
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
      <Box sx={{ minWidth: 0, textAlign: "left" }}>{name}</Box>
    </Stack>
  );
}

/** What a selected step is, beyond its command: where it's from, how it
 *  behaves, and the variables it depends on. */
function StepDetails({ step, env }: { step: StepView; env: EnvVar[] }) {
  const badges: string[] = [];
  if (step.push) badges.push("push");
  else if (step.kind) badges.push(step.kind);
  if (step.persistent) badges.push("persistent");
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

function buildFlow(
  workflow: WorkflowView,
  theme: Theme,
  showOrder: boolean,
  showEnv: boolean,
  showFiles: boolean,
  focused: string | null,
): { nodes: Node[]; edges: Edge[] } {
  const ids = workflow.steps.map((s) => s.name);
  const byName = new Map(workflow.steps.map((s) => [s.name, s]));

  // Input sets are grouped once, here, so the focus and the drawing agree
  // about which steps share a node.
  const groups = inputGroups(workflow);

  const focus: Focus =
    focused === null
      ? NO_FOCUS
      : (() => {
          const reached = litSteps(workflow, focused, groups);
          return { id: focused, lit: (id: string) => id === focused || reached.has(id) };
        })();

  // Only `needs` edges define run order; error/retry branches are annotations
  // on top and would distort the layout if they drove depth.
  const orderEdges = workflow.edges
    .filter((e) => e.kind === "needs")
    .map((e) => ({ source: e.from, target: e.to }));

  // Recovery steps are left out of the numbering: the engine only enters them
  // from a failed step's `on_error`, so they have no position in the sequence
  // the run takes when everything works.
  const sequence = showOrder
    ? executionOrder(
        workflow.steps.filter((s) => !s.recover).map((s) => s.name),
        orderEdges,
      )
    : new Map<string, number>();

  // A workflow graph draws nodes from several packages at once, so each one
  // leads with the sub-workspace it came from — "which package is this step
  // from?" is the first thing anyone asks when a shared build goes wrong.
  const positioned = layeredLayout(ids, orderEdges, (id) => {
    const step = byName.get(id);
    return {
      label:
        step && (step.workspace || showOrder) ? (
          <NodeLabel step={step} order={sequence.get(id) ?? null} />
        ) : (
          id
        ),
      status: step?.status ?? "pending",
    };
  });

  const nodes: Node[] = positioned.map((node) => {
    const step = byName.get(node.id);
    const color = statusColor(step?.status ?? "pending", theme);
    const on = focus.lit(node.id);
    const picked = on && focus.id !== null;
    return {
      ...node,
      style: {
        background: theme.palette.background.paper,
        color: theme.palette.text.primary,
        // Recovery nodes are dashed: they're branches you hope never run.
        border: `2px ${step?.recover ? "dashed" : "solid"} ${
          picked ? theme.palette.secondary.main : color
        }`,
        borderRadius: 8,
        fontSize: 12,
        padding: "6px 12px",
        minWidth: 120,
        opacity: on ? 1 : DIMMED,
        // A ring rather than a colour swap, so a step's status stays readable
        // while it's lit.
        boxShadow: picked ? `0 0 0 3px ${theme.palette.secondary.main}55` : undefined,
      },
    };
  });

  const edges: Edge[] = workflow.edges.map((edge, index) => ({
    ...ORTHOGONAL_EDGE,
    id: `${edge.from}->${edge.to}-${index}`,
    source: edge.from,
    target: edge.to,
    label: edge.kind === "needs" ? undefined : edge.kind,
    animated: byName.get(edge.from)?.status === "running",
    style: {
      stroke:
        edge.kind === "error"
          ? theme.palette.error.main
          : edge.kind === "retry"
            ? theme.palette.warning.main
            : theme.palette.divider,
      strokeDasharray: edge.kind === "needs" ? undefined : "5 4",
      // An edge survives the dimming only if both ends did — so a focused
      // node's steps keep the order between them, and everything else recedes.
      opacity: focus.lit(edge.from) && focus.lit(edge.to) ? 1 : DIMMED,
    },
  }));

  if (showFiles) {
    const files = fileFlow(workflow, theme, positioned, groups, focus);
    nodes.push(...files.nodes);
    edges.push(...files.edges);
  }

  if (showEnv) {
    // Variables sit outside the file column when both are drawn, so the two
    // kinds of dependency read as two columns rather than one pile.
    const env = envFlow(workflow, theme, positioned, focus, showFiles ? 620 : 300);
    nodes.push(...env.nodes);
    edges.push(...env.edges);
  }

  return { nodes, edges };
}

/**
 * The file half of the flowchart: what each target reads, and what it writes.
 *
 * These are two of a target's three dependencies (the third, the steps it
 * needs, the graph already draws), and they are the two that decide whether
 * anything runs at all. A graph that shows only the order is a graph that
 * cannot answer "why did this rebuild?" — the answer is always a file, and
 * until now the only way to see which files were even in scope was to open the
 * config.
 *
 * Input sets are **shared** rather than drawn per step: in a monorepo every step
 * of a package inherits that package's `cache.inputs`, so one node per distinct
 * set feeding several steps is both smaller and truer than one node each. The
 * duplication it collapses is real information — those steps rebuild together.
 *
 * Outputs are per step, because they are: two steps writing the same files
 * would be a bug, not a shared dependency.
 */
function fileFlow(
  workflow: WorkflowView,
  theme: Theme,
  steps: Node[],
  groups: Map<string, { deps: TargetDeps; steps: string[] }>,
  focus: Focus,
): { nodes: Node[]; edges: Edge[] } {
  const withDeps = workflow.steps.filter((step) => step.deps?.name);
  if (withDeps.length === 0) return { nodes: [], edges: [] };

  const xs = steps.map((node) => node.position.x);
  const leftmost = Math.min(...xs, 0);
  const rightmost = Math.max(...xs, 0);
  const centre =
    steps.length > 0 ? steps.reduce((sum, node) => sum + node.position.y, 0) / steps.length : 0;
  const rowHeight = 74;

  const producers = withDeps.filter((step) => step.deps.outputs.length > 0);

  const column = (count: number, index: number, x: number) => ({
    x,
    y: centre + (index - (count - 1) / 2) * rowHeight,
  });

  // A file set is clickable: it focuses the graph on the steps it touches, the
  // same way a variable does. The focused one goes solid and takes the ring —
  // it's the subject now, not an aside.
  const shell = (color: string, id: string) => {
    const picked = focus.id === id;
    return {
      background: theme.palette.background.paper,
      color: theme.palette.text.primary,
      // Dashed, like every dependency edge that isn't the run's own order: a
      // file set is a precondition, not a step that ran before this one.
      border: `${picked ? 2 : 1}px ${picked ? "solid" : "dashed"} ${color}`,
      borderRadius: 8,
      fontSize: 11,
      padding: "5px 10px",
      minWidth: 170,
      maxWidth: 240,
      textAlign: "left" as const,
      opacity: focus.lit(id) ? 1 : DIMMED,
      boxShadow: picked ? `0 0 0 3px ${theme.palette.secondary.main}55` : undefined,
      cursor: "pointer",
    };
  };

  const inputs = [...groups.values()];
  const nodes: Node[] = inputs.map((group, index) => ({
    id: inputNodeId(group.steps[0]),
    position: column(inputs.length, index, leftmost - 300),
    type: "default",
    sourcePosition: Position.Right,
    targetPosition: Position.Left,
    data: {
      label: <FileNodeLabel deps={group.deps} kind="reads" />,
      status: "pending" as StepStatus,
    },
    style: shell(theme.palette.info.main, inputNodeId(group.steps[0])),
  }));

  nodes.push(
    ...producers.map((step, index) => ({
      id: outputNodeId(step.name),
      position: column(producers.length, index, rightmost + 320),
      type: "default",
      sourcePosition: Position.Right,
      targetPosition: Position.Left,
      data: {
        label: <FileNodeLabel deps={step.deps} kind="writes" />,
        status: "pending" as StepStatus,
      },
      style: shell(theme.palette.success.main, outputNodeId(step.name)),
    })),
  );

  // An edge is drawn only as brightly as the dimmer of its two ends, and the
  // focused set's own edges are the one thing on the canvas that should be
  // moving.
  const fileEdge = (source: string, target: string) => ({
    lit: focus.lit(source) && focus.lit(target),
    animated: focus.id === source || focus.id === target,
  });

  const edges: Edge[] = inputs.flatMap((group) =>
    group.steps.map((step) => {
      const id = inputNodeId(group.steps[0]);
      const { lit, animated } = fileEdge(id, step);
      return {
        ...ORTHOGONAL_EDGE,
        id: `in:${group.steps[0]}->${step}`,
        source: id,
        target: step,
        animated,
        style: {
          stroke: theme.palette.info.main,
          strokeDasharray: "2 4",
          strokeWidth: animated ? 2 : 1,
          opacity: lit ? 0.75 : DIMMED,
        },
      };
    }),
  );

  edges.push(
    ...producers.map((step) => {
      const id = outputNodeId(step.name);
      const { lit, animated } = fileEdge(step.name, id);
      return {
        ...ORTHOGONAL_EDGE,
        id: `out:${step.name}`,
        source: step.name,
        target: id,
        // A finished step's outputs are on disk; a running one's are being
        // written as you watch.
        animated: animated || step.status === "running",
        style: {
          stroke: theme.palette.success.main,
          strokeDasharray: "2 4",
          strokeWidth: animated ? 2 : 1,
          opacity: lit ? 0.75 : DIMMED,
        },
      };
    }),
  );

  return { nodes, edges };
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

/**
 * The environment half of the flowchart: one node per variable a step reads,
 * feeding into the steps that read it.
 *
 * They sit in a column of their own to the left of the graph rather than
 * joining the layered layout, so turning them on doesn't reshuffle the waves
 * you were just looking at — an input arriving from off to the side is also how
 * you'd draw this by hand.
 *
 * A variable nothing reads (a `.env` line no step uses) isn't drawn: it has no
 * edge to justify a node. The panel below the graph still lists it.
 */
function envFlow(
  workflow: WorkflowView,
  theme: Theme,
  steps: Node[],
  focus: Focus,
  offset: number,
): { nodes: Node[]; edges: Edge[] } {
  const drawn = workflow.env.vars.filter((variable) => variable.steps.length > 0);
  if (drawn.length === 0) return { nodes: [], edges: [] };

  // One column left of the leftmost step, vertically centred on the graph.
  const leftmost = Math.min(...steps.map((node) => node.position.x), 0);
  const centre =
    steps.length > 0
      ? steps.reduce((sum, node) => sum + node.position.y, 0) / steps.length
      : 0;
  const rowHeight = 62;

  const colorOf = (variable: EnvVar) =>
    variable.origin === "unset" ? theme.palette.error.main : theme.palette.secondary.main;

  const nodes: Node[] = drawn.map((variable, index) => {
    const id = envNodeId(variable.key);
    const picked = focus.id === id;
    return {
      id,
      position: {
        x: leftmost - offset,
        y: centre + (index - (drawn.length - 1) / 2) * rowHeight,
      },
      type: "default",
      // Feeding in from the left, like every other dependency on this canvas.
      sourcePosition: Position.Right,
      targetPosition: Position.Left,
      data: {
        label: <EnvNodeLabel variable={variable} />,
        status: "pending" as StepStatus,
      },
      style: {
        background: theme.palette.background.paper,
        color: theme.palette.text.primary,
        // Dashed, like the other edges that aren't the run's own order: a
        // variable is a precondition, not a step that ran before this one.
        // The selected one goes solid — it's the subject now, not an aside.
        border: `${picked ? 2 : 1}px ${picked ? "solid" : "dashed"} ${colorOf(variable)}`,
        borderRadius: 20,
        fontSize: 11,
        padding: "4px 10px",
        minWidth: 140,
        textAlign: "left" as const,
        opacity: focus.lit(id) ? 1 : DIMMED,
        boxShadow: picked ? `0 0 0 3px ${theme.palette.secondary.main}55` : undefined,
        cursor: "pointer",
      },
    };
  });

  // A variable's step list comes from the resolved run, so guard against an
  // edge into a node this view isn't drawing rather than handing react-flow a
  // dangling target.
  const present = new Set(workflow.steps.map((step) => step.name));
  const edges: Edge[] = drawn.flatMap((variable) => {
    const id = envNodeId(variable.key);
    const picked = focus.id === id;
    return variable.steps
      .filter((step) => present.has(step))
      .map((step) => ({
        ...ORTHOGONAL_EDGE,
        id: `env:${variable.key}->${step}`,
        source: id,
        target: step,
        // The selected variable's edges are the one thing on the canvas that
        // should be moving.
        animated: picked,
        style: {
          stroke: colorOf(variable),
          strokeDasharray: "2 4",
          strokeWidth: picked ? 2 : 1,
          opacity: focus.lit(id) && focus.lit(step) ? 0.75 : DIMMED,
        },
      }));
  });

  return { nodes, edges };
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

function statusColor(status: StepStatus | string, theme: Theme): string {
  switch (status) {
    case "running":
      return theme.palette.warning.main;
    case "success":
      return theme.palette.success.main;
    case "failed":
      return theme.palette.error.main;
    case "skipped":
      return theme.palette.text.disabled;
    default:
      return theme.palette.divider;
  }
}

function stageColor(status: string): "success" | "error" | "warning" | "default" {
  switch (status) {
    case "success":
      return "success";
    case "failed":
      return "error";
    case "running":
      return "warning";
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
