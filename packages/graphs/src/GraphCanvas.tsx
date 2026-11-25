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

import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import FullscreenIcon from "@mui/icons-material/Fullscreen";
import FullscreenExitIcon from "@mui/icons-material/FullscreenExit";
import { Box, useTheme } from "@mui/material";
import {
  Background,
  useNodesInitialized,
  useReactFlow,
  useStore,
  BackgroundVariant,
  ControlButton,
  Controls,
  MiniMap,
  ReactFlow,
  type Edge,
  type Node,
  type NodeMouseHandler,
  type OnNodesChange,
} from "@xyflow/react";
import "@xyflow/react/dist/style.css";

import { RoutedEdge } from "./RoutedEdge";

/** Defined once, outside the component: react-flow warns, and re-mounts every
 *  edge, if it is handed a new object on each render. */
const EDGE_TYPES = { routed: RoutedEdge };

interface GraphCanvasProps {
  nodes: Node[];
  edges: Edge[];
  height?: number | string;
  onNodeClick?: NodeMouseHandler;
  /** Clicking empty canvas — the way out of a focus without hunting for the
   *  node that started it. */
  onPaneClick?: () => void;
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
  /**
   * Keep the view where the user left it when `fitKey` changes, and pan only
   * as far as it takes to bring nodes that moved or appeared into view. For a
   * graph being edited, where re-fitting on every change would jump the
   * picture around under the cursor.
   */
  keepView?: boolean;
  /**
   * A node to keep in sight: when it is off the canvas, or drawn too small to
   * read, the view pans (and zooms in to a readable size) to centre it. A node
   * already on screen at a readable size is left where it is, so clicking one
   * doesn't move the graph under the pointer.
   */
  focus?: string | null;
}

