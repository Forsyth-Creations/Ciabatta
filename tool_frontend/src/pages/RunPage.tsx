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
  Dialog,
  DialogActions,
  DialogContent,
  DialogContentText,
  DialogTitle,
  FormControlLabel,
  MenuItem,
  Stack,
  Switch,
  TextField,
  Typography,
} from "@mui/material";
import AccountTreeIcon from "@mui/icons-material/AccountTree";
import PlayArrowIcon from "@mui/icons-material/PlayArrow";
import { styled } from "@mui/material/styles";
import { Link, useNavigate } from "@tanstack/react-router";

import { missingEnvFrom, useRunTargets, useRuns, useStartRun } from "../api/run";
import { EnvDriftBanner } from "../components/EnvDriftBanner";
import { RunStatusChip, StopButton } from "../components/RunControls";
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
        description="Execute a recipe's step DAG live, with fix-it branches when a step fails. The daemon owns the run, so it survives closing the terminal."
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
    // A workflow compiles a cross-package graph on the daemon; a recipe runs
    // this project's own steps. Same endpoint, same flags — the launcher just
    // has to say which kind of name it picked.
    const terms = filter
      .split(/\s+/)
      .map((term) => term.trim())
      .filter(Boolean);
    const body =
      target?.kind === "workflow"
        ? { project, recipes: [], workflow: selected, filter: terms, dry_run: dryRun, env: withEnv }
        : {
            project,
            recipes: selected ? [selected] : [],
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
            {/* Workflows and recipes in one list: they are the same kind of
                thing, and making someone know which they have before they can
                start it is the distinction this tool exists to remove. */}
            <TextField
              select
              size="small"
              label="Workflow or recipe"
              value={selected}
              onChange={(e) => setSelected(e.target.value)}
              sx={{ minWidth: 260 }}
            >
              <MenuItem value="">
                <em>All run-capable recipes</em>
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
                          : "recipe"
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

      <Typography variant="h3" sx={{ mb: 1.5 }}>
        Runs
      </Typography>

      {isLoading ? (
        <Loading label="Loading runs…" />
      ) : !runs?.length ? (
        <Typography variant="body2" color="text.secondary">
          No runs yet.
        </Typography>
      ) : (
        <Stack spacing={1} sx={{ maxWidth: 900 }}>
          {runs.map((run) => (
            <Card key={run.id}>
              <CardContent sx={{ py: 1.5, "&:last-child": { pb: 1.5 } }}>
                <Stack direction="row" alignItems="center" spacing={2}>
                  <RunStatusChip run={run} />
                  <Box sx={{ flexGrow: 1, minWidth: 0 }}>
                    <RunLink to={`/run/${run.id}`}>{run.recipes.join(", ") || "—"}</RunLink>
                    <Typography variant="caption" color="text.secondary" sx={{ display: "block" }}>
                      #{run.id} · started {new Date(run.created_at).toLocaleTimeString()}
                    </Typography>
                  </Box>
                  {!run.done && <StopButton run={run} />}
                </Stack>
              </CardContent>
            </Card>
          ))}
        </Stack>
      )}
    </>
  );
}

/**
 * Ask for the variables a run can't start without.
 *
 * The daemon already looked in its own environment and in whatever `.env` files
 * the recipe sources, so anything listed here genuinely has nowhere else to come
 * from. Values are used for this launch only — nothing is written to disk.
 */
function EnvPrompt({
  variables,
  initial,
  pending,
  onCancel,
  onSubmit,
}: {
  variables: string[];
  initial: Record<string, string>;
  pending: boolean;
  onCancel: () => void;
  onSubmit: (values: Record<string, string>) => void;
}) {
  const [values, setValues] = useState<Record<string, string>>(() =>
    Object.fromEntries(variables.map((name) => [name, initial[name] ?? ""])),
  );

  // Blank is the state the daemon already rejected, so requiring a value here
  // saves a round trip that could only come back with the same question.
  const complete = variables.every((name) => values[name]?.trim());

  return (
    <Dialog open fullWidth maxWidth="sm" onClose={onCancel}>
      <DialogTitle>This run needs a few variables</DialogTitle>
      <DialogContent>
        <DialogContentText sx={{ mb: 2 }}>
          {variables.length === 1
            ? "One variable the run requires isn't set. Give it a value to continue."
            : `${variables.length} variables the run requires aren't set. Give them values to continue.`}
        </DialogContentText>
        <Stack spacing={2}>
          {variables.map((name) => (
            <TextField
              key={name}
              label={name}
              value={values[name] ?? ""}
              onChange={(e) => setValues((prev) => ({ ...prev, [name]: e.target.value }))}
              size="small"
              fullWidth
              autoFocus={name === variables[0]}
              slotProps={{ input: { sx: { fontFamily: monoFontStack } } }}
            />
          ))}
        </Stack>
      </DialogContent>
      <DialogActions>
        <Button onClick={onCancel}>Cancel</Button>
        <Button
          variant="contained"
          disabled={!complete || pending}
          onClick={() => onSubmit(values)}
        >
          Run
        </Button>
      </DialogActions>
    </Dialog>
  );
}
