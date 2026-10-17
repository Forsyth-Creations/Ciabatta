/**
 * One project at a glance: what's running, what's being watched, what's left
 * to do, what the cache holds — and the project's own README underneath.
 *
 * Each section is a summary with a way through to its tool, not the tool
 * itself: this is the page you land on after picking a project, so it says
 * where things stand and gets out of the way.
 */

import {
  Box,
  Button,
  Chip,
  Grid2 as Grid,
  Link as MuiLink,
  Paper,
  Stack,
  Typography,
} from "@mui/material";
import ArticleIcon from "@mui/icons-material/Article";
import ChecklistIcon from "@mui/icons-material/Checklist";
import InventoryIcon from "@mui/icons-material/Inventory2";
import MonitorHeartIcon from "@mui/icons-material/MonitorHeart";
import RocketLaunchIcon from "@mui/icons-material/RocketLaunch";
import { Link } from "@tanstack/react-router";
import type { ReactNode } from "react";

import { StatusIcon, statusLabel } from "@ciabatta/graphs";
import { humanizeMs, useCacheStatus } from "../api/cache";
import { useRuns } from "../api/run";
import { useWatchSessions } from "../api/watch";
import { ago } from "../components/CacheReport";
import { ErrorNote, Loading, PageHeader } from "../components/Page";
import { TodoList } from "../components/TodoList";
import { useProjectContext } from "../state/project";
import { monoFontStack } from "../theme";
import { ReadmeView } from "./ReadmePage";

export function ProjectDashboardPage() {
  const { project, projectId, isLoading } = useProjectContext();

  if (isLoading) return <Loading label="Loading projects…" />;
  if (!project || !projectId) {
    return (
      <Typography color="text.secondary">
        No project selected.{" "}
        <MuiLink component={Link} to="/">
          Pick one
        </MuiLink>{" "}
        to see its dashboard.
      </Typography>
    );
  }

  return (
    <Box>
      <PageHeader title={project.name} description={project.path} />

      <Grid container spacing={2}>
        <Grid size={{ xs: 12, lg: 6 }}>
          <RunsSection projectId={projectId} />
        </Grid>
        <Grid size={{ xs: 12, lg: 6 }}>
          <WatchSection projectId={projectId} />
        </Grid>
        <Grid size={{ xs: 12, lg: 6 }}>
          <Section icon={<ChecklistIcon />} title="Todo" to="/todo" action="All todos">
            <TodoList
              projectId={projectId}
              filter={(todo) => !todo.done}
              limit={5}
              emptyNote="Nothing left to do."
              footer={(hidden) => (
                <Typography variant="caption" color="text.secondary">
                  {hidden} more on the{" "}
                  <MuiLink component={Link} to="/todo">
                    Todo page
                  </MuiLink>
                  .
                </Typography>
              )}
            />
          </Section>
        </Grid>
        <Grid size={{ xs: 12, lg: 6 }}>
          <CacheSection projectId={projectId} />
        </Grid>
        <Grid size={12}>
          <Section icon={<ArticleIcon />} title="README" to="/readme" action="Open">
            <ReadmeView projectId={projectId} projectName={project.name} />
          </Section>
        </Grid>
      </Grid>
    </Box>
  );
}

/** A dashboard section: a titled panel, with a link through to its tool. */
function Section({
  icon,
  title,
  to,
  action,
  children,
}: {
  icon: ReactNode;
  title: string;
  to: string;
  action: string;
  children: ReactNode;
}) {
  return (
    <Paper variant="outlined" sx={{ p: 2, height: "100%" }}>
      <Stack direction="row" spacing={1} alignItems="center" sx={{ mb: 1.5 }}>
        <Box sx={{ color: "primary.main", display: "flex" }}>{icon}</Box>
        <Typography variant="h3" sx={{ flexGrow: 1 }}>
          {title}
        </Typography>
        <Button component={Link} to={to} size="small">
          {action}
        </Button>
      </Stack>
      {children}
    </Paper>
  );
}

/** A row in a section's list: an icon, a line of text, and a note on the right. */
function Row({
  icon,
  children,
  note,
}: {
  icon: ReactNode;
  children: ReactNode;
  note?: ReactNode;
}) {
  return (
    <Stack
      direction="row"
      spacing={1.25}
      alignItems="center"
      sx={{ py: 0.5, px: 0.5, borderRadius: 0.5, "&:hover": { bgcolor: "action.hover" } }}
    >
      <Box sx={{ display: "flex", flexShrink: 0 }}>{icon}</Box>
      <Box sx={{ flexGrow: 1, minWidth: 0 }}>{children}</Box>
      {note && (
        <Typography variant="caption" color="text.secondary" sx={{ flexShrink: 0 }}>
          {note}
        </Typography>
      )}
    </Stack>
  );
}

