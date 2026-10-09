/**
 * A compiled workflow, drawn as the graph it is.
 *
 * The workspace page used to list a workflow's waves one under another, which
 * says what runs together but hides *why*: which step waits for which, and
 * where a package's steps hand over to the next package's. This draws the same
 * compiled graph with the same layered layout and routed wiring as a live
 * run's flowchart, so the preview and the run look like one thing.
 *
 * Read-only — there is no run yet — but each node carries what the cache
 * would do with it, when caching is on, so the graph doubles as the plan.
 */

import { useMemo } from "react";
import { Box, Stack, Tooltip } from "@mui/material";
import { useTheme } from "@mui/material/styles";
import type { Theme } from "@mui/material/styles";
import BoltIcon from "@mui/icons-material/Bolt";
import CachedIcon from "@mui/icons-material/Cached";
import { type Edge, type Node } from "@xyflow/react";

import type { PlannedStep } from "../api/types";
import type { WorkflowGraph } from "../api/workspace";
import { GraphCanvas } from "./GraphCanvas";
import { ROUTED_EDGE, layeredLayout, routeKey, type LayoutEdge } from "./layout";
import { monoFontStack } from "../theme";

const NODE_WIDTH = 210;
const LINE = { name: 16, small: 14 } as const;
const CHROME = 2 * 2 + 6 * 2;

function heightOf(hasWorkspace: boolean, hasPlan: boolean): number {
  return CHROME + LINE.name + LINE.small + (hasWorkspace ? LINE.small : 0) + (hasPlan ? LINE.small : 0);
}

/** What a stage's planned cache decision looks like on a node. */
function planWords(step: PlannedStep | undefined): { text: string; tone: "good" | "run" | "off" } | null {
  if (!step) return null;
  switch (step.decision.outcome) {
    case "fresh":
      return { text: "cached · up to date", tone: "good" };
    case "hit":
      return { text: "cached · would restore", tone: "good" };
    case "rebuild":
      return { text: "would run", tone: "run" };
    case "uncached":
      return { text: "not cached", tone: "off" };
  }
}

function toneColour(tone: "good" | "run" | "off", theme: Theme): string {
  return tone === "good"
    ? theme.palette.success.main
    : tone === "run"
      ? theme.palette.warning.main
      : theme.palette.text.disabled;
}

