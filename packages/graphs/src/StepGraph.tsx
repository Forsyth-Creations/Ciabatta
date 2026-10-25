/**
 * A finished run's steps as a flowchart, ready to drop into a page.
 *
 * The daemon's run page builds its graph by hand, because it layers a great
 * deal on top (live status, environment and file nodes, focus, cache inspect
 * mode). Anything that only needs to *show* a run — the remote cache's build
 * view — uses this, and gets the same canvas, layout, wires and nodes.
 */

import { useMemo, useRef } from "react";
import { useTheme } from "@mui/material";
import type { Edge, Node, NodeMouseHandler } from "@xyflow/react";

import { GraphCanvas } from "./GraphCanvas";
import { ROUTED_EDGE, layeredLayout, routeKey, type Placement } from "./layout";
import { statusColour } from "./StatusIcon";
import { STEP_LAYOUT, StepLine, StepNodeBody, stepNodeHeight, stepNodeStyle } from "./StepNode";

export interface GraphStep {
  /** Unique: what `needs` refer to it by. */
  name: string;
  /** What the node shows, when that isn't the name. */
  label?: string;
  /** Drawn in the primary colour — the step a page has picked. */
  highlight?: boolean;
  workspace?: string | null;
  status: string;
  /** Steps this one waits for. Names not in the list are ignored. */
  needs?: string[];
  /** The line under the name. */
  detail?: string;
  /** A second small line, such as the cache's verdict. */
  note?: string | null;
  /** Served from the cache rather than run — as good an outcome as passing. */
  reused?: boolean;
  /** A recovery step, drawn dashed. */
  recover?: boolean;
  /** Runs alongside, nothing waits for it: parked in a row underneath. */
  background?: boolean;
}

export function StepGraph({
  steps,
  height = 460,
  onStepClick,
  minimap,
  holdLayout,
}: {
  steps: GraphStep[];
  height?: number | string;
  onStepClick?: (name: string) => void;
  minimap?: boolean;
  /**
   * For a graph being edited: keep every node where it was, and the view where
   * the user left it, rather than laying out and refitting from scratch on
   * each change. Nodes move only when the change forces them to.
   */
  holdLayout?: boolean;
}) {
  const theme = useTheme();
  const placed = useRef<Map<string, Placement> | undefined>(undefined);

  const { nodes, edges } = useMemo(() => {
    const byName = new Map(steps.map((s) => [s.name, s]));
    const heightOf = (id: string) => {
      const step = byName.get(id)!;
      return stepNodeHeight(Boolean(step.workspace), step.note ? 1 : 0);
    };
    const links = steps.flatMap((s) =>
      (s.needs ?? []).filter((n) => byName.has(n)).map((n) => ({ source: n, target: s.name })),
    );

    const {
      nodes: positioned,
      routes,
      placement,
    } = layeredLayout(
      steps.map((s) => s.name),
      links.map((e) => ({ ...e, group: "needs", merge: true })),
      (id) => {
        const s = byName.get(id)!;
        return {
          status: s.status,
          label: (
            <StepNodeBody
              name={s.label ?? s.name}
              workspace={s.workspace}
              status={s.status}
              detail={s.detail ?? s.status}
              extra={s.note ? <StepLine mono={false}>{s.note}</StepLine> : undefined}
            />
          ),
        };
      },
      {
        ...STEP_LAYOUT,
        heightOf,
        bottom: (id) => byName.get(id)?.background ?? false,
        previous: holdLayout ? placed.current : undefined,
      },
    );
    if (holdLayout) placed.current = placement;

    const nodes: Node[] = positioned.map((node) => {
      const s = byName.get(node.id)!;
      return {
        ...node,
        style: stepNodeStyle({
          theme,
          height: heightOf(node.id),
          border: s.highlight
            ? theme.palette.primary.main
            : s.reused
              ? theme.palette.success.main
              : statusColour(s.status, theme),
          dashed: s.recover,
          opacity: s.status === "skipped" || s.status === "pending" ? 0.7 : 1,
        }),
      };
    });

    const stroke = theme.palette.text.secondary;
    const edges: Edge[] = links.map((link) => ({
      ...ROUTED_EDGE,
      id: `${link.source}->${link.target}`,
      source: link.source,
      target: link.target,
      data: { route: routes.get(routeKey(link.source, link.target)) },
      markerEnd: { ...ROUTED_EDGE.markerEnd, color: stroke },
      style: { stroke, strokeWidth: 1.4 },
    }));

    return { nodes, edges };
  }, [steps, theme, holdLayout]);

  const onNodeClick: NodeMouseHandler | undefined = onStepClick
    ? (_, node) => onStepClick(node.id)
    : undefined;

  return (
    <GraphCanvas
      nodes={nodes}
      edges={edges}
      height={height}
      onNodeClick={onNodeClick}
      minimap={minimap}
      keepView={holdLayout}
      // Refit when the shape changes, not just the set of nodes: an edited
      // graph that gains an edge can push a node off the canvas.
      fitKey={steps.map((s) => `${s.name}<${(s.needs ?? []).join(",")}`).join("\u0000")}
      nodeColor={(node) => {
        const s = steps.find((x) => x.name === node.id);
        if (!s) return theme.palette.divider;
        return s.reused ? theme.palette.success.main : statusColour(s.status, theme);
      }}
    />
  );
}
