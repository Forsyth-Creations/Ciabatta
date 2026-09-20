/**
 * Node positioning for the graph views.
 *
 * Deliberately hand-rolled rather than pulling in dagre or elk: the graphs here
 * are small (tens to low hundreds of nodes) and have known shapes, and a layout
 * engine would be a large dependency baked into the Rust binary for very little.
 */

import { MarkerType, Position, type Edge, type Node } from "@xyflow/react";

/**
 * Edge styling for the layered graphs: orthogonal segments with rounded
 * corners, rather than react-flow's default bezier.
 *
 * With nodes in columns, a curve between two of them wanders through the space
 * where other edges are; a right-angled route reads as wiring and stays legible
 * when a dozen of them share a lane. Spread this into an edge and override
 * `style` as needed.
 */
export const ORTHOGONAL_EDGE = {
  type: "smoothstep" as const,
  pathOptions: {
    borderRadius: 14,
    // How far an edge runs straight out of a node before it turns. Larger than
    // react-flow's default so the turn happens in the gap between columns
    // rather than against the node's edge, which is what lets several edges
    // arriving at one node fan out instead of converging into a single stroke
    // three nodes back.
    offset: 24,
  },
  markerEnd: {
    type: MarkerType.ArrowClosed,
    width: 14,
    height: 14,
  },
};

/**
 * Two-ring radial layout: a small set of "hub" nodes on an inner circle, with
 * everything they connect to spread around an outer one.
 *
 * Suits the AI mind map, where a handful of architectures each own many files.
 */
export function radialLayout(
  hubs: { id: string; data: Record<string, unknown> }[],
  leaves: { id: string; data: Record<string, unknown>; hub: string | null }[],
): Node[] {
  const nodes: Node[] = [];

  const hubRadius = Math.max(160, hubs.length * 46);
  hubs.forEach((hub, index) => {
    const angle = (index / Math.max(1, hubs.length)) * Math.PI * 2 - Math.PI / 2;
    nodes.push({
      id: hub.id,
      data: hub.data,
      position: { x: Math.cos(angle) * hubRadius, y: Math.sin(angle) * hubRadius },
      type: "default",
    });
  });

  // Group leaves by their hub so each cluster sits near the hub that owns it.
  const byHub = new Map<string, typeof leaves>();
  for (const leaf of leaves) {
    const key = leaf.hub ?? "__orphans__";
    const list = byHub.get(key) ?? [];
    list.push(leaf);
    byHub.set(key, list);
  }

  // The leaf ring has to clear the hub ring by a wide margin, not a fixed
  // offset: with many hubs the inner circle is already large, and a constant
  // gap leaves the two rings visually interleaved.
  const leafRadius = hubRadius * 1.9 + Math.max(220, leaves.length * 4);
  for (const [hubId, group] of byHub) {
    const hubIndex = hubs.findIndex((h) => h.id === hubId);
    // Orphans (and any hub we can't place) fan out from the bottom.
    const centreAngle =
      hubIndex >= 0
        ? (hubIndex / Math.max(1, hubs.length)) * Math.PI * 2 - Math.PI / 2
        : Math.PI / 2;
    // Each cluster gets a slice of the circle proportional to nothing in
    // particular — an even share keeps dense clusters from overlapping sparse
    // neighbours.
    const spread = (Math.PI * 2) / Math.max(1, byHub.size);

    group.forEach((leaf, index) => {
      const offset = group.length === 1 ? 0 : (index / (group.length - 1) - 0.5) * spread * 0.9;
      const angle = centreAngle + offset;
      // Stagger across three radii so long labels in a dense cluster don't
      // collide with their neighbours.
      const radius = leafRadius + (index % 3) * 110;
      nodes.push({
        id: leaf.id,
        data: leaf.data,
        position: { x: Math.cos(angle) * radius, y: Math.sin(angle) * radius },
        type: "default",
      });
    });
  }

  return nodes;
}

/** The id of the waypoint an edge takes through column `depth`. */
const waypointId = (source: string, target: string, depth: number) =>
  `wp::${source}->${target}::${depth}`;

/** Whether a node is a routing waypoint rather than something in the graph. */
export const isWaypoint = (id: string) => id.startsWith("wp::");

/** The key a routed edge's waypoints are listed under. */
export const routeKey = (source: string, target: string) => `${source}->${target}`;

