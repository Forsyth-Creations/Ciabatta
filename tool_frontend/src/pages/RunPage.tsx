/**
 * Run launcher and run list.
 *
 * Runs belong to the daemon, so this lists everything in flight regardless of
 * which terminal kicked it off — and a run stays here, logs and all, after it
 * finishes.
 */

import { useState } from "react";
import {
  Alert,
  Box,
  Button,
  Card,
  CardContent,
  Chip,
  FormControlLabel,
  IconButton,
  MenuItem,
  Stack,
  Switch,
  TextField,
  Tooltip,
  Typography,
} from "@mui/material";
import AccountTreeIcon from "@mui/icons-material/AccountTree";
import DeleteOutlineIcon from "@mui/icons-material/DeleteOutline";
import PlayArrowIcon from "@mui/icons-material/PlayArrow";
import { styled } from "@mui/material/styles";
import { Link, useNavigate } from "@tanstack/react-router";

import {
  missingEnvFrom,
  useDeleteRun,
  useRunSettings,
  useRunTargets,
  useRuns,
  useSetRunSettings,
  useStartRun,
  type RunSummary,
} from "../api/run";
import { EnvPrompt } from "../components/EnvPrompt";
import { StatusIcon, statusLabel } from "../components/StatusIcon";
import { EnvDriftBanner } from "../components/EnvDriftBanner";
import { ErrorNote, Loading, PageHeader, RequireProject } from "../components/Page";
import { monoFontStack } from "../theme";

const RunLink = styled(Link)(({ theme }) => ({
  fontFamily: monoFontStack,
  fontSize: 14,
  color: theme.palette.text.primary,
  textDecoration: "none",
  "&:hover": { textDecoration: "underline" },
}));

export function RunPage() {
  return (
    <>
      <PageHeader
        title="Run"
        description="Execute a workflow's step DAG live, with fix-it branches when a step fails. The daemon owns the run, so it survives closing the terminal."
        actions={
          <Button component={Link} to="/run/builder" startIcon={<AccountTreeIcon />}>
            Flowchart builder
          </Button>
        }
      />
      <RequireProject>{(project) => <Launcher project={project} />}</RequireProject>
    </>
  );
}