export function GraphCanvas({
  nodes,
  edges,
  height = 560,
  onNodeClick,
  onPaneClick,
  nodeColor,
  minimap,
  fitKey,
  keepView,
  focus,
}: GraphCanvasProps) {
  const theme = useTheme();

  // Full screen: the canvas alone fills the window, so a long run can be read
  // without squinting. Always as an overlay across the whole page, which needs
  // no permission; and, where the browser allows it, as true full screen on
  // top. A browser may refuse that — an iframe without `allow="fullscreen"`,
  // or a click it doesn't count as the person's — and the overlay still works.
  // Esc or the button leaves either.
  const frame = useRef<HTMLDivElement | null>(null);
  const [full, setFull] = useState(false);
  useEffect(() => {
    // Leaving the browser's full screen (its own Esc) leaves ours too.
    const changed = () => {
      if (!document.fullscreenElement) setFull(false);
    };
    document.addEventListener("fullscreenchange", changed);
    return () => document.removeEventListener("fullscreenchange", changed);
  }, []);
  useEffect(() => {
    if (!full) return;
    const esc = (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        // Before a dialog around the canvas takes Esc as "close".
        event.stopPropagation();
        setFull(false);
      }
    };
    window.addEventListener("keydown", esc, true);
    return () => window.removeEventListener("keydown", esc, true);
  }, [full]);
  const toggleFull = () => {
    if (full) {
      setFull(false);
      if (document.fullscreenElement)
        void document.exitFullscreen().catch(() => {});
      return;
    }
    setFull(true);
    frame.current?.requestFullscreen?.().catch(() => {
      // Refused: the overlay is the full screen.
    });
  };

  // The size react-flow last measured for each node, carried across rebuilds.
  //
  // react-flow keys a node's measured size to the *object identity* of the node
  // it was handed (`adoptUserNodes`, via `checkEquality`). These graphs are
  // rebuilt from scratch whenever their source data changes — for a live run,
  // on every frame off the SSE stream — so each node arrives as a new object
  // and its measurement is dropped. An unmeasured node renders
  // `visibility: hidden` and loses its handle bounds, so both it and its edges
  // vanish until a ResizeObserver round-trip restores them. That round-trip
  // usually lands before paint, which is why the graph only *occasionally*
  // goes blank: under a stream of updates it loses the race.
  //
  // Handing the measurement back on the new object is what react-flow's
  // `measured` field is for. The node stays visible at its last known size,
  // and the ResizeObserver still corrects it whenever the real size changes.
  const measured = useRef(new Map<string, { width: number; height: number }>());

  const onNodesChange = useCallback<OnNodesChange>((changes) => {
    for (const change of changes) {
      if (change.type === "dimensions" && change.dimensions) {
        measured.current.set(change.id, change.dimensions);
      }
    }
  }, []);

  const sized = useMemo(() => {
    // Forget nodes that are no longer drawn, so toggling a column on and off
    // doesn't grow the map for the life of the page.
    const drawn = new Set(nodes.map((node) => node.id));
    for (const id of [...measured.current.keys()]) {
      if (!drawn.has(id)) measured.current.delete(id);
    }
    return nodes.map((node) => {
      if (node.measured) return node;
      const size = measured.current.get(node.id);
      return size ? { ...node, measured: size } : node;
    });
  }, [nodes]);

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
      ref={frame}
      sx={{
        height: full ? "100vh" : height,
        bgcolor: "background.default",
        ...(full && {
          position: "fixed",
          inset: 0,
          // Over the page and any dialog it is in; menus opened from a node
          // are portalled after it, so they still draw on top.
          zIndex: theme.zIndex.modal,
          borderRadius: 0,
        }),
        border: 1,
        borderColor: "divider",
        borderRadius: 1,
        overflow: "hidden",
        ...themeVars,
      }}
    >
      <ReactFlow
        key={keepView ? undefined : fitKey}
        nodes={sized}
        edges={edges}
        edgeTypes={EDGE_TYPES}
        onNodesChange={onNodesChange}
        onNodeClick={onNodeClick}
        onPaneClick={onPaneClick}
        // No `fitView` here: `ViewControl` fits, then makes the view readable,
        // in that order. react-flow's own fit raced it.
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
        <ViewControl focus={focus ?? null} refit={full} />
        {keepView && <KeepInView fitKey={fitKey} />}
        <Controls showInteractive={false} fitViewOptions={FIT}>
          {
            <ControlButton
              onClick={toggleFull}
              title={full ? "Leave full screen (Esc)" : "Full screen"}
              aria-label={full ? "Leave full screen" : "Full screen"}
            >
              {full ? (
                <FullscreenExitIcon sx={{ fontSize: 16 }} />
              ) : (
                <FullscreenIcon sx={{ fontSize: 16 }} />
              )}
            </ControlButton>
          }
        </Controls>
        {(full ||
          (minimap ?? (typeof height === "number" && height >= 400))) && (
          <MiniMap
            pannable
            zoomable
            nodeColor={nodeColor}
            nodeStrokeWidth={2}
            // The area outside the viewport, which react-flow washes out with a
            // near-white by default — on a dark canvas that turns the minimap
            // into a bright grey slab covering the graph behind it.
            maskColor={
              theme.palette.mode === "dark"
                ? "rgba(0, 0, 0, 0.55)"
                : "rgba(240, 240, 240, 0.6)"
            }
            bgColor={theme.palette.background.default}
            // Explicitly small and see-through. react-flow's default is a fixed
            // 200×150 panel, which is a corner of a full-width canvas and most
            // of a half-width one — on the split run view it covered the part of
            // the flowchart it was there to help you find.
            style={{ width: 132, height: 88, opacity: 0.85 }}
          />
        )}
      </ReactFlow>
    </Box>
  );
}

/** How a graph is fitted to its canvas. */
const FIT = { padding: 0.08, maxZoom: 1.15 };

/** The smallest zoom a graph is first shown at. */
const READABLE_ZOOM = 0.6;

/**
 * Choose the view, in one fixed order: fit the graph, then hold the zoom at a
 * readable size, then bring `focus` into sight.
 *
 * Open a graph readable rather than complete: fitting a twelve-column run into
 * half a screen draws every label a few pixels high, so when the fit would go
 * below a readable zoom the zoom is held there and the view anchored on the
 * graph's left edge, where the run starts. The rest is a pan away, and the fit
 * button still shows the whole thing.
 *
 * Then the focused node (see `GraphCanvas`'s `focus`): panned to, and zoomed
 * in to a readable size, when it is off the canvas or too small to read; left
 * alone when it is already in sight.
 *
 * These used to be separate effects racing react-flow's own initial fit, and
 * whichever ran last won — so on some canvases the graph opened as a smear of
 * unreadable nodes. Doing the fit here, and awaiting it, takes the race away.
 * It is chosen again whenever `refit` changes (entering or leaving full
 * screen), and the focus again whenever it changes.
 */
