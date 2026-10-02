/**
 * An edge drawn along the route `layeredLayout` chose for it, as one path.
 *
 * It used to be drawn as a chain of react-flow edges strung between invisible
 * waypoint nodes, one per column the edge skipped. Every link in that chain was
 * its own path with its own idea of where to turn, and every waypoint was a
 * node react-flow had to measure before it would draw what touched it. The
 * joins showed — a kink where two links met a pixel apart, a gap while a
 * waypoint was unmeasured, a dash pattern restarting at every column — and a
 * wire that is visibly in pieces reads as several wires.
 *
 * One path per edge has none of those seams: the layout says where the lanes
 * and the verticals are, react-flow says where the two ends are, and this joins
 * them with right angles rounded off.
 */

import { BaseEdge, type EdgeProps } from "@xyflow/react";

import type { EdgeRoute } from "./layout";

interface Point {
  x: number;
  y: number;
}

/** Corner radius where a wire turns. Smaller than the track spacing would let
 *  neighbouring corners touch, so they don't. */
const RADIUS = 8;

/** How far a wire runs straight out of (or into) a node before turning, when
 *  it has no route to follow. */
const REACH = 18;

export function RoutedEdge({
  sourceX,
  sourceY,
  targetX,
  targetY,
  data,
  markerEnd,
  markerStart,
  style,
  label,
  labelStyle,
  labelShowBg,
  labelBgStyle,
  labelBgPadding,
  labelBgBorderRadius,
  interactionWidth,
}: EdgeProps) {
  const route = (data as { route?: EdgeRoute } | undefined)?.route;
  const points = routePoints({ x: sourceX, y: sourceY }, { x: targetX, y: targetY }, route);
  const at = labelPoint(points);
  // Drawn from the target end when the route says so (see
  // `EdgeRoute.fromTarget`). The arrowhead moves to the path's start, where
  // react-flow's `auto-start-reverse` markers turn it to point the same way;
  // an animated dash runs in reverse so it still flows towards the target.
  const reversed = route?.fromTarget === true;

  return (
    <BaseEdge
      path={roundedPath(reversed ? [...points].reverse() : points, RADIUS)}
      markerEnd={reversed ? markerStart : markerEnd}
      markerStart={reversed ? markerEnd : markerStart}
      style={reversed ? { ...style, animationDirection: "reverse" } : style}
      label={label}
      labelX={at.x}
      labelY={at.y}
      labelStyle={labelStyle}
      labelShowBg={labelShowBg}
      labelBgStyle={labelBgStyle}
      labelBgPadding={labelBgPadding}
      labelBgBorderRadius={labelBgBorderRadius}
      interactionWidth={interactionWidth}
    />
  );
}

/** The corners an edge turns at, from its source handle to its target's. */
export function routePoints(source: Point, target: Point, route?: EdgeRoute): Point[] {
  if (route?.loop !== undefined) return loop(source, target, route.loop);

  if (!route || route.tracks.length === 0) {
    // Nothing routed it — an edge to a node parked outside the layering. A
    // single dog-leg if the target is ahead, a loop over the top if not.
    if (target.x - source.x < REACH * 2) {
      return loop(source, target, Math.min(source.y, target.y) - 40);
    }
    const x = target.x - REACH;
    return [source, { x, y: source.y }, { x, y: target.y }, target];
  }

  const points: Point[] = [source];
  let y = source.y;
  route.tracks.forEach((x, gap) => {
    // The height this gap hands on to: the next lane, or the target itself.
    const next = gap < route.lanes.length ? route.lanes[gap] : target.y;
    if (Math.abs(next - y) > 0.5) {
      points.push({ x, y }, { x, y: next });
      y = next;
    }
  });
  points.push(target);
  return points;
}

/** Out to the right, up and over, back down into the target from its left. */
function loop(source: Point, target: Point, over: number): Point[] {
  const out = source.x + REACH;
  const back = target.x - REACH;
  return [
    source,
    { x: out, y: source.y },
    { x: out, y: over },
    { x: back, y: over },
    { x: back, y: target.y },
    target,
  ];
}

/**
 * An SVG path through `points`, its corners rounded.
 *
 * A corner's radius shrinks to fit the shorter of its two legs, so a small jog
 * between nearly level lanes stays a jog rather than overshooting into an S.
 */
export function roundedPath(points: Point[], radius: number): string {
  const pts = simplify(points);
  if (pts.length === 0) return "";
  let d = `M ${pts[0].x} ${pts[0].y}`;
  for (let i = 1; i < pts.length - 1; i++) {
    const [prev, corner, next] = [pts[i - 1], pts[i], pts[i + 1]];
    const r = Math.min(radius, length(prev, corner) / 2, length(corner, next) / 2);
    const a = towards(corner, prev, r);
    const b = towards(corner, next, r);
    d += ` L ${a.x} ${a.y} Q ${corner.x} ${corner.y} ${b.x} ${b.y}`;
  }
  const last = pts[pts.length - 1];
  return `${d} L ${last.x} ${last.y}`;
}

/** Drop repeated points and the middle of any straight run, so every point
 *  left is a real corner. A point where the path doubles back is kept. */
function simplify(points: Point[]): Point[] {
  const out: Point[] = [];
  for (const point of points) {
    if (out.length > 0 && length(out[out.length - 1], point) < 0.01) continue;
    if (out.length >= 2) {
      const [a, b] = [out[out.length - 2], out[out.length - 1]];
      const cross = (b.x - a.x) * (point.y - b.y) - (b.y - a.y) * (point.x - b.x);
      const dot = (b.x - a.x) * (point.x - b.x) + (b.y - a.y) * (point.y - b.y);
      if (Math.abs(cross) < 0.01 && dot > 0) out.pop();
    }
    out.push(point);
  }
  return out;
}

/** Where a label goes: halfway along the first leg long enough to hold one. */
function labelPoint(points: Point[]): Point {
  const pts = simplify(points);
  for (let i = 0; i + 1 < pts.length; i++) {
    if (length(pts[i], pts[i + 1]) >= 40) return midpoint(pts[i], pts[i + 1]);
  }
  return midpoint(pts[0], pts[pts.length - 1]);
}

function length(a: Point, b: Point): number {
  return Math.hypot(b.x - a.x, b.y - a.y);
}

function midpoint(a: Point, b: Point): Point {
  return { x: (a.x + b.x) / 2, y: (a.y + b.y) / 2 };
}

/** The point `distance` along the line from `from` towards `to`. */
function towards(from: Point, to: Point, distance: number): Point {
  const total = length(from, to);
  if (total === 0) return from;
  return {
    x: from.x + ((to.x - from.x) / total) * distance,
    y: from.y + ((to.y - from.y) / total) * distance,
  };
}