/** Runs in progress — or, when nothing is running, the last few that ran. */
function RunsSection({ projectId }: { projectId: string }) {
  const runs = useRuns(projectId);
  const all = runs.data ?? [];
  const active = all.filter((run) => !run.done);
  const shown = active.length > 0 ? active : all.slice(0, 4);

  return (
    <Section icon={<RocketLaunchIcon />} title="Runs" to="/run" action="All runs">
      {runs.error && <ErrorNote error={runs.error} />}
      <Typography variant="caption" color="text.secondary" sx={{ display: "block", mb: 0.5 }}>
        {active.length > 0
          ? `${active.length} in progress`
          : all.length > 0
            ? "Nothing running. The last few:"
            : "No runs yet."}
      </Typography>
      {shown.map((run) => (
        <MuiLink
          key={run.id}
          component={Link}
          to={`/run/${run.id}`}
          underline="none"
          color="inherit"
          sx={{ display: "block" }}
        >
          <Row
            icon={<StatusIcon status={run.status} title={statusLabel(run.status)} />}
            note={ago(run.started_at ?? run.created_at, true)}
          >
            <Typography variant="body2" noWrap>
              #{run.id} {run.workflows.join(", ")}
              {run.dry_run && (
                <Typography component="span" variant="caption" color="text.secondary">
                  {" "}
                  · dry run
                </Typography>
              )}
            </Typography>
          </Row>
        </MuiLink>
      ))}
    </Section>
  );
}

/** This project's watch sessions, the running ones first. */
function WatchSection({ projectId }: { projectId: string }) {
  const sessions = useWatchSessions();
  const mine = (sessions.data ?? [])
    .filter((session) => session.project === projectId)
    .sort((a, b) => Number(b.running) - Number(a.running) || b.id - a.id);
  const running = mine.filter((session) => session.running).length;

  return (
    <Section icon={<MonitorHeartIcon />} title="Watch" to="/watch" action="All sessions">
      {sessions.error && <ErrorNote error={sessions.error} />}
      <Typography variant="caption" color="text.secondary" sx={{ display: "block", mb: 0.5 }}>
        {mine.length === 0
          ? "No watch sessions. Start one with ciabatta watch <command>."
          : `${running} running · ${mine.length - running} finished`}
      </Typography>
      {mine.slice(0, 5).map((session) => (
        <MuiLink
          key={session.id}
          component={Link}
          to={`/watch/${session.id}`}
          underline="none"
          color="inherit"
          sx={{ display: "block" }}
        >
          <Row
            icon={
              <Chip
                size="small"
                variant="outlined"
                color={session.running ? "success" : "default"}
                label={session.running ? "running" : "done"}
                sx={{ height: 20, fontSize: 11 }}
              />
            }
            note={`${session.lines.toLocaleString()} lines`}
          >
            <Typography variant="body2" noWrap sx={{ fontFamily: monoFontStack, fontSize: 13 }}>
              {session.label ?? session.command}
            </Typography>
          </Row>
        </MuiLink>
      ))}
    </Section>
  );
}

/** What the local cache holds, or why it's off. */
function CacheSection({ projectId }: { projectId: string }) {
  const cache = useCacheStatus(projectId);
  const status = cache.data;

  return (
    <Section icon={<InventoryIcon />} title="Cache" to="/cache" action="Open cache">
      {cache.error && <ErrorNote error={cache.error} />}
      {cache.isLoading && <Loading label="Reading the cache…" />}
      {status && !status.enabled && (
        <Typography variant="body2" color="text.secondary">
          {/* The daemon's reason is already a sentence ("caching is off (set …)"). */}
          {status.why_disabled ?? "Caching is off."} <code>ciabatta cache init</code> sets it up
          for a workflow.
        </Typography>
      )}
      {status?.enabled && (
        <Stack direction="row" spacing={3} flexWrap="wrap" useFlexGap>
          <Stat label="entries" value={status.entries.toLocaleString()} />
          <Stat label="on disk" value={status.human} />
          <Stat
            label="build time stored"
            value={status.build_time_ms > 0 ? humanizeMs(status.build_time_ms) : "—"}
          />
          <Stat label="last stored" value={status.newest ? ago(status.newest, true) : "never"} />
          {status.remote && (
            <Stat
              label="remote"
              value={status.remote.enabled ? (status.remote.name ?? "on") : "off"}
            />
          )}
        </Stack>
      )}
    </Section>
  );
}

function Stat({ label, value }: { label: string; value: string }) {
  return (
    <Box>
      <Typography variant="h3" component="div">
        {value}
      </Typography>
      <Typography variant="caption" color="text.secondary">
        {label}
      </Typography>
    </Box>
  );
}
