/**
 * One watch session: the live log, plus search, bookmarks and triggers.
 *
 * The log is virtualized — the buffer holds up to 200,000 lines, so rendering
 * them all would be hopeless. Only the visible slice is ever in the DOM.
 */

import { useEffect, useMemo, useRef, useState } from "react";
import {
  Alert,
  Box,
  Button,
  Chip,
  Divider,
  Drawer,
  IconButton,
  InputAdornment,
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
import { useTheme } from "@mui/material/styles";
import ArrowBackIcon from "@mui/icons-material/ArrowBack";
import BookmarkAddIcon from "@mui/icons-material/BookmarkAdd";
import BookmarkIcon from "@mui/icons-material/Bookmark";
import ContentCopyIcon from "@mui/icons-material/ContentCopy";
import DeleteOutlineIcon from "@mui/icons-material/DeleteOutline";
import DownloadIcon from "@mui/icons-material/Download";
import IosShareIcon from "@mui/icons-material/IosShare";
import NotificationsActiveIcon from "@mui/icons-material/NotificationsActive";
import ScheduleIcon from "@mui/icons-material/Schedule";
import SearchIcon from "@mui/icons-material/Search";
import StopIcon from "@mui/icons-material/Stop";
import VerticalAlignBottomIcon from "@mui/icons-material/VerticalAlignBottom";
import { Link, useParams } from "@tanstack/react-router";
import { useMutation } from "@tanstack/react-query";
import { useVirtualizer } from "@tanstack/react-virtual";

import { api, downloadText } from "../api/client";
import { useSearch, useStopSession, type LogLine } from "../api/watch";
import { useWatchStream } from "../hooks/useWatchStream";
import { AnsiText, stripAnsi } from "../components/AnsiText";
import { ErrorNote, Loading } from "../components/Page";
import { monoFontStack } from "../theme";

const LINE_HEIGHT = 20;

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

  // When searching, the list shows matches instead of the live tail.
  const rows: LogLine[] = useMemo(
    () => (searching ? (search.data?.lines ?? []) : stream.lines),
    [searching, search.data, stream.lines],
  );

  const parentRef = useRef<HTMLDivElement>(null);
  const virtualizer = useVirtualizer({
    count: rows.length,
    getScrollElement: () => parentRef.current,
    estimateSize: () => LINE_HEIGHT,
    overscan: 40,
  });

  // Stick to the bottom while following. Only while not searching — jumping the
  // view around under someone reading search results would be hostile.
  useEffect(() => {
    if (follow && !searching && rows.length > 0) {
      virtualizer.scrollToIndex(rows.length - 1, { align: "end" });
    }
  }, [rows.length, follow, searching, virtualizer]);

  const addBookmark = useMutation({
    mutationFn: (line: LogLine) =>
      api.post(`/api/watch/sessions/${id}/bookmarks`, {
        seq: line.seq,
        // Without the colour escapes, or the 80 characters get spent on `[32m`
        // rather than on the part of the line that made it worth marking.
        label: stripAnsi(line.text).slice(0, 80),
      }),
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
        <TextField
          size="small"
          placeholder="Search the whole buffer…"
          value={query}
          onChange={(e) => setQuery(e.target.value)}
          sx={{ minWidth: 280, flexGrow: 1, maxWidth: 520 }}
          slotProps={{
            input: {
              startAdornment: (
                <InputAdornment position="start">
                  <SearchIcon fontSize="small" />
                </InputAdornment>
              ),
            },
          }}
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
          onClick={() => setFollow((f) => !f)}
          disabled={searching}
        >
          Follow
        </Button>

        {searching && (
          <Typography variant="body2" color="text.secondary">
            {search.data?.total ?? 0} matches
            {search.data?.capped && " (showing the first 2,000)"}
          </Typography>
        )}
      </Stack>

      {stream.error && <ErrorNote error={new Error(stream.error)} />}

      <Box
        ref={parentRef}
        // Scrolling away from the bottom turns follow off, so reading history
        // isn't fought by incoming output.
        onWheel={(e) => e.deltaY < 0 && follow && !searching && setFollow(false)}
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
            {searching ? "No matching lines." : "Waiting for output…"}
          </Typography>
        ) : (
          <Box sx={{ height: virtualizer.getTotalSize(), position: "relative" }}>
            {virtualizer.getVirtualItems().map((item) => {
              const line = rows[item.index];
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
                    "&:hover": { bgcolor: "action.hover" },
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
                  <IconButton
                    className="bookmark"
                    size="small"
                    sx={{ opacity: 0, transition: "opacity 120ms" }}
                    onClick={() => addBookmark.mutate(line)}
                  >
                    <BookmarkAddIcon sx={{ fontSize: 15 }} />
                  </IconButton>
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
}: {
  open: boolean;
  onClose: () => void;
  sessionId: number;
  stream: ReturnType<typeof useWatchStream>;
}) {
  const [pattern, setPattern] = useState("");

  const addTrigger = useMutation({
    mutationFn: (value: string) =>
      api.post(`/api/watch/sessions/${sessionId}/triggers`, { pattern: value, is_regex: false }),
    onSuccess: () => setPattern(""),
  });

  const removeTrigger = useMutation({
    mutationFn: (id: number) =>
      api.post(`/api/watch/sessions/${sessionId}/triggers/delete`, { id }),
  });

  const removeBookmark = useMutation({
    mutationFn: (id: number) =>
      api.post(`/api/watch/sessions/${sessionId}/bookmarks/delete`, { id }),
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
              <Box sx={{ flexGrow: 1, minWidth: 0 }}>
                <Typography variant="body2" sx={{ wordBreak: "break-word" }}>
                  {bookmark.label}
                </Typography>
                <Typography variant="caption" color="text.secondary">
                  line {bookmark.seq}
                </Typography>
              </Box>
              <IconButton size="small" onClick={() => removeBookmark.mutate(bookmark.id)}>
                <DeleteOutlineIcon fontSize="small" />
              </IconButton>
            </Stack>
          ))}
          {stream.bookmarks.length === 0 && (
            <Typography variant="body2" color="text.secondary">
              Hover a line and click the bookmark icon to save a point.
            </Typography>
          )}
        </Stack>
      </Box>
    </Drawer>
  );
}