export function WorkflowFlow({
  graph,
  planned,
  selected,
  onSelect,
  height,
}: {
  graph: WorkflowGraph;
  planned: Map<string, PlannedStep>;
  selected: string | null;
  onSelect: (id: string | null) => void;
  height?: number;
}) {
  // Tall enough for the widest wave and the rows under it, and no taller: a
  // five-step chain in a 480px box is mostly empty canvas, and the fit zooms
  // it down to match.
  const rows = Math.max(1, ...graph.waves.map((wave) => wave.length));
  const extra =
    graph.nodes.some((n) => n.background) || graph.nodes.some((n) => n.recover) ? 1 : 0;
  const canvasHeight = height ?? Math.min(560, Math.max(220, (rows + extra) * 90 + 80));
  const theme = useTheme();

  const { nodes, edges } = useMemo(() => {
    const byId = new Map(graph.nodes.map((node) => [node.id, node]));
    const ids = graph.nodes.map((node) => node.id);
    const layoutEdges: LayoutEdge[] = graph.edges.map((edge) => ({
      source: edge.from,
      target: edge.to,
      group: edge.kind,
      merge: edge.kind === "needs",
    }));

    const { nodes: positioned, routes } = layeredLayout(
      ids,
      layoutEdges,
      (id) => {
        const node = byId.get(id)!;
        const short =
          node.workspace && node.id.startsWith(`${node.workspace}:`)
            ? node.id.slice(node.workspace.length + 1)
            : node.id;
        const plan = planWords(planned.get(id));
        return {
          label: (
            <Stack direction="row" spacing={0.75} alignItems="center" sx={{ width: "100%", minWidth: 0 }}>
              <Box sx={{ minWidth: 0, flexGrow: 1 }} title={node.description ?? node.id}>
                {node.workspace && (
                  <Box sx={{ ...oneLine(LINE.small), fontSize: 10, opacity: 0.75, fontFamily: monoFontStack }}>
                    {node.workspace}
                  </Box>
                )}
                <Box sx={{ ...oneLine(LINE.name), fontWeight: 600 }}>{short}</Box>
                <Box sx={{ ...oneLine(LINE.small), fontSize: 10, opacity: 0.7 }}>
                  {node.recover
                    ? "on failure only"
                    : node.background
                      ? "background · nothing waits"
                      : node.wave !== null
                        ? `wave ${node.wave + 1}${node.owner ? ` · ${node.owner}` : ""}`
                        : (node.owner ?? "")}
                </Box>
                {plan && (
                  <Box
                    sx={{
                      ...oneLine(LINE.small),
                      fontSize: 10,
                      fontWeight: 600,
                      color: toneColour(plan.tone, theme),
                    }}
                  >
                    {plan.text}
                  </Box>
                )}
              </Box>
              {node.background && (
                <Tooltip title="A background task: started first, nothing waits for it">
                  <BoltIcon sx={{ fontSize: 15, color: "info.main" }} />
                </Tooltip>
              )}
              {plan?.tone === "good" && (
                <Tooltip title="The cache already has this — a run would reuse it">
                  <CachedIcon sx={{ fontSize: 15, color: "success.main" }} />
                </Tooltip>
              )}
            </Stack>
          ),
        };
      },
      {
        columnWidth: NODE_WIDTH + 130,
        nodeWidth: NODE_WIDTH,
        rowGap: 26,
        heightOf: (id) => {
          const node = byId.get(id);
          return heightOf(Boolean(node?.workspace), planned.has(id));
        },
        bottom: (id) => byId.get(id)?.background ?? false,
      },
    );

    const nodes: Node[] = positioned.map((node) => {
      const step = byId.get(node.id)!;
      const picked = selected === node.id;
      const border = step.recover
        ? theme.palette.warning.main
        : step.background
          ? theme.palette.info.main
          : theme.palette.primary.main;
      return {
        ...node,
        style: {
          background: theme.palette.background.paper,
          color: theme.palette.text.primary,
          borderRadius: 8,
          width: NODE_WIDTH,
          height: heightOf(Boolean(step.workspace), planned.has(node.id)),
          boxSizing: "border-box",
          display: "flex",
          alignItems: "center",
          overflow: "hidden",
          fontSize: 12,
          padding: "6px 12px",
          border: `2px ${step.recover ? "dashed" : "solid"} ${picked ? theme.palette.secondary.main : border}`,
          boxShadow: picked ? `0 0 0 3px ${theme.palette.secondary.main}55` : undefined,
          cursor: "pointer",
        },
      };
    });

    const edges: Edge[] = graph.edges.map((edge, index) => {
      const stroke =
        edge.kind === "on_error" ? theme.palette.error.main : theme.palette.text.secondary;
      return {
        ...ROUTED_EDGE,
        id: `${edge.from}->${edge.to}-${index}`,
        source: edge.from,
        target: edge.to,
        data: { route: routes.get(routeKey(edge.from, edge.to)) },
        label: edge.kind === "on_error" ? "on failure" : undefined,
        labelStyle: { fill: theme.palette.text.secondary, fontSize: 10 },
        labelBgStyle: { fill: theme.palette.background.paper },
        labelBgPadding: [4, 2] as [number, number],
        labelBgBorderRadius: 4,
        markerEnd: { ...ROUTED_EDGE.markerEnd, color: stroke },
        style: {
          stroke,
          strokeWidth: 1.4,
          strokeDasharray: edge.kind === "needs" ? undefined : "5 4",
        },
      };
    });

    return { nodes, edges };
  }, [graph, planned, selected, theme]);

  return (
    <GraphCanvas
      nodes={nodes}
      edges={edges}
      height={canvasHeight}
      // The plan arrives after the graph and adds a line to every node, so it
      // has to re-fit when it lands, or the first fit is of nodes that have
      // since changed size.
      fitKey={`${graph.workflow}:${graph.nodes.length}:${planned.size}`}
      onNodeClick={(_, node) => onSelect(selected === node.id ? null : node.id)}
      onPaneClick={() => onSelect(null)}
      minimap={graph.nodes.length > 12}
    />
  );
}

function oneLine(height: number) {
  return {
    height,
    lineHeight: `${height}px`,
    overflow: "hidden",
    whiteSpace: "nowrap",
    textOverflow: "ellipsis",
  } as const;
}
