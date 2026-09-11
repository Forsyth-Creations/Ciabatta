/**
 * A run's captured output.
 *
 * This replaced a `lines.map(...)` into a scrolling `<Box>`, which is a fine
 * way to show the fifty lines a passing build prints and the reason the tab
 * stopped responding on one that prints fifty thousand. Every line was a live
 * DOM node — so were the styled spans inside it — and the whole list was rebuilt
 * from scratch on every frame off the SSE stream. The browser was laying out a
 * hundred thousand nodes several times a second to show the forty that fit on
 * screen.
 *
 * So: only the visible window is mounted, lines are keyed by index so React
 * reuses the rows it already has, and the list sticks to the bottom the way a
 * terminal does — until you scroll up, which is the one moment sticking to the
 * bottom is the wrong thing to do.
 */

import { useCallback, useEffect, useLayoutEffect, useRef, useState } from "react";
import { Box, Stack, ToggleButton, Tooltip, Typography, useTheme } from "@mui/material";
import VerticalAlignBottomIcon from "@mui/icons-material/VerticalAlignBottom";
import WrapTextIcon from "@mui/icons-material/WrapText";
import { useVirtualizer } from "@tanstack/react-virtual";

import { AnsiText } from "./AnsiText";
import { monoFontStack } from "../theme";

/** Row height when lines aren't wrapped, in px. Matches the line-height below. */
const ROW = 18;

/**
 * How close to the bottom still counts as "at the bottom".
 *
 * Not zero: a wrapped row being measured a pixel differently than it was
 * estimated is enough to leave `scrollTop` a fraction short of the end, and a
 * log that stops following because of a rounding error looks broken.
 */
const STICK_SLACK = 40;

interface LogViewProps {
  lines: string[];
  /**
   * Lines the daemon dropped off the front of its buffer. Shown rather than
   * hidden — a log that silently begins in the middle gets read as the
   * beginning, and then the missing part is the part everyone looks for.
   */
  dropped?: number;
  /** Fills its container when true, for the side-by-side layout. */
  fill?: boolean;
  /** Fixed height when not filling. */
  height?: number;
}