/** A laid-out graph: the nodes to draw, and the lanes long edges take. */
export interface LayeredGraph {
  nodes: Node[];
  /**
   * For each edge that spans more than one column, the waypoint ids it passes
   * through in order — so the caller can draw it as a chain of segments
   * instead of one stroke straight across the columns in between.
   */
  routes: Map<string, string[]>;
}

/**
 * Layered left-to-right layout by longest-path depth.
 *
 * Suits DAGs — a run flowchart or a dependency graph — where an edge means
 * "comes after" and depth is the meaningful axis.
 *
 * Edges that skip columns are **routed** rather than drawn through them. A
 * `checkout → publish` edge across a six-column build is a straight line over
 * every node between them, and the drawing then says two false things: that the
 * edge has something to do with the nodes it crosses, and that a node with a
 * line through it is connected to something. The fix is the standard one —
 * dummy nodes at each intermediate column, which take part in the ordering and
 * the spacing like any other node and so reserve a lane of their own for the
 * wire to run down. The dummies are invisible; the caller draws the edge as the
 * chain of segments between them.
 */
export function layeredLayout(
  ids: string[],
  edges: { source: string; target: string }[],
  data: (id: string) => Record<string, unknown>,
  options: {
    columnWidth?: number;
    rowHeight?: number;
    /**
     * How tall a node is, used to centre a routed edge's lane on the gap
     * between two rows rather than on their top edges — react-flow hangs a
     * node's handles off the middle of its box but positions it by its corner.
     */
    nodeHeight?: number;
    /**
     * Force a node into a particular column, overriding its computed depth.
     *
     * For nodes that aren't steps in the flow but inputs to it — a variable, a
     * set of files — which belong in a column of their own to the left rather
     * than sharing the first wave with whatever happens to start the run.
     * Pinning them to a negative depth gives them that column *and* keeps them
     * inside the layering, so the edges leaving them are routed like every
     * other edge instead of being drawn across the graph.
     */
    pin?: (id: string) => number | undefined;
    /**
     * Nodes to lift out of the layering and park in a row underneath it.
     *
     * For nodes that are in the picture but not in the flow — background
     * tasks, which nothing waits for. Depth is "how far along the run is this",
     * and a node that gates nothing has no honest answer, so placing it in a
     * column claims an ordering that isn't there.
     */
    bottom?: (id: string) => boolean;
  } = {},
): LayeredGraph {
  const columnWidth = options.columnWidth ?? 300;
  const rowHeight = options.rowHeight ?? 76;
  const nodeHeight = options.nodeHeight ?? 46;
  const isBottom = options.bottom ?? (() => false);

  const layered = ids.filter((id) => !isBottom(id));
  const parked = ids.filter(isBottom);

  const depth = computeDepths(layered, edges);
  if (options.pin) {
    for (const id of layered) {
      const pinned = options.pin(id);
      if (pinned !== undefined) depth.set(id, pinned);
    }
  }

  // Replace every column-skipping edge with a chain through one waypoint per
  // column it would otherwise have crossed. From here on the layout works on
  // the expanded graph, which is what gives those waypoints rows of their own.
  const routes = new Map<string, string[]>();
  const waypointDepth = new Map<string, number>();
  const routed: { source: string; target: string }[] = [];
  for (const edge of edges) {
    const from = depth.get(edge.source);
    const to = depth.get(edge.target);
    if (from === undefined || to === undefined || to - from <= 1) {
      if (from !== undefined && to !== undefined) routed.push(edge);
      continue;
    }
    const lane: string[] = [];
    for (let d = from + 1; d < to; d++) {
      const id = waypointId(edge.source, edge.target, d);
      lane.push(id);
      waypointDepth.set(id, d);
    }
    routes.set(routeKey(edge.source, edge.target), lane);
    const chain = [edge.source, ...lane, edge.target];
    for (let i = 0; i + 1 < chain.length; i++) {
      routed.push({ source: chain[i], target: chain[i + 1] });
    }
  }

  // Bucket by depth, then stack each column.
  const columns = new Map<number, string[]>();
  for (const id of [...layered, ...waypointDepth.keys()]) {
    const d = waypointDepth.get(id) ?? depth.get(id) ?? 0;
    const column = columns.get(d) ?? [];
    column.push(id);
    columns.set(d, column);
  }

  // Columns in depth order, which is what the sweeps below walk. The map is in
  // whatever order the nodes were declared in, and a sweep that visits depth 3
  // before depth 2 is not sweeping.
  const depths = [...columns.keys()].sort((a, b) => a - b);
  const order = depths.map((d) => columns.get(d)!);

  // Only edges whose both ends are in the layering, since these drive both the
  // ordering and the positioning and an edge to a parked node would pull on a
  // row that isn't there. Waypoints are in the layering by construction.
  const placed = new Set([...depth.keys(), ...waypointDepth.keys()]);
  const inner = routed.filter((e) => placed.has(e.source) && placed.has(e.target));

  reduceCrossings(order, inner);
  // A lane only has to clear the wires either side of it, so waypoints are
  // packed closer than nodes: giving each one a full row would push a busy
  // graph apart to make room for empty space.
  const rows = assignRows(order, inner, rowHeight, (id) =>
    isWaypoint(id) ? WAYPOINT_LANE : rowHeight,
  );

  const nodes: Node[] = [];
  order.forEach((column, index) => {
    const d = depths[index];
    for (const id of column) {
      const waypoint = isWaypoint(id);
      nodes.push({
        id,
        data: waypoint ? {} : data(id),
        position: {
          x: d * columnWidth + (waypoint ? columnWidth / 2 : 0),
          // A node's handles hang off the middle of its box, a waypoint's off a
          // point — so a lane between two rows has to be dropped half a node to
          // line up with the handles it joins.
          y: (rows.get(id) ?? 0) + (waypoint ? nodeHeight / 2 : 0),
        },
        type: "default",
        // The graph runs left to right, so edges must leave the right side and
        // arrive at the left. With react-flow's default top/bottom handles
        // every edge doubles back on itself into an S — the "comes after"
        // direction is exactly the thing that stops being readable.
        sourcePosition: Position.Right,
        targetPosition: Position.Left,
        ...(waypoint
          ? {
              // Invisible, and inert: it is a bend in a wire, not something to
              // click, select, or find in the minimap.
              style: { width: 1, height: 1, opacity: 0, pointerEvents: "none" as const },
              selectable: false,
              focusable: false,
              draggable: false,
              deletable: false,
            }
          : {}),
      });
    }
  });

  // The parked row, clear of the lowest any layered node reached.
  if (parked.length > 0) {
    const lowest = Math.max(0, ...[...rows.values()]);
    const y = lowest + rowHeight * 2;
    parked.forEach((id, index) => {
      nodes.push({
        id,
        data: data(id),
        position: { x: index * columnWidth, y },
        type: "default",
        sourcePosition: Position.Right,
        targetPosition: Position.Left,
      });
    });
  }

  return { nodes, routes };
}

