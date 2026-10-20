/**
 * A page made of boxes you arrange yourself: drag one by its title bar to
 * move it, drag its corner to make it bigger or smaller. The arrangement is
 * remembered per page, so it renders the way you left it every time.
 *
 * This replaced fixed panes with a "widen" toggle. A toggle offers the two
 * layouts somebody thought of; a box offers whichever one this graph and these
 * logs actually want — a wide flowchart over a short log, a tall log beside a
 * narrow graph, the environment pulled up top.
 *
 * Built on react-grid-layout. Each box is a grid item; the page keeps the
 * layouts, keyed by breakpoint, in `localStorage`. Storage can be unavailable
 * (a private window, blocked site data), and then the layout simply isn't
 * remembered.
 */

import {
  useCallback,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
  type ReactNode,
} from "react";
import { Box, Stack, Typography } from "@mui/material";
import DragIndicatorIcon from "@mui/icons-material/DragIndicator";
import { Responsive, WidthProvider, type Layout, type Layouts } from "react-grid-layout";
import "react-grid-layout/css/styles.css";
import "react-resizable/css/styles.css";

const Grid = WidthProvider(Responsive);

/** Breakpoints by container width, and the columns each has. Below `md`
 *  there's only room for one column, so every box is full width there. */
const BREAKPOINTS = { lg: 1200, md: 900, sm: 0 };
const COLS = { lg: 12, md: 12, sm: 1 };

/** One grid row, in px. Small, so a box's height can be set finely. */
export const ROW_HEIGHT = 24;
/** The gap between boxes, in px. */
const MARGIN = 12;

/** Grid rows that fit `px` of box height, for writing defaults in pixels. */
export function rowsFor(px: number): number {
  return Math.max(1, Math.round((px + MARGIN) / (ROW_HEIGHT + MARGIN)));
}

/** A box's height around its content: title bar and its rule, the body's
 *  padding, and the box's own top and bottom border. */
const BOX_CHROME = 36 + 1 + 16 + 2;

/** Where each box goes before anybody has moved it, per breakpoint. */
export type DefaultLayouts = Record<keyof typeof BREAKPOINTS, Layout[]>;

/**
 * The saved arrangement for `storageKey`, falling back to `defaults`.
 *
 * Saved layouts are merged with the defaults rather than taken whole: a box
 * added to the page after the layout was saved has no saved place, and
 * without its default it would land as a one-cell sliver at the bottom. A
 * saved box the page no longer has is dropped.
 */
export function useBoxLayout(storageKey: string, defaults: DefaultLayouts) {
  const [saved, setSaved] = useState<Layouts | null>(() => {
    try {
      const text = localStorage.getItem(storageKey);
      return text ? (JSON.parse(text) as Layouts) : null;
    } catch {
      return null;
    }
  });

  // The heights of boxes that size themselves to their content, in rows.
  // Measured afresh on every visit rather than saved: they follow what the
  // box is showing, not where somebody left it.
  const [fitted, setFitted] = useState<Record<string, number>>({});

  const layouts = useMemo(() => {
    const merged: Layouts = {};
    for (const [breakpoint, items] of Object.entries(defaults)) {
      const mine = saved?.[breakpoint] ?? [];
      merged[breakpoint] = items.map((item) => {
        const placed = mine.find((m) => m.i === item.i);
        // Keep the box's limits from the defaults: they're the page's to set.
        const box = placed
          ? { ...item, x: placed.x, y: placed.y, w: placed.w, h: placed.h }
          : item;
        // A fitted box's height is its content's, so only its width is
        // anybody's to drag.
        return fitted[item.i] !== undefined
          ? { ...box, h: fitted[item.i], minH: 1, resizeHandles: ["e" as const, "w" as const] }
          : box;
      });
    }
    return merged;
  }, [saved, defaults, fitted]);

  const fit = useCallback((id: string, rows: number) => {
    setFitted((current) => (current[id] === rows ? current : { ...current, [id]: rows }));
  }, []);

  const onChange = useCallback(
    (_: Layout[], all: Layouts) => {
      setSaved(all);
      try {
        localStorage.setItem(storageKey, JSON.stringify(all));
      } catch {
        // Not remembered; still applied for this page.
      }
    },
    [storageKey],
  );

  const reset = useCallback(() => {
    setSaved(null);
    try {
      localStorage.removeItem(storageKey);
    } catch {
      // Nothing saved to remove.
    }
  }, [storageKey]);

  return { layouts, onChange, reset, fit, customised: saved !== null };
}

/** One box: a title bar to drag it by, and its content. */
export interface BoxItem {
  id: string;
  title: ReactNode;
  /** Controls on the right of the title bar. Clicking them never drags. */
  actions?: ReactNode;
  /**
   * How the content uses the box. `fill` hands it the box's exact size, for
   * content that sizes itself to its container — a graph canvas, a log view.
   * `scroll` lets content of its own height scroll inside the box.
   */
  body?: "fill" | "scroll";
  /**
   * Size the box to its content: it grows when there's more to show and
   * shrinks back when there's less, and only its width can be dragged.
   */
  fit?: boolean;
  children: ReactNode;
}

