/**
 * A finished run's steps as a flowchart, ready to drop into a page.
 *
 * The daemon's run page builds its graph by hand, because it layers a great
 * deal on top (live status, environment and file nodes, focus, cache inspect
 * mode). Anything that only needs to *show* a run — the remote cache's build
 * and job views — uses this, and gets the same canvas, layout, wires and nodes.
 */

import { useMemo, useRef, useState, type SyntheticEvent } from "react";
import ArrowDropDownIcon from "@mui/icons-material/ArrowDropDown";
import LayersIcon from "@mui/icons-material/Layers";
import {
  Box,
  ListSubheader,
  Menu,
  MenuItem,
  Stack,
  Typography,
  useTheme,
} from "@mui/material";
import type { Edge, Node, NodeMouseHandler } from "@xyflow/react";

import { GraphCanvas } from "./GraphCanvas";
import { ROUTED_EDGE, layeredLayout, routeKey, type Placement } from "./layout";
import { StatusIcon, statusColour } from "./StatusIcon";
import {
  STEP_LAYOUT,
  StepLine,
  StepNodeBody,
  monoFontStack,
  stepNodeHeight,
  stepNodeStyle,
} from "./StepNode";

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
  /**
   * The workflow file it belongs to. With `fold` on, the steps of each file
   * are drawn as one stacked node.
   */
  group?: string | null;
}

/** Height of the strip across the top of a stacked node that opens its steps. */
const STACK_HEADER = 18;

/** The node id a folded workflow file takes. */
const stackId = (group: string) => `stack::${group}`;

/**
 * The worst thing happening in a stack, which is what its border says: a
 * failure underneath must not hide behind a step that passed on top.
 */
function worst(steps: GraphStep[], top: GraphStep): string {
  for (const status of [
    "failed",
    "running",
    "processing",
    "restoring",
    "stopped",
  ]) {
    if (steps.some((s) => s.status === status)) return status;
  }
  return top.status;
}

interface Folded {
  /** Each stack's steps, in order, by group. */
  members: Map<string, GraphStep[]>;
  /** The group a folded step is in. */
  of: Map<string, string>;
  /** The step drawn on top of each stack. */
  top: Map<string, GraphStep>;
}

/**
 * Which workflow files to fold, and the step on top of each — the same rule as
 * the daemon's run page. Recovery and background steps stay nodes of their
 * own: one is a branch a failure jumps to, the other gates nothing, and
 * folding either would hide the wire that says so. Nothing folds when the
 * graph is one workflow file: the stack would be the whole graph.
 *
 * On top: the highlighted step, else the one last picked from the stack's
 * list, else the one worth looking at — running, failed, not yet run — else
 * the last.
 */
function fold(steps: GraphStep[], picked: Record<string, string>): Folded {
  const files = new Map<string, GraphStep[]>();
  for (const s of steps) {
    if (s.recover || s.background || !s.group) continue;
    files.set(s.group, [...(files.get(s.group) ?? []), s]);
  }
  const folded: Folded = { members: new Map(), of: new Map(), top: new Map() };
  if (files.size < 2) return folded;
  for (const [group, members] of files) {
    if (members.length < 2) continue;
    folded.members.set(group, members);
    for (const s of members) folded.of.set(s.name, group);
    const top =
      members.find((s) => s.highlight) ??
      members.find((s) => s.name === picked[group]) ??
      members.find((s) =>
        ["running", "processing", "restoring"].includes(s.status),
      ) ??
      members.find((s) => s.status === "failed") ??
      members.find((s) => s.status === "pending") ??
      members[members.length - 1];
    folded.top.set(group, top);
  }
  return folded;
}