/** Vertical room a routed edge's lane takes in a column. */
const WAYPOINT_LANE = 26;

/**
 * One logical edge as the chain of segments its route takes.
 *
 * An edge that skips no columns comes back as a single segment from its own
 * ends, so callers draw every edge the same way whether it was routed or not.
 * `last` marks the segment that gets the arrowhead: putting one on each would
 * draw three arrows into the empty space where the lane bends.
 */
export function routeSegments(
  routes: Map<string, string[]>,
  source: string,
  target: string,
): { source: string; target: string; first: boolean; last: boolean }[] {
  const lane = routes.get(routeKey(source, target)) ?? [];
  const chain = [source, ...lane, target];
  return chain.slice(0, -1).map((from, index) => ({
    source: from,
    target: chain[index + 1],
    first: index === 0,
    last: index === chain.length - 2,
  }));
}

/** How many ordering sweeps to run before taking the best result. */
const SWEEPS = 8;

/**
 * Reorder each column, in place, to cut the number of edges that cross.
 *
 * The layering alone decides which column a node is in; it says nothing about
 * where in the column it sits, and the answer used to be "wherever it was
 * declared". On a four-node graph that is fine. On a monorepo graph — forty
 * steps across six packages, every package's `compile` feeding every package's
 * `test` — declaration order interleaves the packages and every edge crosses
 * most of the others. The picture is technically correct and completely
 * unreadable, which is the complaint this answers.
 *
 * The method is the standard one (Sugiyama's second phase, by barycentres):
 * sweep forward putting each node at the average height of the nodes feeding
 * it, sweep back putting it at the average height of the nodes it feeds, and
 * keep whichever pass crossed least. It is a heuristic — minimising crossings
 * exactly is NP-hard — but it reliably turns that tangle into something with
 * visible lanes, and it is thirty lines rather than a layout engine in the
 * bundle.
 */