function ViewControl({
  focus,
  refit,
}: {
  focus: string | null;
  refit: boolean;
}) {
  const initialized = useNodesInitialized();
  const flow = useReactFlow();
  const width = useStore((state) => state.width);
  const height = useStore((state) => state.height);
  const fitted = useRef<boolean | null>(null);

  const bringIntoView = useCallback(
    (id: string) => {
      const node = flow.getNode(id);
      if (!node) return;
      const w = node.measured?.width ?? node.width ?? 0;
      const h = node.measured?.height ?? node.height ?? 0;
      const { x, y, zoom } = flow.getViewport();
      const left = node.position.x * zoom + x;
      const top = node.position.y * zoom + y;
      const inside =
        left >= 0 &&
        top >= 0 &&
        left + w * zoom <= width &&
        top + h * zoom <= height;
      if (inside && zoom >= READABLE_ZOOM) return;
      void flow.setCenter(node.position.x + w / 2, node.position.y + h / 2, {
        zoom: Math.max(zoom, READABLE_ZOOM),
        duration: 300,
      });
    },
    [flow, width, height],
  );

  // The whole view: on mount, and on entering or leaving full screen — the
  // latter only once the canvas has actually taken its new size, or the fit
  // is worked out for the old one.
  const sizedAt = useRef<string | null>(null);
  useEffect(() => {
    if (!initialized || width === 0 || height === 0) return;
    const size = `${width}x${height}`;
    const toggled = fitted.current !== null && fitted.current !== refit;
    if (fitted.current !== null && !toggled) {
      sizedAt.current = size;
      return;
    }
    if (toggled && size === sizedAt.current) return;
    fitted.current = refit;
    sizedAt.current = size;
    let live = true;
    const frame = () => new Promise((done) => requestAnimationFrame(done));
    void (async () => {
      await flow.fitView(FIT);
      // react-flow applies a fit on its next update; read the view after it.
      await frame();
      if (!live) return;
      if (flow.getViewport().zoom < READABLE_ZOOM) {
        const bounds = flow.getNodesBounds(flow.getNodes());
        await flow.setViewport({
          zoom: READABLE_ZOOM,
          x: 24 - bounds.x * READABLE_ZOOM,
          y: height / 2 - (bounds.y + bounds.height / 2) * READABLE_ZOOM,
        });
        await frame();
      }
      if (live && focus) bringIntoView(focus);
    })();
    return () => {
      live = false;
    };
    // `focus` is read, not followed: a new focus only pans (below).
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [initialized, flow, width, height, refit]);

  // A new focus once the view is chosen.
  useEffect(() => {
    if (fitted.current === null || !focus) return;
    bringIntoView(focus);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [focus]);

  return null;
}

/** Room left between a node brought into view and the canvas edge, in pixels. */
const VIEW_MARGIN = 24;

/**
 * When `fitKey` changes, pan — never zoom — just far enough that every node
 * which moved or appeared is on screen. Nothing that stayed put is looked at,
 * so a graph already wider than the canvas doesn't pan on every edit.
 */
function KeepInView({ fitKey }: { fitKey?: string | number }) {
  const flow = useReactFlow();
  const width = useStore((state) => state.width);
  const height = useStore((state) => state.height);
  const seen = useRef<Map<string, { x: number; y: number }> | null>(null);

  useEffect(() => {
    // After react-flow has taken the new nodes into its store.
    const frame = requestAnimationFrame(() => {
      const nodes = flow.getNodes();
      const before = seen.current;
      seen.current = new Map(nodes.map((node) => [node.id, node.position]));
      if (!before) return;
      const moved = nodes.filter((node) => {
        const was = before.get(node.id);
        return !was || was.x !== node.position.x || was.y !== node.position.y;
      });
      if (moved.length === 0) return;

      const bounds = flow.getNodesBounds(moved);
      const { x, y, zoom } = flow.getViewport();
      const margin = VIEW_MARGIN / zoom;
      const left = -x / zoom + margin;
      const top = -y / zoom + margin;
      const right = (width - x) / zoom - margin;
      const bottom = (height - y) / zoom - margin;
      // Whichever edge is out, by how much; the left and top win when the
      // nodes are bigger than the view.
      const dx =
        bounds.x < left
          ? left - bounds.x
          : bounds.x + bounds.width > right
            ? right - (bounds.x + bounds.width)
            : 0;
      const dy =
        bounds.y < top
          ? top - bounds.y
          : bounds.y + bounds.height > bottom
            ? bottom - (bounds.y + bounds.height)
            : 0;
      if (dx === 0 && dy === 0) return;
      void flow.setViewport(
        { x: x + dx * zoom, y: y + dy * zoom, zoom },
        { duration: 250 },
      );
    });
    return () => cancelAnimationFrame(frame);
  }, [fitKey, flow, width, height]);

  return null;
}
