/**
 * Node positioning for the graph views.
 *
 * Deliberately hand-rolled rather than pulling in dagre or elk: the graphs here
 * are small (tens to low hundreds of nodes) and have known shapes, and a layout
 * engine would be a large dependency baked into the Rust binary for very little.
 */

import { MarkerType, Position, type Edge, type Node } from "@xyflow/react";


/**
 * Edge defaults for the layered graphs: drawn by `RoutedEdge` along the route
 * `layeredLayout` worked out, rather than by react-flow's own path functions.
 *
 * react-flow can only draw an edge as a function of its two ends. That is fine
 * for a pair of nodes and wrong for a graph: whether an edge can run straight,
 * where it may turn, and which other edges it can share a wire with all depend
 * on everything else on the canvas. So the layout decides the route, and the
 * edge only draws it. Spread this into an edge, then set `data.route` and
 * `style`.
 */
export const ROUTED_EDGE = {
  type: "routed" as const,
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

/** An edge as the layout sees it. */
export interface LayoutEdge {
  source: string;
  target: string;
  /**
   * The family of wire this is: `needs`, a variable's, a file set's. Only wires
   * of one family share a trunk, because they are drawn differently: a dashed
   * dependency running down the same lane as a solid `needs` edge reads as one
   * wire changing its mind halfway.
   */
  group?: string;
  /**
   * Whether wires from different sources converging on one target may share
   * the last vertical into it. Only for solid strokes: a dash pattern starts
   * where its path does, so two dashed edges overlapping from different starts
   * draw two dash patterns out of phase, which reads as a smudge.
   */
  merge?: boolean;
  /**
   * False for an edge that is drawn but has no say in which column anything
   * goes in — a retry, which points back at the step it re-runs and would
   * otherwise make the graph a cycle.
   */
  rank?: boolean;
}

/**
 * Where one edge goes, beyond its two ends — everything `RoutedEdge` needs to
 * draw it. The ends themselves come from react-flow, which measures them.
 */
export interface EdgeRoute {
  /** The centre line of the lane taken through each column passed through. */
  lanes: number[];
  /** The x of the vertical run in each gap crossed: one more than `lanes`. */
  tracks: number[];
  /**
   * For an edge that points backwards, or at its own column: the height it
   * loops over at, clear of the tops of both nodes.
   */
  loop?: number;
  /**
   * Draw the path from the target end. A dash pattern starts where its path
   * does, so dashed wires that share their *last* stretch — several sources
   * converging on one target — only line up if they are all drawn from there.
   */
  fromTarget?: boolean;
}

/** The key an edge's route is listed under. */
export const routeKey = (source: string, target: string) => `${source}->${target}`;

/** A laid-out graph: the nodes to draw, and the route each edge takes. */
export interface LayeredGraph {
  nodes: Node[];
  routes: Map<string, EdgeRoute>;
}

/**
 * The id of the lane a trunk takes through column `depth`. A trunk is one
 * source's wires fanning out (`owner` is the source), or several sources' wires
 * converging (`owner` is `>` and the target).
 */
const laneId = (group: string, owner: string, depth: number) =>
  `lane::${group}::${owner}::${depth}`;

const isLane = (id: string) => id.startsWith("lane::");

/** Vertical room between two lanes running side by side through a column. */
const LANE_GAP = 10;
/** Vertical room between a lane and a node it runs past. */
const LANE_CLEARANCE = 16;
/** Horizontal room between neighbouring verticals in a gap. */
const TRACK_SPACING = 10;
/** How close a vertical may come to the column on its left… */
const GAP_MARGIN_LEFT = 14;
/** …and on its right, which is further: the arrowhead needs a run-up. */
const GAP_MARGIN_RIGHT = 26;
/** How far above the higher of its two nodes a backwards edge loops. */
const LOOP_LIFT = 18;

/**
 * Layered left-to-right layout by longest-path depth, with every edge routed.
 *
 * Suits DAGs — a run flowchart or a dependency graph — where an edge means
 * "comes after" and depth is the meaningful axis. Three decisions make the
 * wiring legible rather than merely correct:
 *
 * **Edges that skip columns take lanes**, rather than cutting across the
 * columns in between. A line drawn over a node says it has something to do
 * with that node. Each skipped column gets a dummy entry that takes part in the
 * ordering and spacing like a node, and so reserves a row for the wire.
 *
 * **Wires share trunks.** A variable read by six steps is one statement with
 * six readers, and drawing it as six parallel wires made the graph look six
 * times as busy as it is. So a source's wires run as one trunk and branch off
 * as each reaches its column. And the reverse: a source with only the one wire
 * has nothing to fan out, so its wire joins the trunk *into* its target, and
 * four credentials feeding one push arrive as one wire rather than four. Only
 * wires of one family share (see `LayoutEdge.group`), and a trunk is only ever
 * one of the two kinds — a lane shared by both would draw connections between
 * sources and targets that don't exist.
 *
 * **Every gap's verticals get their own tracks.** Where a wire changes height
 * between two columns it needs a vertical, and two verticals at one x draw as a
 * single line that seems to connect everything either of them touches. Each
 * gap's verticals are given distinct x positions, ordered to cross as little as
 * possible, and shared only where the sharing is true — one source fanning out,
 * or several converging on one target.
 *
 * Positions are by a node's *centre*, which is where react-flow hangs its
 * handles, so a lane at a node's height meets its handle exactly instead of a
 * few pixels off. That needs each node's height up front: pass `heightOf`, and
 * give the nodes that height.
 */
export function layeredLayout(
  ids: string[],
  edges: LayoutEdge[],
  data: (id: string) => Record<string, unknown>,
  options: {
    columnWidth?: number;
    /** How wide a node is drawn; the gap between columns is what's left. */
    nodeWidth?: number;
    /** Vertical room between two nodes in a column. */
    rowGap?: number;
    /** How tall a node is drawn. Defaults to `nodeHeight` for every node. */
    heightOf?: (id: string) => number;
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
  const nodeWidth = options.nodeWidth ?? 150;
  const rowGap = options.rowGap ?? 30;
  const nodeHeight = options.nodeHeight ?? 46;
  const isBottom = options.bottom ?? (() => false);
  const heightOf = (id: string) => (isLane(id) ? 0 : (options.heightOf?.(id) ?? nodeHeight));
  // Centre to centre: half of each, plus the room the pair needs between them.
  const distance = (a: string, b: string) =>
    heightOf(a) / 2 +
    heightOf(b) / 2 +
    (isLane(a) && isLane(b) ? LANE_GAP : isLane(a) || isLane(b) ? LANE_CLEARANCE : rowGap);

  const layered = ids.filter((id) => !isBottom(id));
  const parked = ids.filter(isBottom);

  const depth = computeDepths(
    layered,
    edges.filter((edge) => edge.rank !== false),
  );
  if (options.pin) {
    for (const id of layered) {
      const pinned = options.pin(id);
      if (pinned !== undefined) depth.set(id, pinned);
    }
  }

  // Expand every forward edge into the chain of lanes it takes. Lanes are
  // shared by source and family, so a chain can share links with its siblings;
  // `links` holds each once, which is what the ordering should count.
  // How many wires each source has in each family. A source with one has
  // nothing to fan out, so its wire converges with the others into its target.
  const fanOut = new Map<string, number>();
  const family = (edge: LayoutEdge) => `${edge.group ?? ""}\u0000${edge.source}`;
  for (const edge of edges) fanOut.set(family(edge), (fanOut.get(family(edge)) ?? 0) + 1);
  const converges = (edge: LayoutEdge) => fanOut.get(family(edge)) === 1;

  const laneDepth = new Map<string, number>();
  const chains = new Map<string, { edge: LayoutEdge; chain: string[]; from: number }>();
  const links = new Map<string, { source: string; target: string }>();
  for (const edge of edges) {
    const from = depth.get(edge.source);
    const to = depth.get(edge.target);
    if (from === undefined || to === undefined || to <= from) continue;
    const lanes: string[] = [];
    for (let d = from + 1; d < to; d++) {
      const owner = converges(edge) ? `>${edge.target}` : edge.source;
      const id = laneId(edge.group ?? "", owner, d);
      laneDepth.set(id, d);
      lanes.push(id);
    }
    const chain = [edge.source, ...lanes, edge.target];
    chains.set(routeKey(edge.source, edge.target), { edge, chain, from });
    for (let i = 0; i + 1 < chain.length; i++) {
      links.set(`${chain[i]}\u0000${chain[i + 1]}`, { source: chain[i], target: chain[i + 1] });
    }
  }
  const inner = [...links.values()];

  // Bucket by depth, then stack each column.
  const columns = new Map<number, string[]>();
  for (const id of [...layered, ...laneDepth.keys()]) {
    const d = laneDepth.get(id) ?? depth.get(id) ?? 0;
    const column = columns.get(d) ?? [];
    column.push(id);
    columns.set(d, column);
  }

  // Columns in depth order, which is what the sweeps below walk. The map is in
  // whatever order the nodes were declared in, and a sweep that visits depth 3
  // before depth 2 is not sweeping.
  const depths = [...columns.keys()].sort((a, b) => a - b);
  const order = depths.map((d) => columns.get(d)!);
  // Where each depth is drawn: by its place among the depths that have
  // anything in them, so a pinned column with nothing to show (variables on,
  // no file sets declared) doesn't leave a blank stripe across the canvas.
  const columnOf = new Map(depths.map((d, index) => [d, index]));

  reduceCrossings(order, inner);
  const centres = assignRows(order, inner, distance);

  const nodes: Node[] = [];
  order.forEach((column, index) => {
    for (const id of column) {
      if (isLane(id)) continue;
      nodes.push({
        id,
        data: data(id),
        position: { x: index * columnWidth, y: centres.get(id)! - heightOf(id) / 2 },
        type: "default",
        // The graph runs left to right, so edges must leave the right side and
        // arrive at the left. With react-flow's default top/bottom handles
        // every edge doubles back on itself into an S — the "comes after"
        // direction is exactly the thing that stops being readable.
        sourcePosition: Position.Right,
        targetPosition: Position.Left,
      });
    }
  });

  // The parked row, clear of the lowest any layered node reached.
  if (parked.length > 0) {
    const lowest = Math.max(0, ...[...centres].map(([id, y]) => y + heightOf(id) / 2));
    parked.forEach((id, index) => {
      nodes.push({
        id,
        data: data(id),
        position: { x: index * columnWidth, y: lowest + rowGap * 2 },
        type: "default",
        sourcePosition: Position.Right,
        targetPosition: Position.Left,
      });
    });
  }

  // ── tracks: where each gap's verticals run ────────────────────────────────
  const gaps = new Map<number, Map<string, GapSegment>>();
  for (const { edge, chain, from } of chains.values()) {
    for (let i = 0; i + 1 < chain.length; i++) {
      const gap = from + i;
      const segments = gaps.get(gap) ?? new Map<string, GapSegment>();
      segments.set(`${chain[i]}\u0000${chain[i + 1]}`, {
        left: chain[i],
        right: chain[i + 1],
        ly: centres.get(chain[i])!,
        ry: centres.get(chain[i + 1])!,
        group: edge.group ?? "",
        // A link shared by several edges merges only if every one of them may.
        // A converging wire may, whatever its stroke: it's drawn from its
        // target end, so the shared stretch's dashes line up.
        merge:
          (segments.get(`${chain[i]}\u0000${chain[i + 1]}`)?.merge ?? true) &&
          (!!edge.merge || converges(edge)),
      });
      gaps.set(gap, segments);
    }
  }
  const trackX = new Map<string, number>();
  for (const [gap, segments] of gaps) {
    // A chain steps one depth at a time, and every depth it steps through
    // holds at least its lane, so the column after `gap` is always the next.
    const column = columnOf.get(gap)!;
    const left = column * columnWidth + nodeWidth + GAP_MARGIN_LEFT;
    const right = (column + 1) * columnWidth - GAP_MARGIN_RIGHT;
    for (const [key, x] of assignTracks([...segments.values()], left, right)) {
      trackX.set(`${gap}\u0000${key}`, x);
    }
  }

  const routes = new Map<string, EdgeRoute>();
  for (const [key, { edge, chain, from }] of chains) {
    routes.set(key, {
      fromTarget: converges(edge) && !edge.merge,
      lanes: chain.slice(1, -1).map((id) => centres.get(id)!),
      tracks: chain.slice(0, -1).map((id, i) => trackX.get(`${from + i}\u0000${id}\u0000${chain[i + 1]}`)!),
    });
  }

  // Backwards edges loop round, through the nearest height that is clear in
  // every column they pass over.
  for (const edge of edges) {
    const from = depth.get(edge.source);
    const to = depth.get(edge.target);
    if (from === undefined || to === undefined || to > from) continue;
    const spanned = order.filter((_, index) => depths[index] >= to && depths[index] <= from);
    routes.set(routeKey(edge.source, edge.target), {
      lanes: [],
      tracks: [],
      loop: clearHeight(
        spanned.flat(),
        centres,
        heightOf,
        (centres.get(edge.source)! + centres.get(edge.target)!) / 2,
      ),
    });
  }

  return { nodes, routes };
}

/**
 * The height nearest `wanted` that a horizontal wire can run along without
 * crossing any of `ids` — nodes, and the lanes other wires already run in.
 *
 * "Over the top of both ends" is the obvious answer for a loop and the wrong
 * one: it clears the two nodes it joins and then runs straight through
 * whatever sits between them a column along.
 */
function clearHeight(
  ids: string[],
  centres: Map<string, number>,
  heightOf: (id: string) => number,
  wanted: number,
): number {
  const busy = ids
    .map((id) => {
      const half = isLane(id) ? LANE_GAP / 2 : heightOf(id) / 2 + LANE_CLEARANCE / 2;
      return [centres.get(id)! - half, centres.get(id)! + half] as const;
    })
    .sort((a, b) => a[0] - b[0]);
  if (busy.length === 0) return wanted;

  // Above everything and below everything always work; between two busy
  // stretches works if there is room for the wire.
  const candidates = [busy[0][0] - LOOP_LIFT];
  let reach = busy[0][1];
  for (const [start, end] of busy.slice(1)) {
    if (start - reach >= LANE_GAP) candidates.push((start + reach) / 2);
    reach = Math.max(reach, end);
  }
  candidates.push(reach + LOOP_LIFT);
  return candidates.reduce((best, y) => (Math.abs(y - wanted) < Math.abs(best - wanted) ? y : best));
}

/** One wire's crossing of one gap, from its height on the left to the right. */
interface GapSegment {
  left: string;
  right: string;
  ly: number;
  ry: number;
  group: string;
  merge: boolean;
}

/** Wires sharing one vertical in a gap. */
interface Net {
  segments: GapSegment[];
  lo: number;
  hi: number;
  /** Heights the net's wires arrive at from the left… */
  lefts: number[];
  /** …and leave at to the right. */
  rights: number[];
}

/**
 * The x of every wire's vertical in one gap, keyed `left\0right`.
 *
 * Wires are first gathered into nets, which share a vertical. A net is either
 * one source fanning out — its wires leave from the same point, so a shared
 * trunk says nothing false — or several sources converging on one target,
 * where each has no other business in the gap. Mixing the two would not be
 * true: a bus joining A→C, A→D and B→C draws a B→D that doesn't exist.
 *
 * Nets that overlap vertically need different tracks, and their left-to-right
 * order decides how many wires cross. Each net is placed where it crosses
 * least against the ones already placed, then each is taken out and put back
 * where it now does best — a local search, but crossings here are counted
 * exactly, and the gaps hold a handful of nets rather than hundreds.
 */
function assignTracks(segments: GapSegment[], left: number, right: number): Map<string, number> {
  const key = (s: GapSegment) => `${s.left}\u0000${s.right}`;
  const centre = (left + right) / 2;
  const x = new Map<string, number>();

  const groups = new Map<string, GapSegment[]>();
  for (const segment of segments) {
    const out = `out\u0000${segment.group}\u0000${segment.left}`;
    groups.set(out, [...(groups.get(out) ?? []), segment]);
  }
  const converging = new Map<string, string[]>();
  for (const [name, members] of groups) {
    if (members.length !== 1 || !members[0].merge) continue;
    const into = `in\u0000${members[0].group}\u0000${members[0].right}`;
    converging.set(into, [...(converging.get(into) ?? []), name]);
  }
  for (const [into, names] of converging) {
    if (names.length < 2) continue;
    groups.set(
      into,
      names.flatMap((name) => groups.get(name)!),
    );
    for (const name of names) groups.delete(name);
  }

  const nets: Net[] = [];
  for (const members of groups.values()) {
    const lefts = [...new Set(members.map((s) => s.ly))];
    const rights = [...new Set(members.map((s) => s.ry))];
    const all = [...lefts, ...rights];
    const net = { segments: members, lo: Math.min(...all), hi: Math.max(...all), lefts, rights };
    // A net that never changes height has no vertical, so it needs no track.
    if (net.hi - net.lo < 0.5) {
      for (const segment of members) x.set(key(segment), centre);
    } else {
      nets.push(net);
    }
  }

  const order = orderNets(nets);

  // Compact: a net only has to sit right of the nets before it that it
  // overlaps, so two that never share a height can share a track — which keeps
  // a busy gap from spreading its verticals thinner than it needs to.
  const track: number[] = [];
  order.forEach((net, i) => {
    let at = 0;
    for (let j = 0; j < i; j++) {
      if (overlaps(order[j], net)) at = Math.max(at, track[j] + 1);
    }
    track.push(at);
  });
  const count = Math.max(0, ...track) + 1;
  const spacing = count > 1 ? Math.min(TRACK_SPACING, (right - left) / (count - 1)) : 0;
  order.forEach((net, i) => {
    const at = centre + (track[i] - (count - 1) / 2) * spacing;
    for (const segment of net.segments) x.set(key(segment), at);
  });
  return x;
}

/** Whether two nets' verticals share any height, with room for the stroke. */
function overlaps(a: Net, b: Net): boolean {
  return a.lo <= b.hi + 4 && b.lo <= a.hi + 4;
}

/**
 * How many times two nets' wires cross when `a`'s vertical is left of `b`'s.
 *
 * Each wire is a horizontal in from the left edge to its net's vertical, the
 * vertical, and a horizontal out to the right edge. With `a` on the left, `b`'s
 * incoming horizontals reach past `a`'s vertical, and `a`'s outgoing ones reach
 * past `b`'s; nothing else can meet. A horizontal that merely *touches* the
 * end of a vertical counts too, since it draws a junction that isn't one.
 */
function crossings(a: Net, b: Net): number {
  const within = (y: number, net: Net) => y >= net.lo - 0.5 && y <= net.hi + 0.5;
  return (
    b.lefts.filter((y) => within(y, a)).length + a.rights.filter((y) => within(y, b)).length
  );
}

/** Nets in left-to-right order, chosen to cross as little as possible. */
function orderNets(nets: Net[]): Net[] {
  const cost = (order: Net[], net: Net, at: number) => {
    let total = 0;
    order.forEach((other, i) => {
      total += i < at ? crossings(other, net) : crossings(net, other);
    });
    return total;
  };
  const insert = (order: Net[], net: Net) => {
    let best = order.length;
    let fewest = Infinity;
    // Last position first, so a tie keeps the net where the sort put it.
    for (let at = order.length; at >= 0; at--) {
      const c = cost(order, net, at);
      if (c < fewest) {
        fewest = c;
        best = at;
      }
    }
    order.splice(best, 0, net);
  };

  // The widest first: they constrain the most, so they should choose first.
  const order: Net[] = [];
  for (const net of [...nets].sort((a, b) => b.hi - b.lo - (a.hi - a.lo))) insert(order, net);
  for (let pass = 0; pass < 3; pass++) {
    for (const net of [...order]) {
      order.splice(order.indexOf(net), 1);
      insert(order, net);
    }
  }
  return order;
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
 * The centre height of every node and lane, once the columns are ordered.
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
 * rather than once at the end stops the two from fighting. It is also what
 * makes trunks straight: a lane has one predecessor, so it is pulled level with
 * it, and a wire with nothing in its way never has to change height.
 */
function assignRows(
  order: string[][],
  edges: { source: string; target: string }[],
  distance: (above: string, below: string) => number,
): Map<string, number> {
  const rows = new Map<string, number>();
  for (const column of order) {
    let y = 0;
    column.forEach((id, index) => {
      if (index > 0) y += distance(column[index - 1], id);
      rows.set(id, y);
    });
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
    for (const column of order) separate(column, rows, distance);
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
 * Push a column's entries apart until none overlaps, keeping the column centred
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
  distance: (above: string, below: string) => number,
): void {
  if (column.length < 2) return;

  const wanted = column.map((id) => rows.get(id)!);
  const placed = [...wanted];
  for (let index = 1; index < placed.length; index++) {
    placed[index] = Math.max(
      placed[index],
      placed[index - 1] + distance(column[index - 1], column[index]),
    );
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