function reduceCrossings(order: string[][], edges: { source: string; target: string }[]): void {
  if (order.length < 2) return;

  const successors = new Map<string, string[]>();
  const predecessors = new Map<string, string[]>();
  for (const edge of edges) {
    successors.set(edge.source, [...(successors.get(edge.source) ?? []), edge.target]);
    predecessors.set(edge.target, [...(predecessors.get(edge.target) ?? []), edge.source]);
  }

  let best = order.map((column) => [...column]);
  let fewest = countCrossings(order, edges);

  for (let sweep = 0; sweep < SWEEPS; sweep++) {
    // Forward, then back. Alternating matters: a forward-only sweep settles
    // the later columns against the earlier ones and never asks whether the
    // earlier ones could move to suit.
    const neighbours = sweep % 2 === 0 ? predecessors : successors;
    const columns =
      sweep % 2 === 0
        ? order.map((_, index) => index).slice(1)
        : order
            .map((_, index) => index)
            .slice(0, -1)
            .reverse();

    for (const index of columns) {
      const fixed = new Map(
        order[sweep % 2 === 0 ? index - 1 : index + 1].map((id, position) => [id, position]),
      );
      // A node with no neighbours in the fixed column has no barycentre, so it
      // keeps the position it has rather than being swept to the top.
      const keys = new Map<string, number>();
      order[index].forEach((id, position) => {
        const linked = (neighbours.get(id) ?? [])
          .map((other) => fixed.get(other))
          .filter((p): p is number => p !== undefined);
        keys.set(id, linked.length === 0 ? position : mean(linked));
      });
      // Stable, so nodes that tie keep the order they already had — which on
      // the first sweep is declaration order, the most meaningful tiebreak
      // available.
      order[index] = [...order[index]].sort((a, b) => keys.get(a)! - keys.get(b)!);
    }

    const crossings = countCrossings(order, edges);
    if (crossings < fewest) {
      fewest = crossings;
      best = order.map((column) => [...column]);
    }
    if (fewest === 0) break;
  }

  best.forEach((column, index) => {
    order[index] = column;
  });
}

/**
 * Edge pairs that cross, summed over every adjacent pair of columns.
 *
 * Two edges between the same pair of columns cross exactly when their endpoints
 * are in opposite orders, so this counts inversions in the target positions
 * after sorting by source position. Quadratic in the edges between one pair of
 * columns, which on graphs of this size is nothing, and it is only ever
 * compared against itself.
 */
function countCrossings(order: string[][], edges: { source: string; target: string }[]): number {
  const position = new Map<string, { column: number; row: number }>();
  order.forEach((column, index) => {
    column.forEach((id, row) => position.set(id, { column: index, row }));
  });

  let crossings = 0;
  for (let index = 0; index + 1 < order.length; index++) {
    const between = edges
      .map((edge) => ({ from: position.get(edge.source), to: position.get(edge.target) }))
      .filter((e) => e.from?.column === index && e.to?.column === index + 1)
      .map((e) => [e.from!.row, e.to!.row] as const)
      .sort((a, b) => a[0] - b[0] || a[1] - b[1]);

    for (let a = 0; a < between.length; a++) {
      for (let b = a + 1; b < between.length; b++) {
        if (between[a][1] > between[b][1]) crossings++;
      }
    }
  }
  return crossings;
}

/**
 * The y position of every node, once the columns are ordered.
 *
 * Index times row height would keep the ordering and throw away the reason for
 * it: a node whose only parent sits four rows up is drawn level with the top of
 * its own column, and the edge between them is a long diagonal across
 * everything in between. So each node is pulled towards the average height of
 * what it connects to, and then each column is pushed apart again until nothing
 * overlaps.
 *
 * The push-apart is what keeps this honest — barycentres alone will happily
 * stack two nodes on the same pixel — and doing it after every relaxation pass
 * rather than once at the end stops the two from fighting.
 */
function assignRows(
  order: string[][],
  edges: { source: string; target: string }[],
  rowHeight: number,
  heightOf: (id: string) => number = () => rowHeight,
): Map<string, number> {
  const rows = new Map<string, number>();
  for (const column of order) {
    column.forEach((id, index) => rows.set(id, index * rowHeight));
  }

  const successors = new Map<string, string[]>();
  const predecessors = new Map<string, string[]>();
  for (const edge of edges) {
    successors.set(edge.source, [...(successors.get(edge.source) ?? []), edge.target]);
    predecessors.set(edge.target, [...(predecessors.get(edge.target) ?? []), edge.source]);
  }

  for (let pass = 0; pass < SWEEPS; pass++) {
    const forward = pass % 2 === 0;
    const columns = forward ? order : [...order].reverse();

    for (const column of columns) {
      for (const id of column) {
        const linked = (forward ? predecessors : successors).get(id) ?? [];
        const heights = linked
          .map((other) => rows.get(other))
          .filter((y): y is number => y !== undefined);
        if (heights.length > 0) rows.set(id, mean(heights));
      }
    }

    // Separate, in order, so the ordering the sweeps chose survives being
    // pulled around by the barycentres.
    for (const column of order) separate(column, rows, heightOf);
  }

  // Centre the whole thing on zero, so the canvas opens on the graph rather
  // than below it.
  const values = [...rows.values()];
  if (values.length > 0) {
    const middle = (Math.min(...values) + Math.max(...values)) / 2;
    for (const [id, y] of rows) rows.set(id, y - middle);
  }

  return rows;
}

