/**
 * Watch session list.
 *
 * Sessions belong to the daemon, not to whichever terminal started them, so
 * this page shows everything currently running regardless of where it came
 * from — including sessions started by `ciabatta watch` in another window.
 *
 * There is deliberately no "type a command and run it" box here. The daemon
 * runs on a developer's machine with their full privileges, and a text field
 * that hands it any shell string is a remote-execution surface reachable by
 * anything that can reach the port. Sessions start from the CLI, where the
 * person starting one is unambiguously the person at the keyboard.
 */

import { useEffect, useState } from "react";
import {
  Box,
  Button,
  Card,
  CardContent,
  Checkbox,
  Chip,
  IconButton,
  Stack,
  Tooltip,
  Typography,
} from "@mui/material";
import DeleteOutlineIcon from "@mui/icons-material/DeleteOutline";
import StopIcon from "@mui/icons-material/Stop";
import { styled } from "@mui/material/styles";
import CompareArrowsIcon from "@mui/icons-material/CompareArrows";
import { Link, useNavigate } from "@tanstack/react-router";

import { useCloseSession, useStopSession, useWatchSessions } from "../api/watch";
import { ErrorNote, Loading, PageHeader, RequireProject } from "../components/Page";
import { monoFontStack } from "../theme";

/**
 * The command text, as a link to its session.
 *
 * `styled(Link)` rather than MUI's `component={Link}`: the polymorphic
 * `component` prop drops Link's own props from the type.
 */
const CommandLink = styled(Link)(({ theme }) => ({
  fontFamily: monoFontStack,
  fontSize: 14,
  display: "block",
  color: theme.palette.text.primary,
  textDecoration: "none",
  overflow: "hidden",
  textOverflow: "ellipsis",
  whiteSpace: "nowrap",
  "&:hover": { textDecoration: "underline" },
}));

export function WatchPage() {
  return (
    <>
      <PageHeader
        title="Watch"
        description="Live, searchable output from the commands the daemon is running. Sessions outlive the terminal that started them. Start one from a terminal with ciabatta watch <command>. Tick two sessions and press Triage to compare their logs."
      />
      <RequireProject>{() => <SessionList />}</RequireProject>
    </>
  );
}

function SessionList() {
  const { data: sessions, isLoading, error } = useWatchSessions();
  const stop = useStopSession();
  const close = useCloseSession();
  const navigate = useNavigate();

  // Sessions ticked for triage, oldest pick first. A third tick drops the
  // oldest, so there's never a "you can only pick two" dead end.
  const [picked, setPicked] = useState<number[]>([]);
  const toggle = (id: number) =>
    setPicked((now) =>
      now.includes(id) ? now.filter((p) => p !== id) : [...now, id].slice(-2),
    );

  // A discarded session can't stay ticked.
  useEffect(() => {
    if (!sessions) return;
    setPicked((now) => now.filter((id) => sessions.some((s) => s.id === id)));
  }, [sessions]);

  // The older session is A: the usual question is "it worked, now it doesn't —
  // what's new?", so B's unique lines are the new ones.
  const triage = () => {
    const [a, b] = [...picked].sort((x, y) => x - y);
    navigate({ to: "/triage", search: { a, b } });
  };

  return (
    <>
      {error && <ErrorNote error={error} />}

      {isLoading ? (
        <Loading label="Loading sessions…" />
      ) : !sessions?.length ? (
        <Typography variant="body2" color="text.secondary">
          No watch sessions. Start one with{" "}
          <code style={{ fontFamily: monoFontStack }}>ciabatta watch "npm run dev"</code> in a
          terminal — it appears here as soon as it's running.
        </Typography>
      ) : (
        <Stack spacing={1} sx={{ maxWidth: 1100 }}>
          <Stack direction="row" alignItems="center" spacing={2} sx={{ minHeight: 40 }}>
            <Button
              variant="contained"
              startIcon={<CompareArrowsIcon />}
              disabled={picked.length !== 2}
              onClick={triage}
            >
              Triage
            </Button>
            <Typography variant="body2" color="text.secondary">
              {sessions.length < 2
                ? "Start a second session to compare two logs."
                : picked.length === 2
                  ? `Compare #${Math.min(...picked)} (A) with #${Math.max(...picked)} (B): the lines only one of them printed.`
                  : `Tick ${picked.length === 0 ? "two sessions" : "one more session"} to compare their logs.`}
            </Typography>
            {picked.length > 0 && (
              <Button size="small" onClick={() => setPicked([])}>
                Clear
              </Button>
            )}
          </Stack>
          {sessions.map((session) => (
            <Card key={session.id}>
              <CardContent sx={{ py: 1.5, "&:last-child": { pb: 1.5 } }}>
                <Stack direction="row" alignItems="center" spacing={2}>
                  <Tooltip title="Select for triage">
                    <Checkbox
                      size="small"
                      checked={picked.includes(session.id)}
                      onChange={() => toggle(session.id)}
                      sx={{ p: 0.5, ml: -0.5 }}
                      slotProps={{ input: { "aria-label": `Select session ${session.id} for triage` } }}
                    />
                  </Tooltip>
                  <Chip
                    size="small"
                    color={session.running ? "success" : "default"}
                    variant="outlined"
                    label={session.running ? "running" : "finished"}
                  />

                  <Box sx={{ flexGrow: 1, minWidth: 0 }}>
                    {/* A label identifies a session far better than the
                        command line it happens to run. */}
                    {session.label && (
                      <Typography variant="body2" sx={{ fontWeight: 600 }}>
                        {session.label}
                      </Typography>
                    )}
                    <CommandLink to={`/watch/${session.id}`}>{session.command}</CommandLink>
                    <Typography variant="caption" color="text.secondary">
                      #{session.id} · {session.lines.toLocaleString()} lines · started{" "}
                      {new Date(session.created_at).toLocaleTimeString()}
                    </Typography>
                  </Box>

                  {session.running && (
                    <Tooltip title="Stop the command">
                      <IconButton size="small" onClick={() => stop.mutate(session.id)}>
                        <StopIcon fontSize="small" />
                      </IconButton>
                    </Tooltip>
                  )}
                  <Tooltip title="Discard this session and its output">
                    <IconButton size="small" onClick={() => close.mutate(session.id)}>
                      <DeleteOutlineIcon fontSize="small" />
                    </IconButton>
                  </Tooltip>
                </Stack>
              </CardContent>
            </Card>
          ))}
        </Stack>
      )}
    </>
  );
}
