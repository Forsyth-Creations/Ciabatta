/**
 * A react-flow canvas with ciabatta's theme applied.
 *
 * Shared by the AI mind map, the analyze dependency graph and the run
 * flowchart, so all three pan, zoom and select identically instead of each
 * inventing its own interaction model.
 *
 * Layout is caller-supplied — these graphs have genuinely different shapes
 * (radial for the mind map, layered for a run DAG), so there's no useful
 * shared positioning strategy. See `layout.ts` for the helpers.
 */

import { useMemo } from "react";
import { Box, useTheme } from "@mui/material";
import {
  Background,
  BackgroundVariant,
  Controls,
  MiniMap,
  ReactFlow,
  type Edge,
  type Node,
  type NodeMouseHandler,
} from "@xyflow/react";
import "@xyflow/react/dist/style.css";

interface GraphCanvasProps {
  nodes: Node[];
  edges: Edge[];
  height?: number | string;
  onNodeClick?: NodeMouseHandler;
  /** Colour used by the minimap for a node. */
  nodeColor?: (node: Node) => string;
  /**
   * Show the minimap. Off by default on short canvases, where it covers more
   * of the graph than it helps you navigate.
   */
  minimap?: boolean;
  /**
   * Change this when the graph's *shape* changes — nodes added or removed
   * rather than restyled — to re-fit the view around it.
   *
   * react-flow only fits on mount, so without it a node placed outside the
   * current viewport (a new column of environment inputs, say) is drawn where
   * nobody can see it. Panning and zooming reset with it, which is the right
   * trade when what you're looking at has changed.
   */
  fitKey?: string | number;
}

export function GraphCanvas({
  nodes,
  edges,
  height = 560,
  onNodeClick,
  nodeColor,
  minimap,
  fitKey,
}: GraphCanvasProps) {
  const theme = useTheme();

  // react-flow's stylesheet is written against CSS variables, so the theme is
  // applied by overriding those rather than by restyling each element.
  const themeVars = useMemo(
    () => ({
      "--xy-background-color": theme.palette.background.default,
      "--xy-node-color": theme.palette.text.primary,
      "--xy-node-border": `1px solid ${theme.palette.divider}`,
      "--xy-node-background-color": theme.palette.background.paper,
      "--xy-node-boxshadow-hover": `0 0 0 1px ${theme.palette.primary.main}`,
      "--xy-node-boxshadow-selected": `0 0 0 2px ${theme.palette.primary.main}`,
      "--xy-edge-stroke": theme.palette.divider,
      "--xy-edge-stroke-selected": theme.palette.primary.main,
      "--xy-attribution-background-color": "transparent",
      "--xy-controls-button-background-color": theme.palette.background.paper,
      "--xy-controls-button-background-color-hover": theme.palette.action.hover,
      "--xy-controls-button-color": theme.palette.text.primary,
      "--xy-controls-button-border-color": theme.palette.divider,
      "--xy-minimap-background-color": theme.palette.background.paper,
    }),
    [theme],
  );

  return (
    <Box
      sx={{
        height,
        border: 1,
        borderColor: "divider",
        borderRadius: 1,
        overflow: "hidden",
        ...themeVars,
      }}
    >
      <ReactFlow
        key={fitKey}
        nodes={nodes}
        edges={edges}
        onNodeClick={onNodeClick}
        fitView
        // These are views of computed state, not editors: dragging a node
        // around would imply the position means something and survives a
        // refresh, and neither is true.
        nodesDraggable={false}
        nodesConnectable={false}
        edgesFocusable={false}
        proOptions={{ hideAttribution: true }}
        minZoom={0.05}
      >
        <Background variant={BackgroundVariant.Dots} gap={18} size={1} />
        <Controls showInteractive={false} />
        {(minimap ?? (typeof height === "number" && height >= 400)) && (
          <MiniMap pannable zoomable nodeColor={nodeColor} nodeStrokeWidth={2} />
        )}
      </ReactFlow>
    </Box>
  );
}
