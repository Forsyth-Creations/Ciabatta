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
  type RecipeView,
  type RunState,
  type StepStatus,
  type StepView,
} from "../api/run";
import type { EnvVar } from "../api/types";
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

/** How far the graph fades what a highlighted variable doesn't reach. */
const DIMMED = 0.18;

export function RunDetailPage() {
  const { runId } = useParams({ from: "/run/$runId" });
  const id = Number(runId);

  const { state, error } = useRunStream(id);
  const [recipeIndex, setRecipeIndex] = useState(0);
  const [selectedStep, setSelectedStep] = useState<string | null>(null);

  if (error) return <ErrorNote error={new Error(error)} />;
  if (!state) return <Loading label="Connecting to the run…" />;

  const recipe = state.recipes[recipeIndex];

  return (
    <>
      <Stack direction="row" alignItems="center" spacing={1.5} sx={{ mb: 2 }}>
        <IconButton component={Link} to="/run" size="small" aria-label="Back to runs">
          <ArrowBackIcon />
        </IconButton>
        <Box sx={{ flexGrow: 1, minWidth: 0 }}>
          <Typography variant="h1">Run #{id}</Typography>
          <Typography variant="caption" color="text.secondary">
            {state.run.recipes.join(", ")}
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

      {state.recipes.length > 1 && (
        <Tabs
          value={recipeIndex}
          onChange={(_, next) => {
            setRecipeIndex(next);
            setSelectedStep(null);
          }}
          sx={{ mb: 2 }}
        >
          {state.recipes.map((r) => (
            <Tab key={r.name} label={r.name} />
          ))}
        </Tabs>
      )}

      {recipe && (
        <RecipePanel
          runId={id}
          recipe={recipe}
          selectedStep={selectedStep}
          onSelectStep={setSelectedStep}
        />
      )}
    </>
  );
}

