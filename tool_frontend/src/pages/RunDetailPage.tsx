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
import type { Edge, Node } from "@xyflow/react";

import { streamUrl } from "../api/client";
import {
  useChoose,
  type RecipeView,
  type RunState,
  type StepStatus,
  type StepView,
} from "../api/run";
import { GraphCanvas } from "../components/GraphCanvas";
import { executionOrder, layeredLayout } from "../components/layout";
import { ErrorNote, Loading } from "../components/Page";
import { monoFontStack } from "../theme";

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

  const { nodes, edges } = useMemo(
    () => buildFlow(recipe, theme, showOrder),
    [recipe, theme, showOrder],
  );
  const step = recipe.steps.find((s) => s.name === selectedStep);

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
        onNodeClick={(_, node) => onSelectStep(node.id)}
        nodeColor={(node) => statusColor(node.data?.status as StepStatus, theme)}
      />

      <Box sx={{ mt: 2 }}>
        <Typography variant="h3" sx={{ mb: 1 }}>
          {step ? `${step.name} logs` : "Recipe logs"}
        </Typography>
        {step && <StepDetails step={step} />}
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

/** What a selected step is, beyond its command: where it's from and how it behaves. */
function StepDetails({ step }: { step: StepView }) {
  const badges: string[] = [];
  if (step.push) badges.push("push");
  else if (step.kind) badges.push(step.kind);
  if (step.persistent) badges.push("persistent");
  if (step.timeout) badges.push(`timeout ${step.timeout}`);
  if (step.requires.length > 0) badges.push(`needs ${step.requires.join(", ")}`);

  if (!step.workspace && !step.description && badges.length === 0) return null;

  return (
    <Stack direction="row" spacing={1} sx={{ mb: 1 }} flexWrap="wrap" useFlexGap alignItems="center">
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
  );
}

function buildFlow(
  recipe: RecipeView,
  theme: Theme,
  showOrder: boolean,
): { nodes: Node[]; edges: Edge[] } {
  const ids = recipe.steps.map((s) => s.name);
  const byName = new Map(recipe.steps.map((s) => [s.name, s]));

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
    return {
      ...node,
      style: {
        background: theme.palette.background.paper,
        color: theme.palette.text.primary,
        // Recovery nodes are dashed: they're branches you hope never run.
        border: `2px ${step?.recover ? "dashed" : "solid"} ${color}`,
        borderRadius: 8,
        fontSize: 12,
        padding: "6px 12px",
        minWidth: 120,
      },
    };
  });

  const edges: Edge[] = recipe.edges.map((edge, index) => ({
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
    },
  }));

  return { nodes, edges };
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
