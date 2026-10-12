/**
 * What a step looks like on a run graph — one definition, so the daemon's run
 * page and the remote cache's build view draw the same node.
 *
 * Every line is a fixed height. The layout has to know each node's height
 * before react-flow has drawn it: handles hang off the middle of a node, and a
 * wire routed to where the layout *thought* the middle was arrives a few pixels
 * off it and kinks. And fixed per node rather than measured, so a node doesn't
 * grow when its step finishes and gains a time — which shoved the rest of its
 * column down mid-run.
 */

import type { CSSProperties, ReactNode } from "react";
import { Box, Stack } from "@mui/material";
import type { Theme } from "@mui/material/styles";

import { StatusIcon } from "./StatusIcon";

export const monoFontStack = [
  "ui-monospace",
  "SFMono-Regular",
  "Menlo",
  "Consolas",
  "'Liberation Mono'",
  "monospace",
].join(", ");

/**
 * How wide a step node is drawn.
 *
 * Fixed rather than fitted, because the layout has to know: it places columns a
 * set distance apart, and a node that grows past that distance reaches into the
 * lane the next column's edges arrive through — which is how a line ends up
 * drawn across the middle of a node.
 */
export const STEP_NODE_WIDTH = 210;

/** The lines a step node is built from, each a fixed height. */
export const STEP_LINE = { name: 16, small: 14 } as const;

/** A step node's border and padding, top and bottom together. */
export const STEP_CHROME = 2 * 2 + 6 * 2;

/** How the layered layout spaces a run graph's columns and rows. */
export const STEP_LAYOUT = {
  // A column has to be wider than the widest node it can hold, or a long step
  // name grows its node into the next column and the edges arriving there run
  // over it. This leaves a gap the wires can turn in.
  columnWidth: STEP_NODE_WIDTH + 130,
  nodeWidth: STEP_NODE_WIDTH,
  rowGap: 26,
} as const;

/**
 * How tall a step node is: its name and detail line always, a workspace line
 * when it has one, and however many extra one-line rows the caller adds.
 */
export function stepNodeHeight(hasWorkspace: boolean, extraLines = 0): number {
  return (
    STEP_CHROME +
    STEP_LINE.name +
    // The detail line is always there, so a step's node is the same height
    // before it runs as after.
    STEP_LINE.small +
    (hasWorkspace ? STEP_LINE.small : 0) +
    extraLines * STEP_LINE.small
  );
}

/** One line of text at exactly `height`, ending in an ellipsis if it's long. */
export function oneLine(height: number) {
  return {
    height,
    lineHeight: `${height}px`,
    overflow: "hidden",
    whiteSpace: "nowrap",
    textOverflow: "ellipsis",
  } as const;
}

/** A small line under a step's name: its timing, a note, a cache verdict. */
export function StepLine({ children, mono = true }: { children: ReactNode; mono?: boolean }) {
  return (
    <Box
      sx={{
        ...oneLine(STEP_LINE.small),
        fontSize: 10,
        opacity: 0.7,
        fontFamily: mono ? monoFontStack : undefined,
      }}
    >
      {children}
    </Box>
  );
}

/**
 * The inside of a step node: status glyph, workspace, name, a detail line, and
 * whatever extra lines and trailing adornment the page wants to add.
 */
export function StepNodeBody({
  name,
  workspace,
  status,
  detail,
  order = null,
  extra,
  adornment,
}: {
  name: string;
  workspace?: string | null;
  status: string;
  /** The line under the name — timing, or what happened instead. */
  detail: ReactNode;
  /** A position in the run's sequence, drawn as a badge. */
  order?: number | null;
  /** Extra one-line rows; count them into `stepNodeHeight`. */
  extra?: ReactNode;
  /** Something at the right edge, such as a "from cache" icon. */
  adornment?: ReactNode;
}) {
  // The id is "<workspace>:<step>" (or "<workspace>:<workflow>:<step>"), and
  // repeating the workspace in both lines just wastes the node's width.
  const short =
    workspace && name.startsWith(`${workspace}:`) ? name.slice(workspace.length + 1) : name;

  return (
    <Stack direction="row" spacing={0.75} alignItems="center" sx={{ minWidth: 0, width: "100%" }}>
      {order !== null && (
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
      )}
      {/* No tooltip: the node has its own hover, and two would fight. */}
      <StatusIcon status={status} title={null} />
      {/* Every line is one line high, cut off rather than wrapped: the node's
          height is fixed (see `stepNodeHeight`), and the full name is in the
          title. */}
      <Box sx={{ minWidth: 0, flexGrow: 1, textAlign: "left" }} title={name}>
        {workspace && (
          <Box
            sx={{
              ...oneLine(STEP_LINE.small),
              fontSize: 10,
              opacity: 0.75,
              fontFamily: monoFontStack,
            }}
          >
            {workspace}
          </Box>
        )}
        <Box sx={{ ...oneLine(STEP_LINE.name), fontWeight: 600 }}>{short}</Box>
        <StepLine>{detail}</StepLine>
        {extra}
      </Box>
      {adornment}
    </Stack>
  );
}

/** The box a step node is drawn in: its react-flow `style`. */
export function stepNodeStyle({
  theme,
  height,
  border,
  dashed = false,
  opacity = 1,
  picked = false,
}: {
  theme: Theme;
  height: number;
  /** The border's colour — the step's status, or whatever the view is about. */
  border: string;
  /** Recovery nodes are dashed: they're branches you hope never run. */
  dashed?: boolean;
  opacity?: number;
  /** Ring the node that was clicked. */
  picked?: boolean;
}): CSSProperties {
  return {
    background: theme.palette.background.paper,
    color: theme.palette.text.primary,
    borderRadius: 8,
    // Bounded, so a node can't grow across the gap the wires route through. A
    // long name is cut off inside the box instead of widening it.
    width: STEP_NODE_WIDTH,
    height,
    // Border-box, so a border thickening on focus eats into the node rather
    // than growing it off the height the wires were routed to.
    boxSizing: "border-box",
    display: "flex",
    alignItems: "center",
    overflow: "hidden",
    textAlign: "left",
    opacity,
    boxShadow: picked ? `0 0 0 3px ${theme.palette.secondary.main}55` : undefined,
    border: `2px ${dashed ? "dashed" : "solid"} ${picked ? theme.palette.secondary.main : border}`,
    fontSize: 12,
    padding: "6px 12px",
  };
}