function Launcher({ project }: { project: string }) {
  const navigate = useNavigate();
  const { targets, isLoading: loadingTargets } = useRunTargets(project);
  const { data: runs, isLoading, error } = useRuns();
  const start = useStartRun();

  const [selected, setSelected] = useState<string>("");
  const [dryRun, setDryRun] = useState(false);
  const [filter, setFilter] = useState("");

  // Values typed into the missing-variable prompt. They persist across
  // attempts, so answering a second round of prompts doesn't lose the first.
  const [env, setEnv] = useState<Record<string, string>>({});
  const [prompting, setPrompting] = useState<string[] | null>(null);

  const target = targets.find((t) => t.name === selected);

  const launch = (withEnv: Record<string, string>) => {
    setEnv(withEnv);
    // A workflow compiles a cross-package graph on the daemon; a workflow runs
    // this project's own steps. Same endpoint, same flags — the launcher just
    // has to say which kind of name it picked.
    const terms = filter
      .split(/\s+/)
      .map((term) => term.trim())
      .filter(Boolean);
    const body =
      target?.kind === "workflow"
        ? { project, workflows: [], workflow: selected, filter: terms, dry_run: dryRun, env: withEnv }
        : {
            project,
            workflows: selected ? [selected] : [],
            filter: terms,
            dry_run: dryRun,
            env: withEnv,
          };

    start.mutate(body, {
      onSuccess: (run) => {
        setPrompting(null);
        navigate({ to: "/run/$runId", params: { runId: String(run.id) } });
      },
      // A run the daemon won't start for want of variables isn't a failure to
      // report — it's a question to ask. Sourcing an env file can reveal more
      // once the first answers land, so this may open more than once.
      onError: (error) => setPrompting(missingEnvFrom(error)),
    });
  };

  if (loadingTargets) return <Loading label="Loading what this project can run…" />;

  return (
    <>
      {/* Before the launcher, not after: a changed variable is something to
          know about while deciding whether to start a run. */}
      <EnvDriftBanner project={project} />

      {targets.length === 0 ? (
        <Alert severity="info" sx={{ mb: 3 }}>
          Nothing to run in this project yet. Opt a package in with{" "}
          <code style={{ fontFamily: monoFontStack }}>ciabatta init --lib</code>, add a{" "}
          <code style={{ fontFamily: monoFontStack }}>[recipies.&lt;name&gt;.run]</code> section, or
          generate a worked example with{" "}
          <code style={{ fontFamily: monoFontStack }}>ciabatta init --example</code>.
        </Alert>
      ) : (
        <Stack spacing={1.5} sx={{ mb: 3 }}>
          <Stack direction="row" spacing={2} alignItems="center" flexWrap="wrap" useFlexGap>
            {/* Workflows and workflows in one list: they are the same kind of
                thing, and making someone know which they have before they can
                start it is the distinction this tool exists to remove. */}
            <TextField
              select
              size="small"
              label="Workflow or workflow"
              value={selected}
              onChange={(e) => setSelected(e.target.value)}
              sx={{ minWidth: 260 }}
            >
              <MenuItem value="">
                <em>All run-capable workflows</em>
              </MenuItem>
              {targets.map((t) => (
                <MenuItem key={`${t.kind}:${t.name}`} value={t.name}>
                  <Stack direction="row" spacing={1} alignItems="center" sx={{ width: "100%" }}>
                    <Box component="span" sx={{ fontFamily: monoFontStack }}>
                      {t.name}
                    </Box>
                    <Chip
                      size="small"
                      variant="outlined"
                      color={t.kind === "workflow" ? "primary" : "default"}
                      label={
                        t.kind === "workflow"
                          ? `${t.members.length} package${t.members.length === 1 ? "" : "s"}`
                          : "workflow"
                      }
                    />
                  </Stack>
                </MenuItem>
              ))}
            </TextField>

            <TextField
              size="small"
              label="Filter"
              placeholder="tag:fast  !tag:flaky"
              value={filter}
              onChange={(e) => setFilter(e.target.value)}
              sx={{ minWidth: 240, "& input": { fontFamily: monoFontStack } }}
            />

            <FormControlLabel
              control={
                <Switch size="small" checked={dryRun} onChange={(e) => setDryRun(e.target.checked)} />
              }
              label="Dry run"
            />

            <Button
              variant="contained"
              startIcon={<PlayArrowIcon />}
              onClick={() => launch(env)}
              disabled={start.isPending}
            >
              Run
            </Button>
          </Stack>

          <Typography variant="caption" color="text.secondary">
            {target?.kind === "workflow"
              ? `Compiles ${target.name} across ${target.members.join(", ")} in dependency order.${
                  target.description ? ` ${target.description}.` : ""
                }`
              : "Space-separated filter terms narrow the graph: tag:, workspace:, kind:, owner:, step:, or a bare word. Prefix with ! to exclude."}
          </Typography>
        </Stack>
      )}

      {prompting && (
        <EnvPrompt
          // Sourcing an env file can surface a second, different set of
          // variables; keying on the names rebuilds the form for them instead
          // of leaving the first round's fields in place.
          key={prompting.join(",")}
          variables={prompting}
          initial={env}
          pending={start.isPending}
          onCancel={() => setPrompting(null)}
          onSubmit={(values) => launch({ ...env, ...values })}
        />
      )}

      {/* The missing-variable rejection is answered by the dialog, so showing
          it as an error too would just be noise. */}
      {start.error && !prompting && <ErrorNote error={start.error} />}
      {error && <ErrorNote error={error} />}

      <Stack direction="row" alignItems="baseline" spacing={2} sx={{ mb: 1.5 }}>
        <Typography variant="h3">Runs</Typography>
        <Box sx={{ flexGrow: 1 }} />
        <RetentionControl />
      </Stack>

      {isLoading ? (
        <Loading label="Loading runs…" />
      ) : !runs?.length ? (
        <Typography variant="body2" color="text.secondary">
          No runs yet.
        </Typography>
      ) : (
        <Stack spacing={1} sx={{ maxWidth: 900 }}>
          {runs.map((run) => (
            <RunRow key={run.id} run={run} />
          ))}
        </Stack>
      )}
    </>
  );
}

