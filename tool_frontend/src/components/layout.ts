/**
 * Node positioning for the graph views.
 *
 * Deliberately hand-rolled rather than pulling in dagre or elk: the graphs here
 * are small (tens to low hundreds of nodes) and have known shapes, and a layout
 * engine would be a large dependency baked into the Rust binary for very little.
 */

import { Position, type Edge, type Node } from "@xyflow/react";

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
  pathOptions: { borderRadius: 14 },
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

/**
 * Layered left-to-right layout by longest-path depth.
 *
 * Suits DAGs — a run flowchart or a dependency graph — where an edge means
 * "comes after" and depth is the meaningful axis.
 */
export function layeredLayout(
  ids: string[],
  edges: { source: string; target: string }[],
  data: (id: string) => Record<string, unknown>,
  options: {
    columnWidth?: number;
    rowHeight?: number;
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
): Node[] {
  const columnWidth = options.columnWidth ?? 260;
  const rowHeight = options.rowHeight ?? 76;
  const isBottom = options.bottom ?? (() => false);

  const layered = ids.filter((id) => !isBottom(id));
  const parked = ids.filter(isBottom);

  const depth = computeDepths(layered, edges);

  // Bucket by depth, then stack each column.
  const columns = new Map<number, string[]>();
  for (const id of layered) {
    const d = depth.get(id) ?? 0;
    const column = columns.get(d) ?? [];
    column.push(id);
    columns.set(d, column);
  }

  const nodes: Node[] = [];
  for (const [d, column] of columns) {
    column.forEach((id, index) => {
      nodes.push({
        id,
        data: data(id),
        position: {
          x: d * columnWidth,
          // Centre each column vertically against the others.
          y: (index - (column.length - 1) / 2) * rowHeight,
        },
        type: "default",
        // The graph runs left to right, so edges must leave the right side and
        // arrive at the left. With react-flow's default top/bottom handles
        // every edge doubles back on itself into an S — the "comes after"
        // direction is exactly the thing that stops being readable.
        sourcePosition: Position.Right,
        targetPosition: Position.Left,
      });
    });
  }

  // The parked row, clear of the deepest column the layered nodes reached.
  if (parked.length > 0) {
    const tallest = Math.max(1, ...[...columns.values()].map((column) => column.length));
    const y = ((tallest - 1) / 2) * rowHeight + rowHeight * 2;
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

  return nodes;
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