export function StepGraph({
  steps,
  height = 460,
  onStepClick,
  minimap,
  holdLayout,
  focus,
  fold: folding = false,
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
  /** A step to keep in sight — see `GraphCanvas`'s `focus`. */
  focus?: string | null;
  /**
   * Draw each workflow file (`group`) with several steps as one stacked node,
   * the step on top chosen from a list across its top. Clicking the stack
   * picks the step on top.
   */
  fold?: boolean;
}) {
  const theme = useTheme();
  const placed = useRef<Map<string, Placement> | undefined>(undefined);
  // The step last chosen from each stack's own list.
  const [picked, setPicked] = useState<Record<string, string>>({});
  // Read through a ref: a page passing a new function each render must not
  // re-lay-out the graph.
  const click = useRef(onStepClick);
  click.current = onStepClick;

  const folded = useMemo(
    () =>
      folding
        ? fold(steps, picked)
        : ({ members: new Map(), of: new Map(), top: new Map() } as Folded),
    [folding, steps, picked],
  );
  // The node a step is drawn as: its stack's, when it's folded.
  const nodeOf = (name: string) => {
    const group = folded.of.get(name);
    return group === undefined ? name : stackId(group);
  };

  const { nodes, edges } = useMemo(() => {
    const byName = new Map(steps.map((s) => [s.name, s]));
    const ids = [...new Set(steps.map((s) => nodeOf(s.name)))];
    // What each node draws: a step, or the step on top of a stack.
    const shown = (id: string): GraphStep => {
      const group = id.startsWith("stack::")
        ? id.slice("stack::".length)
        : null;
      return group !== null ? folded.top.get(group)! : byName.get(id)!;
    };
    const membersOf = (id: string): GraphStep[] | null =>
      id.startsWith("stack::")
        ? (folded.members.get(id.slice("stack::".length)) ?? null)
        : null;

    const heightOf = (id: string) => {
      const s = shown(id);
      return (
        stepNodeHeight(Boolean(s.workspace), s.note ? 1 : 0) +
        (membersOf(id) ? STACK_HEADER : 0)
      );
    };
    const seen = new Set<string>();
    const links = steps.flatMap((s) =>
      (s.needs ?? [])
        .filter((n) => byName.has(n))
        .map((n) => ({ source: nodeOf(n), target: nodeOf(s.name) }))
        .filter((l) => {
          const key = `${l.source}->${l.target}`;
          if (l.source === l.target || seen.has(key)) return false;
          seen.add(key);
          return true;
        }),
    );

    const {
      nodes: positioned,
      routes,
      placement,
    } = layeredLayout(
      ids,
      links.map((e) => ({ ...e, group: "needs", merge: true })),
      (id) => {
        const s = shown(id);
        const members = membersOf(id);
        const body = (
          <StepNodeBody
            name={s.label ?? s.name}
            workspace={s.workspace}
            status={s.status}
            detail={s.detail ?? s.status}
            extra={
              s.note ? <StepLine mono={false}>{s.note}</StepLine> : undefined
            }
          />
        );
        return {
          status: members ? worst(members, s) : s.status,
          label: members ? (
            <StackLabel
              steps={members}
              top={s}
              onPick={(name) => {
                setPicked((old) => ({ ...old, [folded.of.get(name)!]: name }));
                click.current?.(name);
              }}
            >
              {body}
            </StackLabel>
          ) : (
            body
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
      const s = shown(node.id);
      const members = membersOf(node.id);
      const status = members ? worst(members, s) : s.status;
      const highlight = members
        ? members.some((m) => m.highlight)
        : s.highlight;
      return {
        ...node,
        style: stepNodeStyle({
          theme,
          height: heightOf(node.id),
          border: highlight
            ? theme.palette.primary.main
            : s.reused && status === s.status
              ? theme.palette.success.main
              : statusColour(status, theme),
          dashed: s.recover,
          opacity: status === "skipped" || status === "pending" ? 0.7 : 1,
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
    // `nodeOf` reads `folded`, which is a dependency.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [steps, theme, holdLayout, folded]);

  // A stack stands for the step on top of it.
  const onNodeClick: NodeMouseHandler | undefined = onStepClick
    ? (_, node) =>
        onStepClick(
          node.id.startsWith("stack::")
            ? (folded.top.get(node.id.slice("stack::".length))?.name ?? node.id)
            : node.id,
        )
    : undefined;

  return (
    <GraphCanvas
      nodes={nodes}
      edges={edges}
      height={height}
      onNodeClick={onNodeClick}
      minimap={minimap}
      keepView={holdLayout}
      focus={focus ? nodeOf(focus) : null}
      // Refit when the shape changes, not just the set of nodes: an edited
      // graph that gains an edge can push a node off the canvas. Folding or
      // unfolding changes it too.
      fitKey={
        (folding ? "folded\u0000" : "") +
        steps
          .map((s) => `${s.name}<${(s.needs ?? []).join(",")}`)
          .join("\u0000")
      }
      nodeColor={(node) => {
        const group = node.id.startsWith("stack::")
          ? node.id.slice("stack::".length)
          : null;
        const s =
          group !== null
            ? folded.top.get(group)
            : steps.find((x) => x.name === node.id);
        if (!s) return theme.palette.divider;
        const status =
          group !== null ? worst(folded.members.get(group)!, s) : s.status;
        return s.reused && status === s.status
          ? theme.palette.success.main
          : statusColour(status, theme);
      }}
    />
  );
}

/**
 * A folded workflow file: a strip across the top saying which of its steps is
 * showing, which opens the list of all of them, over the step on top drawn the
 * way any step is.
 *
 * Clicks in the strip and its menu stop there. The menu is portalled out of the
 * node, but React still bubbles its events through it, and a click reaching
 * react-flow would also select the stack.
 */
function StackLabel({
  steps,
  top,
  onPick,
  children,
}: {
  steps: GraphStep[];
  top: GraphStep;
  onPick: (name: string) => void;
  children: React.ReactNode;
}) {
  const [anchor, setAnchor] = useState<HTMLElement | null>(null);
  const stop = (event: SyntheticEvent) => event.stopPropagation();
  const index = steps.findIndex((s) => s.name === top.name);
  const group = top.group ?? "";
  return (
    <Box
      sx={{
        display: "flex",
        flexDirection: "column",
        minWidth: 0,
        flexGrow: 1,
      }}
    >
      <Box
        component="button"
        type="button"
        className="nodrag nopan"
        title={`Choose the step of ${group} shown on top`}
        onClick={(event: React.MouseEvent<HTMLElement>) => {
          stop(event);
          setAnchor(event.currentTarget);
        }}
        sx={{
          all: "unset",
          boxSizing: "border-box",
          height: STACK_HEADER,
          display: "flex",
          alignItems: "center",
          gap: 0.5,
          mx: -1.5,
          px: 1.5,
          fontSize: 10,
          color: "text.secondary",
          borderBottom: 1,
          borderColor: "divider",
          cursor: "pointer",
          flexShrink: 0,
          "&:hover": { color: "text.primary", bgcolor: "action.hover" },
        }}
      >
        <LayersIcon sx={{ fontSize: 12 }} />
        <Box component="span" sx={{ fontWeight: 600 }}>
          step {index + 1} of {steps.length}
        </Box>
        <ArrowDropDownIcon sx={{ fontSize: 16, ml: "auto" }} />
      </Box>
      <Box
        sx={{ display: "flex", alignItems: "center", flexGrow: 1, minWidth: 0 }}
      >
        {children}
      </Box>
      <Menu
        anchorEl={anchor}
        open={anchor !== null}
        onClose={() => setAnchor(null)}
        onClick={stop}
        onMouseDown={stop}
      >
        <ListSubheader sx={{ lineHeight: "32px" }}>
          {group} · {steps.length} steps, in order
        </ListSubheader>
        {steps.map((step, i) => (
          <MenuItem
            key={step.name}
            selected={step.name === top.name}
            dense
            onClick={() => {
              setAnchor(null);
              onPick(step.name);
            }}
          >
            <Stack
              direction="row"
              spacing={1}
              alignItems="center"
              sx={{ minWidth: 0 }}
            >
              <Typography
                variant="caption"
                color="text.secondary"
                sx={{ width: 16 }}
              >
                {i + 1}
              </Typography>
              <StatusIcon status={step.reused ? "success" : step.status} />
              <Typography variant="body2" sx={{ fontFamily: monoFontStack }}>
                {step.label ?? step.name}
              </Typography>
              {step.detail && (
                <Typography variant="caption" color="text.secondary" noWrap>
                  {step.detail}
                </Typography>
              )}
            </Stack>
          </MenuItem>
        ))}
      </Menu>
    </Box>
  );
}