function RecipePanel({
  runId,
  recipe,
  selectedStep,
  onSelectStep,
}: {
  runId: number;
  recipe: RecipeView;
  selectedStep: string | null;
  onSelectStep: (name: string | null) => void;
}) {
  const theme = useTheme();
  const choose = useChoose(runId);
  const [showOrder, setShowOrder] = useState(false);
  // Variables are dependencies, so the graph draws them like every other
  // dependency. The toggle is for the graphs where they'd crowd out the steps.
  const [showEnv, setShowEnv] = useState(true);
  // The variable whose dependents are lit up. "Who reads DATABASE_URL?" is the
  // question a variable node exists to answer, and on a wide graph the edges
  // alone don't answer it — so selecting one dims everything it doesn't reach.
  const [highlighted, setHighlighted] = useState<string | null>(null);

  const { nodes, edges } = useMemo(
    () => buildFlow(recipe, theme, showOrder, showEnv, highlighted),
    [recipe, theme, showOrder, showEnv, highlighted],
  );
  const step = recipe.steps.find((s) => s.name === selectedStep);
  const highlightedVar = recipe.env.vars.find((v) => v.key === highlighted);

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
        {recipe.stages.map((stage) => (
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
                onChange={(_, checked) => setShowEnv(checked)}
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

      {recipe.error && (
        <Alert severity="error" sx={{ mb: 2 }}>
          {recipe.error}
        </Alert>
      )}

      {recipe.pending && (
        <Alert severity="warning" sx={{ mb: 2 }}>
          <Typography sx={{ mb: 1 }}>{recipe.pending.message}</Typography>
          <Stack direction="row" spacing={1} flexWrap="wrap" useFlexGap>
            {recipe.pending.options.map((option, index) => (
              <Button
                key={option}
                size="small"
                variant="contained"
                disabled={choose.isPending}
                onClick={() =>
                  choose.mutate({
                    recipe: recipe.name,
                    step: recipe.pending!.step,
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
        fitKey={showEnv ? "with-env" : "steps-only"}
        // An environment node isn't a step and has no logs of its own: clicking
        // one lights up the steps that read it instead, and clicking it again
        // (or picking any step) puts the whole graph back.
        onNodeClick={(_, node) => {
          const key = envKeyOf(node.id);
          if (key === null) {
            setHighlighted(null);
            onSelectStep(node.id);
            return;
          }
          setHighlighted((current) => (current === key ? null : key));
          onSelectStep(null);
        }}
        nodeColor={(node) => statusColor(node.data?.status as StepStatus, theme)}
      />

      {highlightedVar && (
        <Stack
          direction="row"
          spacing={1}
          sx={{ mt: 1 }}
          alignItems="center"
          flexWrap="wrap"
          useFlexGap
        >
          <EnvVarChip variable={highlightedVar} />
          <Typography variant="caption" color="text.secondary">
            {highlightedVar.steps.length > 0
              ? `read by ${highlightedVar.steps.join(", ")}`
              : "no step reads this"}
          </Typography>
          <Button size="small" onClick={() => setHighlighted(null)}>
            Clear
          </Button>
        </Stack>
      )}

      <Box sx={{ mt: 2 }}>
        <EnvPanel report={recipe.env} title="Environment this run started with" />
      </Box>

      <Box sx={{ mt: 2 }}>
        <Typography variant="h3" sx={{ mb: 1 }}>
          {step ? `${step.name} logs` : "Recipe logs"}
        </Typography>
        {step && (
          <StepDetails
            step={step}
            env={recipe.env.vars.filter((variable) => variable.steps.includes(step.name))}
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
        <LogBox lines={step ? step.logs : recipe.logs} />
        {step && (
          <Button size="small" sx={{ mt: 1 }} onClick={() => onSelectStep(null)}>
            Show all recipe logs
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
        lines.map((line, index) => <div key={index}>{line}</div>)
      )}
    </Box>
  );
}

/** Build the react-flow graph for one recipe's step DAG. */
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

  const bare =
    !step.workspace && !step.description && badges.length === 0 && reads.length === 0 && own.size === 0;
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
    </Stack>
  );
}

function buildFlow(
  recipe: RecipeView,
  theme: Theme,
  showOrder: boolean,
  showEnv: boolean,
  highlighted: string | null,
): { nodes: Node[]; edges: Edge[] } {
  const ids = recipe.steps.map((s) => s.name);
  const byName = new Map(recipe.steps.map((s) => [s.name, s]));

  // Which steps a highlighted variable reaches. Null means nothing is
  // highlighted, which is not the same as "reaches nothing".
  const lit =
    highlighted === null
      ? null
      : new Set(recipe.env.vars.find((v) => v.key === highlighted)?.steps ?? []);

  // Only `needs` edges define run order; error/retry branches are annotations
  // on top and would distort the layout if they drove depth.
  const orderEdges = recipe.edges
    .filter((e) => e.kind === "needs")
    .map((e) => ({ source: e.from, target: e.to }));

  // Recovery steps are left out of the numbering: the engine only enters them
  // from a failed step's `on_error`, so they have no position in the sequence
  // the run takes when everything works.
  const sequence = showOrder
    ? executionOrder(
        recipe.steps.filter((s) => !s.recover).map((s) => s.name),
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
    const reads = lit?.has(node.id) ?? true;
    return {
      ...node,
      style: {
        background: theme.palette.background.paper,
        color: theme.palette.text.primary,
        // Recovery nodes are dashed: they're branches you hope never run.
        border: `2px ${step?.recover ? "dashed" : "solid"} ${
          reads && lit ? theme.palette.secondary.main : color
        }`,
        borderRadius: 8,
        fontSize: 12,
        padding: "6px 12px",
        minWidth: 120,
        opacity: reads ? 1 : DIMMED,
        // A ring rather than a colour swap, so a step's status stays readable
        // while it's lit.
        boxShadow: reads && lit ? `0 0 0 3px ${theme.palette.secondary.main}55` : undefined,
      },
    };
  });

  const edges: Edge[] = recipe.edges.map((edge, index) => ({
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
      // While a variable is highlighted the run's own edges are context, not
      // the subject.
      opacity: lit ? DIMMED : 1,
    },
  }));

  if (showEnv) {
    const env = envFlow(recipe, theme, positioned, highlighted);
    nodes.push(...env.nodes);
    edges.push(...env.edges);
  }

  return { nodes, edges };
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
  recipe: RecipeView,
  theme: Theme,
  steps: Node[],
  highlighted: string | null,
): { nodes: Node[]; edges: Edge[] } {
  const drawn = recipe.env.vars.filter((variable) => variable.steps.length > 0);
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
    const on = highlighted === null || highlighted === variable.key;
    return {
      id: envNodeId(variable.key),
      position: {
        x: leftmost - 300,
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
        border: `${highlighted === variable.key ? 2 : 1}px ${
          highlighted === variable.key ? "solid" : "dashed"
        } ${colorOf(variable)}`,
        borderRadius: 20,
        fontSize: 11,
        padding: "4px 10px",
        minWidth: 140,
        textAlign: "left" as const,
        opacity: on ? 1 : DIMMED,
        boxShadow:
          highlighted === variable.key ? `0 0 0 3px ${theme.palette.secondary.main}55` : undefined,
        cursor: "pointer",
      },
    };
  });

  // A variable's step list comes from the resolved run, so guard against an
  // edge into a node this view isn't drawing rather than handing react-flow a
  // dangling target.
  const present = new Set(recipe.steps.map((step) => step.name));
  const edges: Edge[] = drawn.flatMap((variable) => {
    const on = highlighted === null || highlighted === variable.key;
    return variable.steps
      .filter((step) => present.has(step))
      .map((step) => ({
        ...ORTHOGONAL_EDGE,
        id: `env:${variable.key}->${step}`,
        source: envNodeId(variable.key),
        target: step,
        // The selected variable's edges are the one thing on the canvas that
        // should be moving.
        animated: highlighted === variable.key,
        style: {
          stroke: colorOf(variable),
          strokeDasharray: "2 4",
          strokeWidth: highlighted === variable.key ? 2 : 1,
          opacity: on ? 0.75 : DIMMED,
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