/**
 * One run in the list: how it went, what it ran, and a way to be rid of it.
 *
 * The status is an icon rather than a "finished" chip. Every run in a list of
 * fifty is finished; which of them *failed* is the only reason anyone is
 * reading the list, and that was the one thing it didn't say.
 */
function RunRow({ run }: { run: RunSummary }) {
  const remove = useDeleteRun();
  const [confirming, setConfirming] = useState(false);

  return (
    <Card>
      <CardContent sx={{ py: 1.5, "&:last-child": { pb: 1.5 } }}>
        <Stack direction="row" alignItems="center" spacing={2}>
          <StatusIcon status={run.status ?? (run.done ? "success" : "running")} size={20} />
          <Box sx={{ flexGrow: 1, minWidth: 0 }}>
            <RunLink to={`/run/${run.id}`}>{run.workflows.join(", ") || "—"}</RunLink>
            <Typography variant="caption" color="text.secondary" sx={{ display: "block" }}>
              #{run.id} · {statusLabel(run.status ?? (run.done ? "success" : "running"))} · started{" "}
              {new Date(run.created_at).toLocaleString()}
              {run.dry_run && " · dry run"}
              {(run.filter ?? []).length > 0 && ` · ${run.filter.join(" ")}`}
            </Typography>
          </Box>

          {confirming ? (
            <Stack direction="row" spacing={1} alignItems="center">
              <Typography variant="caption" color="text.secondary">
                Delete this run and its logs?
              </Typography>
              <Button size="small" onClick={() => setConfirming(false)}>
                Keep
              </Button>
              <Button
                size="small"
                color="error"
                variant="contained"
                disabled={remove.isPending}
                onClick={() => remove.mutate(run.id)}
              >
                Delete
              </Button>
            </Stack>
          ) : (
            <Tooltip
              title={
                run.done
                  ? "Delete this run and its logs"
                  : "Still running — stop it first, from the run's own page"
              }
            >
              {/* A disabled button swallows hover, so the tooltip hangs off a
                  live wrapper instead. */}
              <Box component="span">
                <IconButton
                  size="small"
                  aria-label={`Delete run ${run.id}`}
                  disabled={!run.done}
                  onClick={() => setConfirming(true)}
                >
                  <DeleteOutlineIcon fontSize="small" />
                </IconButton>
              </Box>
            </Tooltip>
          )}
        </Stack>
        {remove.error && <ErrorNote error={remove.error} />}
      </CardContent>
    </Card>
  );
}

/** The retention choices offered, in hours. Zero is "keep them". */
const RETENTIONS: { label: string; hours: number }[] = [
  { label: "1 day", hours: 24 },
  { label: "7 days", hours: 24 * 7 },
  { label: "30 days", hours: 24 * 30 },
  { label: "Forever", hours: 0 },
];

/**
 * How long finished runs are kept.
 *
 * Runs survive a daemon restart now, which is the point — but it also means the
 * history grows on its own, so there has to be somewhere to say how much of it
 * you want. Shortening it takes effect immediately rather than at the next
 * restart: a setting that quietly waits is a setting that looks broken.
 */
function RetentionControl() {
  const { data } = useRunSettings();
  const save = useSetRunSettings();
  if (!data) return null;

  return (
    <Tooltip title="Finished runs and their logs are kept on disk, so they survive restarting the daemon. This is how long before one is deleted automatically. Changing it applies right away.">
      <TextField
        select
        size="small"
        label="Keep run logs"
        value={data.ttl_hours}
        onChange={(event) => save.mutate(Number(event.target.value))}
        sx={{ minWidth: 150 }}
      >
        {RETENTIONS.map((option) => (
          <MenuItem key={option.hours} value={option.hours}>
            {option.label}
          </MenuItem>
        ))}
        {/* A TTL set from elsewhere (or an older default) still has to have a
            row, or the select shows blank. */}
        {!RETENTIONS.some((option) => option.hours === data.ttl_hours) && (
          <MenuItem value={data.ttl_hours}>{data.ttl_hours} hours</MenuItem>
        )}
      </TextField>
    </Tooltip>
  );
}