/** Report `children`'s height whenever it changes. */
function Measured({ onHeight, children }: { onHeight: (px: number) => void; children: ReactNode }) {
  const ref = useRef<HTMLDivElement>(null);
  // Through a ref, so a new callback each render doesn't re-subscribe.
  const report = useRef(onHeight);
  report.current = onHeight;
  useLayoutEffect(() => {
    const element = ref.current;
    if (!element) return;
    const observer = new ResizeObserver(() => report.current(element.offsetHeight));
    observer.observe(element);
    report.current(element.offsetHeight);
    return () => observer.disconnect();
  }, []);
  return <div ref={ref}>{children}</div>;
}

export function BoxLayout({
  boxes,
  layouts,
  onChange,
  onFit,
}: {
  boxes: BoxItem[];
  layouts: Layouts;
  onChange: (current: Layout[], all: Layouts) => void;
  /** Told the rows a `fit` box needs; `useBoxLayout().fit`. */
  onFit?: (id: string, rows: number) => void;
}) {
  return (
    <Grid
      layouts={layouts}
      breakpoints={BREAKPOINTS}
      cols={COLS}
      rowHeight={ROW_HEIGHT}
      margin={[MARGIN, MARGIN]}
      containerPadding={[0, 0]}
      draggableHandle=".box-drag"
      // Buttons, switches and fields in a title bar are for clicking, not
      // for picking the box up.
      draggableCancel=".box-actions"
      // Every side but the top, which is the handle for moving it. A box on
      // the right half of the page has to be able to grow to the left.
      resizeHandles={["e", "w", "s", "se", "sw"]}
      onLayoutChange={onChange}
      // Boxes pack upwards: shrinking or moving one never leaves a hole
      // above the boxes below it.
      compactType="vertical"
    >
      {boxes.map((box) => (
        <Box
          key={box.id}
          sx={{
            display: "flex",
            flexDirection: "column",
            border: 1,
            borderColor: "divider",
            borderRadius: 1,
            bgcolor: "background.paper",
            overflow: "hidden",
            // The library draws a triangular grip in each corner, which pops
            // up under the pointer and reads as clutter. The corners still
            // resize — the cursor says so — but draw nothing; edges become
            // strips the full length of the side, which light up under the
            // pointer — easier to find than a 20px square. The selectors name
            // the grid item and both handle classes to outrank the library's
            // own (`.react-grid-item > .react-resizable-handle.…-w`), which
            // turns each side handle 45° — a long diagonal bar otherwise.
            "&.react-grid-item > .react-resizable-handle": { zIndex: 3, backgroundImage: "none" },
            "&.react-grid-item > .react-resizable-handle::after": { display: "none" },
            "&.react-grid-item > .react-resizable-handle.react-resizable-handle-e, &.react-grid-item > .react-resizable-handle.react-resizable-handle-w": {
              top: 36,
              bottom: 16,
              height: "auto",
              width: 8,
              marginTop: 0,
              transform: "none",
              "&::after": { display: "none" },
              "&:hover": { bgcolor: "action.selected" },
            },
            "&.react-grid-item > .react-resizable-handle.react-resizable-handle-s": {
              left: 16,
              right: 16,
              width: "auto",
              height: 8,
              marginLeft: 0,
              transform: "none",
              "&::after": { display: "none" },
              "&:hover": { bgcolor: "action.selected" },
            },
          }}
        >
          <Stack
            direction="row"
            alignItems="center"
            spacing={1}
            className="box-drag"
            sx={{
              height: 36,
              flexShrink: 0,
              pl: 0.5,
              pr: 1,
              borderBottom: 1,
              borderColor: "divider",
              cursor: "grab",
              "&:active": { cursor: "grabbing" },
              userSelect: "none",
            }}
          >
            <DragIndicatorIcon fontSize="small" sx={{ color: "text.disabled" }} />
            {/* Never squeezed to nothing by a wide search field beside it. */}
            <Typography variant="h4" component="h2" noWrap sx={{ flexGrow: 1, minWidth: 64 }}>
              {box.title}
            </Typography>
            {box.actions && (
              <Stack
                direction="row"
                alignItems="center"
                spacing={1}
                className="box-actions"
                sx={{ cursor: "default", minWidth: 0 }}
              >
                {box.actions}
              </Stack>
            )}
          </Stack>
          <Box
            sx={{
              flex: 1,
              minHeight: 0,
              position: "relative",
              ...(box.body === "fill" ? {} : { overflow: "auto", px: 1.5, py: 1 }),
            }}
          >
            {box.body === "fill" ? (
              // Absolutely sized, so content asking for 100% gets the box's
              // height rather than its own. The box is the frame: a graph or
              // log view that draws its own border and rounded corners would
              // sit inside it as a second, inset frame.
              <Box
                sx={{
                  position: "absolute",
                  inset: 0,
                  display: "flex",
                  flexDirection: "column",
                  // Doubled, to outrank the child's own single-class styles.
                  "&& > *": { border: 0, borderRadius: 0 },
                }}
              >
                {box.children}
              </Box>
            ) : box.fit && onFit ? (
              <Measured
                onHeight={(px) =>
                  onFit(
                    box.id,
                    // Rounded up: a row short and the last line is cut off.
                    Math.max(1, Math.ceil((px + BOX_CHROME + MARGIN) / (ROW_HEIGHT + MARGIN))),
                  )
                }
              >
                {box.children}
              </Measured>
            ) : (
              box.children
            )}
          </Box>
        </Box>
      ))}
    </Grid>
  );
}