/**
 * Push a column's nodes apart until none overlaps, keeping the column centred
 * where the barycentres wanted it.
 *
 * The push itself walks top to bottom, which on its own anchors the column to
 * whichever node happens to be first and slides every other one down: the two
 * halves of a diamond come out as "level with the parent" and "one row below
 * it" rather than straddling it, and the parent then reads as belonging to the
 * upper branch. Shifting the column back by the average displacement undoes
 * exactly that bias without disturbing the order or the spacing.
 */
function separate(
  column: string[],
  rows: Map<string, number>,
  heightOf: (id: string) => number,
): void {
  if (column.length < 2) return;

  const wanted = column.map((id) => rows.get(id)!);
  const placed = [...wanted];
  for (let index = 1; index < placed.length; index++) {
    // The gap a pair needs is the taller of the two, so a wire lane squeezed
    // between two nodes still clears both of them.
    const gap = Math.max(heightOf(column[index - 1]), heightOf(column[index]));
    placed[index] = Math.max(placed[index], placed[index - 1] + gap);
  }

  const drift = mean(placed) - mean(wanted);
  column.forEach((id, index) => rows.set(id, placed[index] - drift));
}

function mean(values: number[]): number {
  return values.reduce((total, value) => total + value, 0) / values.length;
}

/**
 * The position each node takes in the run's execution sequence, numbered from 1.
 *
 * This mirrors what the engine actually does rather than inventing an order for
 * display: it runs one wave at a time — every step whose `needs` are satisfied —
 * and runs the steps within a wave serially in declaration order. So a node's
 * wave is its longest-path depth, and `ids` in declaration order breaks the tie.
 *
 * Nodes the engine never schedules (recovery branches) belong out of `ids`; they
 * only run if something fails, so they have no place in the sequence.
 */
export function executionOrder(
  ids: string[],
  edges: { source: string; target: string }[],
  options: { exclude?: (id: string) => boolean } = {},
): Map<string, number> {
  if (options.exclude) ids = ids.filter((id) => !options.exclude!(id));
  const depth = computeDepths(ids, edges);
  // Array#sort is stable, so equal depths keep their declaration order.
  const sequence = [...ids].sort((a, b) => (depth.get(a) ?? 0) - (depth.get(b) ?? 0));
  return new Map(sequence.map((id, index) => [id, index + 1]));
}

/**
 * Longest-path depth for each node.
 *
 * Iterative relaxation rather than a topological sort, because these graphs
 * aren't guaranteed acyclic — a malformed workflow or a dependency cycle should
 * still render something rather than throw. The pass count bounds the work if a
 * cycle is present.
 */
function computeDepths(ids: string[], edges: { source: string; target: string }[]): Map<string, number> {
  const depth = new Map<string, number>(ids.map((id) => [id, 0]));

  for (let pass = 0; pass < ids.length; pass++) {
    let changed = false;
    for (const edge of edges) {
      const from = depth.get(edge.source);
      const to = depth.get(edge.target);
      if (from === undefined || to === undefined) continue;
      if (to < from + 1) {
        depth.set(edge.target, from + 1);
        changed = true;
      }
    }
    if (!changed) break;
  }

  return depth;
}

/** Convenience: turn `{from,to}` pairs into react-flow edges. */
export function toEdges(
  pairs: { from: string; to: string }[],
  options: { animated?: boolean; dashed?: (pair: { from: string; to: string }) => boolean } = {},
): Edge[] {
  return pairs.map((pair, index) => ({
    id: `${pair.from}->${pair.to}-${index}`,
    source: pair.from,
    target: pair.to,
    animated: options.animated ?? false,
    style: options.dashed?.(pair) ? { strokeDasharray: "4 4" } : undefined,
  }));
}