export function LogView({ lines, dropped = 0, fill = false, height = 320 }: LogViewProps) {
  const theme = useTheme();
  // Resolved here rather than written as `"error.main"` per row: these are
  // inline styles, which take CSS colours and not theme tokens.
  const stderrColor = theme.palette.error.main;
  const scroller = useRef<HTMLDivElement>(null);
  const [wrap, setWrap] = useState(false);
  // Whether new output scrolls into view. On by default — a running build is
  // something you watch — and turned off the moment you scroll away from the
  // bottom, because following is only helpful while you're reading the end.
  const [follow, setFollow] = useState(true);

  const virtualizer = useVirtualizer({
    count: lines.length,
    getScrollElement: () => scroller.current,
    // Wrapped rows are measured, since their height depends on the width of the
    // pane and the length of the line. Unwrapped rows are all exactly `ROW`, so
    // the estimate is the answer and nothing needs measuring at all.
    estimateSize: () => ROW,
    overscan: 12,
  });

  const atBottom = useCallback(() => {
    const element = scroller.current;
    if (!element) return true;
    return element.scrollHeight - element.scrollTop - element.clientHeight <= STICK_SLACK;
  }, []);

  const onScroll = useCallback(() => {
    // Reading the scroll position rather than tracking wheel or key events, so
    // dragging the scrollbar, a trackpad flick and Page Up all behave the same.
    // Scrolling back to the bottom re-arms following, which is what a terminal
    // multiplexer does and what people expect without being told.
    setFollow(atBottom());
  }, [atBottom]);

  /**
   * Pin the view to the last line.
   *
   * Assigning `scrollTop` rather than calling `virtualizer.scrollToIndex`.
   * `scrollToIndex` computes the right offset and sets it — the container does
   * end up at the bottom — but on the first layout after mount the virtualizer
   * does not take its own programmatic scroll as a scroll, so it goes on
   * rendering the window for offset zero. The container is at the end and the
   * rows are at the beginning, which shows as an empty log pane until you
   * scroll by hand. A plain assignment fires the ordinary scroll event that the
   * virtualizer is already listening for, and it is what "follow the end"
   * literally means.
   *
   * The extra frame covers mount, where the rows have not been laid out yet and
   * `scrollHeight` is still the height of an empty list.
   */
  const stickToBottom = useCallback(() => {
    const pin = () => {
      const element = scroller.current;
      if (!element) return;
      element.scrollTop = element.scrollHeight;
      // The virtualizer decides which rows to mount from the scroll events it
      // hears, and the one this assignment fires at mount lands before it has
      // finished attaching its listener. Nothing fires afterwards — the
      // position is already right, so no further event is coming — and it
      // renders the top of the list into a pane scrolled to the bottom, which
      // reads as an empty log. Saying so explicitly costs one event and removes
      // the dependence on whose effect ran first.
      element.dispatchEvent(new Event("scroll"));
    };

    pin();
    // Again next frame: on mount the rows have not been laid out yet, so
    // `scrollHeight` is still the height of an empty list and the first pin
    // lands short of the end.
    requestAnimationFrame(pin);
  }, []);

  // Layout effect rather than effect: this runs once the new rows are laid out
  // but before paint, so the view never shows the old position for a frame.
  useLayoutEffect(() => {
    if (!follow || lines.length === 0) return;
    stickToBottom();
  }, [lines.length, follow, wrap, stickToBottom]);

  // Turning wrapping on changes every row's height, so the measurements taken
  // under the old setting are all wrong.
  useEffect(() => {
    virtualizer.measure();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [wrap]);

  const items = virtualizer.getVirtualItems();

  return (
    <Box
      sx={{
        border: 1,
        borderColor: "divider",
        borderRadius: 1,
        bgcolor: "background.default",
        display: "flex",
        flexDirection: "column",
        minHeight: 0,
        ...(fill ? { flex: 1 } : { height }),
      }}
    >
      <Stack
        direction="row"
        spacing={0.5}
        alignItems="center"
        sx={{ px: 1, py: 0.5, borderBottom: 1, borderColor: "divider", flexShrink: 0 }}
      >
        <Typography variant="caption" color="text.secondary" sx={{ flexGrow: 1, minWidth: 0 }}>
          {lines.length === 0
            ? "No output yet."
            : `${lines.length.toLocaleString()} line${lines.length === 1 ? "" : "s"}`}
          {dropped > 0 &&
            ` · ${dropped.toLocaleString()} earlier line${dropped === 1 ? "" : "s"} dropped`}
        </Typography>

        <Tooltip title={wrap ? "Don't wrap long lines" : "Wrap long lines"}>
          <ToggleButton
            value="wrap"
            size="small"
            selected={wrap}
            onChange={() => setWrap((on) => !on)}
            sx={{ border: 0, p: 0.5 }}
          >
            <WrapTextIcon fontSize="small" />
          </ToggleButton>
        </Tooltip>

        <Tooltip
          title={
            follow
              ? "Following new output — scroll up to stop"
              : "Jump to the end and follow new output"
          }
        >
          <ToggleButton
            value="follow"
            size="small"
            selected={follow}
            onChange={() => {
              setFollow(true);
              stickToBottom();
            }}
            sx={{ border: 0, p: 0.5 }}
          >
            <VerticalAlignBottomIcon fontSize="small" />
          </ToggleButton>
        </Tooltip>
      </Stack>

      <Box
        ref={scroller}
        onScroll={onScroll}
        sx={{
          flex: 1,
          minHeight: 0,
          overflowY: "auto",
          // Unwrapped lines run off to the right rather than reflowing, which is
          // what a terminal does and what keeps one row one line — the thing
          // that makes the virtual list exact instead of a guess.
          overflowX: wrap ? "hidden" : "auto",
          px: 1.5,
          py: 1,
          fontFamily: monoFontStack,
          fontSize: 12.5,
          lineHeight: `${ROW}px`,
        }}
      >
        {dropped > 0 && (
          <Typography
            variant="caption"
            color="text.secondary"
            sx={{ display: "block", fontStyle: "italic", mb: 0.5 }}
          >
            … {dropped.toLocaleString()} earlier line{dropped === 1 ? "" : "s"} dropped to keep the
            page responsive.
          </Typography>
        )}

        {/*
          Everything that varies per row goes in `style`, not `sx`. MUI's `sx`
          compiles each distinct style object into its own emotion class, so a
          per-row `translateY` would mint a fresh class — and a fresh stylesheet
          rule — for every line the list scrolls past, which is the cost this
          component exists to avoid. It also silently dropped the transform
          here, leaving the rows stacked at the top of a container scrolled to
          the bottom: a log pane that was blank until you scrolled back up.
        */}
        <div style={{ height: virtualizer.getTotalSize(), position: "relative" }}>
          {items.map((item) => (
            <div
              key={item.key}
              data-index={item.index}
              ref={wrap ? virtualizer.measureElement : undefined}
              style={{
                position: "absolute",
                top: 0,
                left: 0,
                width: "100%",
                transform: `translateY(${item.start}px)`,
                whiteSpace: wrap ? "pre-wrap" : "pre",
                wordBreak: wrap ? "break-word" : undefined,
                // stderr is marked by the daemon rather than inferred, so this
                // is a fact about the line, not a guess from its contents.
                color: lines[item.index].startsWith("[stderr]") ? stderrColor : undefined,
              }}
            >
              <AnsiText text={lines[item.index]} />
            </div>
          ))}
        </div>
      </Box>
    </Box>
  );
}
