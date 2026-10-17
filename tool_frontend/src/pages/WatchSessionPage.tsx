/**
 * One watch session: the live log, plus search, bookmarks and triggers.
 *
 * The log is virtualized — the buffer holds up to 200,000 lines, so rendering
 * them all would be hopeless. Only the visible slice is ever in the DOM.
 *
 * Search finds lines in place rather than filtering down to them: matches are
 * tinted where they sit and Enter steps between them, so each is read with
 * the lines around it. The daemon does the matching, over its whole buffer.
 */

import { useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import {
  Alert,
  Box,
  Button,
  Chip,
  Divider,
  Drawer,
  IconButton,
  ListItemIcon,
  ListItemText,
  Menu,
  MenuItem,
  Snackbar,
  Stack,
  TextField,
  ToggleButton,
  ToggleButtonGroup,
  Tooltip,
  Typography,
} from "@mui/material";
import { alpha, useTheme } from "@mui/material/styles";
import ArrowBackIcon from "@mui/icons-material/ArrowBack";
import BookmarkAddIcon from "@mui/icons-material/BookmarkAdd";
import BookmarkIcon from "@mui/icons-material/Bookmark";
import ContentCopyIcon from "@mui/icons-material/ContentCopy";
import DeleteOutlineIcon from "@mui/icons-material/DeleteOutline";
import DownloadIcon from "@mui/icons-material/Download";
import IosShareIcon from "@mui/icons-material/IosShare";
import NotificationsActiveIcon from "@mui/icons-material/NotificationsActive";
import ScheduleIcon from "@mui/icons-material/Schedule";
import StopIcon from "@mui/icons-material/Stop";
import VerticalAlignBottomIcon from "@mui/icons-material/VerticalAlignBottom";
import { Link, useParams } from "@tanstack/react-router";
import { useMutation } from "@tanstack/react-query";
import { useVirtualizer } from "@tanstack/react-virtual";

import { api, downloadText } from "../api/client";
import { useSearch, useStopSession, type Bookmark, type LogLine } from "../api/watch";
import { useWatchStream } from "../hooks/useWatchStream";
import { AnsiText, stripAnsi } from "../components/AnsiText";
import { ErrorNote, Loading } from "../components/Page";
import { FindBar, stepMatch } from "../components/FindBar";
import { monoFontStack } from "../theme";

const LINE_HEIGHT = 20;

/** Where line `seq` is in `lines`, which are in `seq` order; -1 when it has
 *  dropped out of the buffer. A binary search: the buffer runs to 200,000. */
function indexOfSeq(lines: LogLine[], seq: number): number {
  let low = 0;
  let high = lines.length - 1;
  while (low <= high) {
    const mid = (low + high) >> 1;
    const at = lines[mid].seq;
    if (at === seq) return mid;
    if (at < seq) low = mid + 1;
    else high = mid - 1;
  }
  return -1;
}

export function WatchSessionPage() {
  const { sessionId } = useParams({ from: "/watch/$sessionId" });
  const id = Number(sessionId);
  const theme = useTheme();

  const stream = useWatchStream(id);
  const stop = useStopSession();

  const [follow, setFollow] = useState(true);
  const [query, setQuery] = useState("");
  const [mode, setMode] = useState<"any" | "all">("any");
  const [sidebar, setSidebar] = useState(false);

  const search = useSearch(id, query, mode, false);
  const searching = query.trim().length > 0;
  // The match that Enter last stepped to, by its place among the matches.
  const [current, setCurrent] = useState(0);
  // A line to point out for a moment — the one a bookmark was clicked for.
  const [flash, setFlash] = useState<number | null>(null);

  // The whole log, always: a search marks lines, it doesn't hide the others.
  const rows: LogLine[] = stream.lines;
  // Matches still in the page's copy of the log, in order. One that has
  // dropped off the front of the buffer has nowhere to be shown.
  const matches = useMemo(
    () =>
      searching
        ? (search.data?.lines ?? [])
            .map((line) => line.seq)
            .filter((seq) => indexOfSeq(rows, seq) !== -1)
        : [],
    [searching, search.data, rows],
  );
  const matched = useMemo(() => new Set(matches), [matches]);
  const currentSeq = matches.length > 0 ? matches[Math.min(current, matches.length - 1)] : null;
  const marked = useMemo(
    () => new Map(stream.bookmarks.map((mark) => [mark.seq, mark])),
    [stream.bookmarks],
  );

  const parentRef = useRef<HTMLDivElement>(null);
  const virtualizer = useVirtualizer({
    count: rows.length,
    getScrollElement: () => parentRef.current,
    estimateSize: () => LINE_HEIGHT,
    overscan: 40,
  });

  // `follow` is also read from a scroll handler, which fires far too often to
  // wait for a re-render: by the time React had re-rendered with follow off,
  // several more frames of output would have pinned the view back down.
  const followRef = useRef(follow);
  const setFollowing = (next: boolean) => {
    followRef.current = next;
    setFollow(next);
  };

  // Whether the viewport is at the bottom, within a line. Not an exact
  // comparison: scrollHeight and scrollTop are fractional under browser zoom
  // and on hidpi displays, so `=== 0` would be false at the bottom.
  const atBottom = (el: HTMLElement) =>
    el.scrollHeight - el.scrollTop - el.clientHeight <= LINE_HEIGHT;

  // Follow is a fact about where the viewport is, not a gesture to intercept.
  //
  // It used to be turned off by an `onWheel` handler, which meant it only
  // noticed a mouse wheel — dragging the scrollbar, PageUp, Home, arrow keys
  // and touch scrolling all left it on, so the next line of output yanked the
  // view straight back to the bottom. Deriving it from the scroll position
  // instead covers every way there is to scroll, including ones that don't
  // exist yet. A scroll we caused ourselves lands at the bottom and so reads
  // as follow-on, which is what it already was: no need to tell the two apart.
  const onScroll = () => {
    const el = parentRef.current;
    if (!el) return;
    const bottom = atBottom(el);
    if (bottom !== followRef.current) setFollowing(bottom);
  };

  // Stick to the bottom while following. Stepping to a match turns following
  // off, so a match being read isn't scrolled away by the next line of output.
  //
  // Setting scrollTop directly rather than going through the virtualizer:
  // `scrollToIndex` schedules retries on later frames to land accurately on
  // dynamically-sized rows, and with output arriving every frame there was
  // always one in flight to undo the scroll the reader had just made. Rows here
  // are a fixed LINE_HEIGHT, so the bottom is just the bottom. Layout effect, so
  // it runs after the new rows are in the DOM but before the browser paints —
  // no visible jump.
  useLayoutEffect(() => {
    const el = parentRef.current;
    if (!el || !follow || rows.length === 0) return;
    el.scrollTop = el.scrollHeight;
  }, [rows.length, follow]);

  /** Bring line `seq` into the middle of the view, and stop following. */
  const showLine = (seq: number) => {
    const el = parentRef.current;
    const index = indexOfSeq(rows, seq);
    if (!el || index === -1) return;
    setFollowing(false);
    el.scrollTop = index * LINE_HEIGHT - el.clientHeight / 2 + LINE_HEIGHT / 2;
  };
  const showMatch = (index: number) => {
    if (matches[index] === undefined) return;
    setCurrent(index);
    showLine(matches[index]);
  };

  // New results start at the first match from where you're reading, the way
  // a browser's find does — not back at the top of the log.
  useEffect(() => {
    const el = parentRef.current;
    if (!el || matches.length === 0) return;
    const top = rows[Math.floor(el.scrollTop / LINE_HEIGHT)]?.seq ?? 0;
    const next = matches.findIndex((seq) => seq >= top);
    showMatch(next === -1 ? 0 : next);
    // Only for a new set of results: output arriving mustn't move the view.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [search.data]);

  const jumpToBookmark = (mark: Bookmark) => {
    setSidebar(false);
    showLine(mark.seq);
    setFlash(mark.seq);
    window.setTimeout(() => setFlash((seq) => (seq === mark.seq ? null : seq)), 1600);
  };

  const addBookmark = useMutation({
    mutationFn: (line: LogLine) =>
      api.post(`/api/watch/sessions/${id}/bookmarks`, {
        seq: line.seq,
        // Without the colour escapes, or the 80 characters get spent on `[32m`
        // rather than on the part of the line that made it worth marking.
        label: stripAnsi(line.text).slice(0, 80),
      }),
    onSuccess: () => stream.refresh(),
  });

  const removeBookmark = useMutation({
    mutationFn: (bookmarkId: number) =>
      api.post(`/api/watch/sessions/${id}/bookmarks/delete`, { id: bookmarkId }),
    onSuccess: () => stream.refresh(),
  });

  if (!stream.ready && !stream.error) return <Loading label="Connecting to the session…" />;

  return (
    <Box sx={{ display: "flex", flexDirection: "column", height: "calc(100vh - 128px)" }}>
      <Stack direction="row" alignItems="center" spacing={1.5} sx={{ mb: 1.5 }}>
        <IconButton component={Link} to="/watch" size="small" aria-label="Back to sessions">
          <ArrowBackIcon />
        </IconButton>

        <Box sx={{ minWidth: 0, flexGrow: 1 }}>
          <Typography
            sx={{
              fontFamily: monoFontStack,
              fontSize: 15,
              overflow: "hidden",
              textOverflow: "ellipsis",
              whiteSpace: "nowrap",
            }}
          >
            {stream.command}
          </Typography>
          <Typography variant="caption" color="text.secondary">
            #{id} · {stream.lines.length.toLocaleString()} lines buffered
          </Typography>
        </Box>

        <StatusChip stream={stream} />

        <ExportMenu id={id} />

        {stream.running && (
          <Tooltip title="Stop the command">
            <IconButton size="small" onClick={() => stop.mutate(id)}>
              <StopIcon />
            </IconButton>
          </Tooltip>
        )}

        <Tooltip title="Bookmarks and triggers">
          <IconButton size="small" onClick={() => setSidebar(true)}>
            <BookmarkIcon />
          </IconButton>
        </Tooltip>
      </Stack>

      <Stack direction="row" spacing={1} alignItems="center" sx={{ mb: 1.5 }} flexWrap="wrap" useFlexGap>
        <FindBar
          value={query}
          onChange={(next) => {
            setQuery(next);
            setCurrent(0);
          }}
          count={matches.length}
          current={Math.min(current, Math.max(matches.length - 1, 0))}
          onStep={(by) => showMatch(stepMatch(current, matches.length, by))}
          placeholder="Search the whole buffer…"
          width={420}
          note={search.data?.capped ? "Showing the first 2,000 matches" : undefined}
        />

        <ToggleButtonGroup
          size="small"
          exclusive
          value={mode}
          onChange={(_, next: "any" | "all" | null) => next && setMode(next)}
        >
          <Tooltip title="Match any term">
            <ToggleButton value="any">any</ToggleButton>
          </Tooltip>
          <Tooltip title="Match every term">
            <ToggleButton value="all">all</ToggleButton>
          </Tooltip>
        </ToggleButtonGroup>

        <Button
          size="small"
          variant={follow ? "contained" : "outlined"}
          startIcon={<VerticalAlignBottomIcon />}
          onClick={() => setFollowing(!follow)}
        >
          Follow
        </Button>

        {searching && search.data?.capped && (
          <Typography variant="caption" color="text.secondary">
            {search.data.total.toLocaleString()} matches — stepping through the first 2,000
          </Typography>
        )}
      </Stack>

      {stream.error && <ErrorNote error={new Error(stream.error)} />}

      <Box
        ref={parentRef}
        // Scrolling away from the bottom turns follow off, so reading history
        // isn't fought by incoming output — and scrolling back to the bottom
        // turns it on again, which is the only thing anyone means by it.
        onScroll={onScroll}
        sx={{
          flexGrow: 1,
          overflow: "auto",
          border: 1,
          borderColor: "divider",
          borderRadius: 1,
          bgcolor: "background.default",
        }}
      >
        {rows.length === 0 ? (
          <Typography variant="body2" color="text.secondary" sx={{ p: 2 }}>
            Waiting for output…
          </Typography>
        ) : (
          <Box sx={{ height: virtualizer.getTotalSize(), position: "relative" }}>
            {virtualizer.getVirtualItems().map((item) => {
              const line = rows[item.index];
              const isCurrent = line.seq === currentSeq || line.seq === flash;
              const mark = marked.get(line.seq);
              return (
                <Box
                  key={item.key}
                  sx={{
                    position: "absolute",
                    top: 0,
                    left: 0,
                    width: "100%",
                    height: LINE_HEIGHT,
                    transform: `translateY(${item.start}px)`,
                    display: "flex",
                    alignItems: "center",
                    gap: 1.5,
                    px: 1.5,
                    fontFamily: monoFontStack,
                    fontSize: 12.5,
                    whiteSpace: "pre",
                    color: line.stream === "stderr" ? "error.main" : "text.primary",
                    bgcolor: isCurrent
                      ? alpha(theme.palette.warning.main, 0.38)
                      : matched.has(line.seq)
                        ? alpha(theme.palette.warning.main, 0.14)
                        : undefined,
                    boxShadow: isCurrent ? `inset 2px 0 ${theme.palette.warning.main}` : undefined,
                    transition: "background-color 300ms",
                    "&:hover": isCurrent ? undefined : { bgcolor: "action.hover" },
                    "&:hover .bookmark": { opacity: 1 },
                  }}
                >
                  <Box component="span" sx={{ color: "text.disabled", userSelect: "none" }}>
                    {String(line.seq).padStart(6, " ")}
                  </Box>
                  <Box component="span" sx={{ flexGrow: 1, overflow: "hidden" }}>
                    <AnsiText
                      text={line.text}
                      fallbackColor={
                        line.stream === "stderr"
                          ? theme.palette.error.main
                          : theme.palette.text.primary
                      }
                    />
                  </Box>
                  {mark ? (
                    // A bookmarked line keeps its mark in view; clicking it
                    // takes the bookmark off again.
                    <Tooltip title={`Bookmarked: ${mark.label} — click to remove`}>
                      <IconButton
                        size="small"
                        aria-label="Remove this bookmark"
                        onClick={() => removeBookmark.mutate(mark.id)}
                      >
                        <BookmarkIcon color="primary" sx={{ fontSize: 15 }} />
                      </IconButton>
                    </Tooltip>
                  ) : (
                    <IconButton
                      className="bookmark"
                      size="small"
                      aria-label="Bookmark this line"
                      disabled={addBookmark.isPending}
                      sx={{ opacity: 0, transition: "opacity 120ms" }}
                      onClick={() => addBookmark.mutate(line)}
                    >
                      <BookmarkAddIcon sx={{ fontSize: 15 }} />
                    </IconButton>
                  )}
                </Box>
              );
            })}
          </Box>
        )}
      </Box>

      <SidePanel
        open={sidebar}
        onClose={() => setSidebar(false)}
        sessionId={id}
        stream={stream}
        onJump={jumpToBookmark}
        onRemoveBookmark={(bookmarkId) => removeBookmark.mutate(bookmarkId)}
      />
    </Box>
  );
}

/**
 * Save or share the session log.
 *
 * The transcript is built by the daemon, not from `stream.lines`: the page only
 * holds what it has streamed since you opened it, while the daemon has the
 * whole buffer plus the command, the exit status, and your bookmarks. Sending
 * someone the partial version you happened to be looking at is exactly the
 * failure mode a share button should not have.
 */
function ExportMenu({ id }: { id: number }) {
  const [anchor, setAnchor] = useState<HTMLElement | null>(null);
  const [toast, setToast] = useState<string | null>(null);

  const fetchLog = (timestamps: boolean) =>
    api.text(`/api/watch/sessions/${id}/export?timestamps=${timestamps}`);

  const save = useMutation({
    mutationFn: async (timestamps: boolean) => {
      const { text, filename } = await fetchLog(timestamps);
      downloadText(filename, text);
      return filename;
    },
    onSuccess: (filename) => setToast(`Saved ${filename}`),
    onError: (error: Error) => setToast(`Couldn't save the log: ${error.message}`),
  });

  const copy = useMutation({
    mutationFn: async () => {
      const { text } = await fetchLog(false);
      await navigator.clipboard.writeText(text);
      return text;
    },
    onSuccess: (text) =>
      setToast(`Copied ${text.split("\n").length.toLocaleString()} lines to the clipboard`),
    // Clipboard writes are refused outside a secure context, which is worth
    // saying plainly rather than failing silently.
    onError: () => setToast("Couldn't copy — save the file instead."),
  });

  const choose = (action: () => void) => () => {
    setAnchor(null);
    action();
  };

  return (
    <>
      <Tooltip title="Save or share this log">
        <IconButton
          size="small"
          onClick={(e) => setAnchor(e.currentTarget)}
          disabled={save.isPending || copy.isPending}
          aria-label="Export this log"
        >
          <IosShareIcon />
        </IconButton>
      </Tooltip>

      <Menu anchorEl={anchor} open={Boolean(anchor)} onClose={() => setAnchor(null)}>
        <MenuItem onClick={choose(() => save.mutate(false))}>
          <ListItemIcon>
            <DownloadIcon fontSize="small" />
          </ListItemIcon>
          <ListItemText primary="Download as a file" secondary="Command, status and bookmarks included" />
        </MenuItem>
        <MenuItem onClick={choose(() => save.mutate(true))}>
          <ListItemIcon>
            <ScheduleIcon fontSize="small" />
          </ListItemIcon>
          <ListItemText primary="Download with timestamps" secondary="For working out where it stalled" />
        </MenuItem>
        <MenuItem onClick={choose(() => copy.mutate())}>
          <ListItemIcon>
            <ContentCopyIcon fontSize="small" />
          </ListItemIcon>
          <ListItemText primary="Copy to clipboard" secondary="Paste straight into a ticket or chat" />
        </MenuItem>
      </Menu>

      <Snackbar
        open={toast !== null}
        autoHideDuration={4000}
        onClose={() => setToast(null)}
        message={toast}
      />
    </>
  );
}

function StatusChip({ stream }: { stream: ReturnType<typeof useWatchStream> }) {
  if (stream.running) {
    return <Chip size="small" color="success" variant="outlined" label="running" />;
  }
  const status = stream.status;
  if (!status) return null;

  switch (status.kind) {
    case "exited":
      return (
        <Chip
          size="small"
          color={status.code === 0 ? "default" : "error"}
          variant="outlined"
          label={`exited ${status.code}`}
        />
      );
    case "signaled":
      return <Chip size="small" color="warning" variant="outlined" label="stopped" />;
    case "failed":
      return <Chip size="small" color="error" variant="outlined" label="failed" />;
    default:
      return null;
  }
}

/** Bookmarks and triggers, kept out of the way until asked for. */
function SidePanel({
  open,
  onClose,
  sessionId,
  stream,
  onJump,
  onRemoveBookmark,
}: {
  open: boolean;
  onClose: () => void;
  sessionId: number;
  stream: ReturnType<typeof useWatchStream>;
  /** Show a bookmark's line in the log. */
  onJump: (bookmark: Bookmark) => void;
  onRemoveBookmark: (id: number) => void;
}) {
  const [pattern, setPattern] = useState("");

  const addTrigger = useMutation({
    mutationFn: (value: string) =>
      api.post(`/api/watch/sessions/${sessionId}/triggers`, { pattern: value, is_regex: false }),
    onSuccess: () => {
      setPattern("");
      stream.refresh();
    },
  });

  const removeTrigger = useMutation({
    mutationFn: (id: number) =>
      api.post(`/api/watch/sessions/${sessionId}/triggers/delete`, { id }),
    onSuccess: () => stream.refresh(),
  });

  return (
    <Drawer
      anchor="right"
      open={open}
      onClose={onClose}
      // The AppBar deliberately sits above the *nav* drawer (zIndex.drawer + 1)
      // so the permanent rail tucks under it. This panel is a transient overlay
      // and has to clear the AppBar instead, or its top slips behind it.
      sx={{ zIndex: (theme) => theme.zIndex.drawer + 2 }}
    >
      <Box sx={{ width: 380, p: 2 }}>
        <Typography variant="h3" sx={{ mb: 2 }}>
          Triggers
        </Typography>

        <Box
          component="form"
          onSubmit={(e) => {
            e.preventDefault();
            if (pattern.trim()) addTrigger.mutate(pattern.trim());
          }}
        >
          <TextField
            fullWidth
            size="small"
            placeholder="e.g. error"
            value={pattern}
            onChange={(e) => setPattern(e.target.value)}
            helperText="Matching lines raise a notification. Persisted per command."
          />
        </Box>

        <Stack spacing={0.5} sx={{ mt: 1.5 }}>
          {stream.triggers.map((trigger) => (
            <Stack key={trigger.id} direction="row" alignItems="center" spacing={1}>
              <NotificationsActiveIcon fontSize="small" color="warning" />
              <Typography sx={{ flexGrow: 1, fontFamily: monoFontStack, fontSize: 13 }}>
                {trigger.pattern}
              </Typography>
              <Chip size="small" label={trigger.hits} />
              <IconButton size="small" onClick={() => removeTrigger.mutate(trigger.id)}>
                <DeleteOutlineIcon fontSize="small" />
              </IconButton>
            </Stack>
          ))}
          {stream.triggers.length === 0 && (
            <Typography variant="body2" color="text.secondary">
              No triggers set.
            </Typography>
          )}
        </Stack>

        {stream.hits.length > 0 && (
          <Alert severity="warning" sx={{ mt: 2 }}>
            {stream.hits.length} trigger hit{stream.hits.length === 1 ? "" : "s"} in this view.
          </Alert>
        )}

        <Divider sx={{ my: 3 }} />

        <Typography variant="h3" sx={{ mb: 2 }}>
          Bookmarks
        </Typography>
        <Stack spacing={1}>
          {stream.bookmarks.map((bookmark) => (
            <Stack key={bookmark.id} direction="row" alignItems="flex-start" spacing={1}>
              <BookmarkIcon fontSize="small" color="primary" sx={{ mt: 0.25 }} />
              <Box
                component="button"
                type="button"
                onClick={() => onJump(bookmark)}
                title="Show this line in the log"
                sx={{
                  flexGrow: 1,
                  minWidth: 0,
                  p: 0,
                  border: 0,
                  bgcolor: "transparent",
                  color: "inherit",
                  font: "inherit",
                  textAlign: "left",
                  cursor: "pointer",
                  borderRadius: 0.5,
                  "&:hover": { bgcolor: "action.hover" },
                }}
              >
                <Typography variant="body2" sx={{ wordBreak: "break-word" }}>
                  {bookmark.label}
                </Typography>
                <Typography variant="caption" color="text.secondary">
                  line {bookmark.seq}
                </Typography>
              </Box>
              <IconButton
                size="small"
                aria-label="Remove this bookmark"
                onClick={() => onRemoveBookmark(bookmark.id)}
              >
                <DeleteOutlineIcon fontSize="small" />
              </IconButton>
            </Stack>
          ))}
          {stream.bookmarks.length === 0 && (
            <Typography variant="body2" color="text.secondary">
              Hover a line and click the bookmark icon to save a point. Click a
              bookmark here to jump back to it.
            </Typography>
          )}
        </Stack>
      </Box>
    </Drawer>
  );
}
