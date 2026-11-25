/**
 * One offloaded job, opened from the Executors page: its flowchart, a step
 * inspector and its log — the same way into a failure the daemon's run page
 * gives a local run.
 *
 * Click a node and the inspector says what that step is, what the cache made
 * of it, what it waits for and what waits for it, and lists the lines in its
 * log that look like the problem; the log narrows to that step. A failed job
 * opens on the step that failed. The URL names the job and the step
 * (`/executors?job=15&step=padua:build-checks:test`), so a failure can be
 * sent to someone as a link.
 *
 * Everything here comes from what the cache already holds for a job: its log,
 * and the build record the run sends as it goes. Each log line starts with the
 * step it came from (`[deploy]   [padua:build-checks:test] …`), which is what
 * lets the log be split by step in the browser.
 */
import { useEffect, useMemo, useRef, useState } from "react";
import ErrorOutlineIcon from "@mui/icons-material/ErrorOutline";
import SearchIcon from "@mui/icons-material/Search";
import {
  Alert,
  Box,
  Button,
  Chip,
  Dialog,
  DialogActions,
  DialogContent,
  DialogTitle,
  FormControlLabel,
  InputAdornment,
  Stack,
  Switch,
  TextField,
  ToggleButton,
  ToggleButtonGroup,
  Tooltip,
  Typography,
} from "@mui/material";

import { StatusIcon, StepGraph } from "@ciabatta/graphs";
import {
  api,
  isDone,
  word,
  type Build,
  type Job,
  type LogPage,
  type Step,
} from "../api";
import { Mono, Tag } from "../components/ui";
import { duration } from "../format";
import { monoFontStack } from "../theme";
import { graphStep } from "./Builds";

// ─── Deep links ─────────────────────────────────────────────────────────────

/** The job (and step) the address bar names, if any. */
export function linkedJob(): { job: number; step: string | null } | null {
  const params = new URLSearchParams(location.search);
  const job = Number(params.get("job"));
  if (!Number.isInteger(job) || job <= 0) return null;
  return { job, step: params.get("step") };
}

/** Point the address bar at a job and step, or at neither. */
function link(job: number | null, step: string | null) {
  const params = new URLSearchParams(location.search);
  params.delete("job");
  params.delete("step");
  if (job !== null) params.set("job", String(job));
  if (job !== null && step) params.set("step", step);
  const query = params.toString();
  const next = location.pathname + (query ? `?${query}` : "");
  if (next !== location.pathname + location.search) {
    history.replaceState(null, "", next);
  }
}

/**
 * The workflow file a step belongs to: its qualified name without the step
 * (`padua:build-checks:lint` → `padua:build-checks`). What folding groups by.
 */
function workflowOf(name: string): string | null {
  const parts = name.split(":");
  return parts.length > 1 ? parts.slice(0, -1).join(":") : null;
}

// Folding is a per-viewer preference, so it lives in this browser — and the
// page is fine without it (private windows, blocked storage).
const FOLD_KEY = "ciabatta-cache-fold-workflows";
function rememberedFold(): boolean {
  try {
    return localStorage.getItem(FOLD_KEY) !== "off";
  } catch {
    return true;
  }
}
function rememberFold(on: boolean) {
  try {
    localStorage.setItem(FOLD_KEY, on ? "on" : "off");
  } catch {
    // Not remembered; it still applies to this view.
  }
}

// ─── The log, split by step ─────────────────────────────────────────────────

interface Line {
  /** 1-based, across the whole job. */
  n: number;
  /** The step it came from, when the line names one. */
  step: string | null;
  /** The line with its `[workflow]   [step]` prefix taken off. */
  body: string;
  /** The line as the executor wrote it. */
  raw: string;
  problem: boolean;
}

// `[deploy]   [padua:build-checks:test] text`
const STEP_LINE = /^\[[^\]]+\]\s+\[([^\]\s]+)\]\s?(.*)$/;
// `[deploy]   ✗ step: padua:build-checks:test (52.4s · 18:14:33)` and the
// other status lines the runner writes about a step.
const STATUS_LINE =
  /^\[[^\]]+\]\s+(?:[▶✓✗⊘↓]\s+)?[a-z ]*step:?\s+([^\s(]+)(.*)$/;
// What a failure tends to print. Deliberately loose: it picks lines to look
// at first, it doesn't decide anything.
const PROBLEM =
  /(\bFAIL(ED)?\b|\bERR!|\berror(\[[A-Z0-9]+\])?:|\bError:|panicked at|Traceback|✗|×|\bexit (code|status) [1-9]|timed out)/;

function plain(line: string): string {
  // eslint-disable-next-line no-control-regex
  return line.replace(/\x1b\[[0-9;?]*[ -/]*[@-~]/g, "");
}

function parse(raw: string, n: number): Line {
  const own = STEP_LINE.exec(raw);
  if (own) {
    return {
      n,
      step: own[1],
      body: own[2],
      raw,
      problem: PROBLEM.test(own[2]),
    };
  }
  const status = STATUS_LINE.exec(raw);
  if (status) {
    const body = raw.replace(/^\[[^\]]+\]\s+/, "");
    return { n, step: status[1], body, raw, problem: /✗/.test(raw) };
  }
  return { n, step: null, body: raw, raw, problem: PROBLEM.test(raw) };
}

// ─── The dialog ─────────────────────────────────────────────────────────────

/** Steps a recovery branch the run didn't take would only clutter. */
function shown(steps: Step[]): Step[] {
  return steps.filter((s) => !(s.kind === "recover" && s.status === "not run"));
}

/** Where to start looking: the first step that failed, if one did. */
function firstFailure(builds: Build[]): string | null {
  for (const b of builds) {
    const failed = b.steps.find((s) => s.status === "failed");
    if (failed) return failed.name;
  }
  return null;
}

export function JobDialog({
  id,
  initialStep,
  onClose,
  onError,
}: {
  id: number;
  initialStep?: string | null;
  onClose: () => void;
  onError: (message: string) => void;
}) {
  const [job, setJob] = useState<Job | null>(null);
  const [raw, setRaw] = useState<string[]>([]);
  // The records the run reported, one per workflow: the flow it ran.
  const [builds, setBuilds] = useState<Build[]>([]);
  const [selected, setSelected] = useState<string | null>(initialStep ?? null);
  const [folded, setFolded] = useState(rememberedFold);
  // Whether a selection came from a person; until one does, a failure that
  // shows up picks itself.
  const chosen = useRef(Boolean(initialStep));
  const next = useRef(0);

  // Follows the log while the job runs; stops once it's over.
  useEffect(() => {
    let live = true;
    let timer: ReturnType<typeof setTimeout> | undefined;
    next.current = 0;
    setRaw([]);
    setBuilds([]);
    const tick = async () => {
      try {
        const current = await api<Job>(`/api/oven/jobs/${id}`);
        const page = await api<LogPage>(
          `/api/oven/jobs/${id}/log?from=${next.current}`,
        );
        if (!live) return;
        setJob(current);
        if (page.lines.length) {
          next.current = page.next;
          setRaw((old) => [...old, ...page.lines.map(plain)]);
        }
        // The run sends its record as it goes, so the flow is redrawn on
        // every tick while it runs, and once more when it's over.
        const found = await api<{ builds: Build[] }>(
          `/api/oven/jobs/${id}/builds`,
        );
        if (live) setBuilds(found.builds);
        if (!isDone(current.state)) timer = setTimeout(tick, 1500);
      } catch (e) {
        if (live) onError((e as Error).message);
      }
    };
    void tick();
    return () => {
      live = false;
      if (timer) clearTimeout(timer);
    };
  }, [id, onError]);

  // A failed job opens on what failed, unless someone has already picked.
  useEffect(() => {
    if (chosen.current) return;
    const failed = firstFailure(builds);
    if (failed) setSelected(failed);
  }, [builds]);

  useEffect(() => link(id, selected), [id, selected]);

  const select = (name: string | null) => {
    chosen.current = true;
    setSelected(name);
  };
  const close = () => {
    link(null, null);
    onClose();
  };

  const lines = useMemo(() => raw.map((l, i) => parse(l, i + 1)), [raw]);
  const allSteps = useMemo(() => builds.flatMap((b) => b.steps), [builds]);
  const step = allSteps.find((s) => s.name === selected) ?? null;
  const build = builds.find((b) => b.steps.some((s) => s.name === selected));
  const running = job !== null && !isDone(job.state);

  return (
    <Dialog
      open
      onClose={close}
      fullWidth
      maxWidth="xl"
      PaperProps={{ sx: { height: "92vh" } }}
    >
      <DialogTitle
        sx={{
          display: "flex",
          alignItems: "center",
          gap: 1.5,
          flexWrap: "wrap",
        }}
      >
        Job {id}
        {job && <Tag kind={word(job.state)} />}
        {job && (
          <Typography variant="body2" color="text.secondary">
            {job.spec?.workflows.join(" ")} at{" "}
            <Mono>{job.spec?.commit.slice(0, 12)}</Mono>
            {job.executor ? ` on ${job.executor}` : ""}
            {job.cancel_requested && running ? " · stopping…" : ""}
          </Typography>
        )}
      </DialogTitle>
      <DialogContent
        dividers
        sx={{
          display: "flex",
          flexDirection: "column",
          overflow: "hidden",
          gap: 1.5,
        }}
      >
        {job?.message && (
          <Alert
            severity={
              job.state === "JOB_STATE_SUCCEEDED" ? "success" : "warning"
            }
          >
            {job.message}
          </Alert>
        )}

        {builds.length > 0 && (
          <Stack
            direction="row"
            alignItems="center"
            spacing={1}
            sx={{ flexShrink: 0 }}
          >
            <FormControlLabel
              control={
                <Switch
                  size="small"
                  checked={folded}
                  onChange={(_, on) => {
                    setFolded(on);
                    rememberFold(on);
                  }}
                />
              }
              label={
                <Typography variant="body2">
                  Fold each workflow file into one node
                </Typography>
              }
            />
            <Typography variant="caption" color="text.secondary">
              The strip across a folded node picks the step it shows.
            </Typography>
          </Stack>
        )}
        {builds.map((b) => (
          <Box key={b.id} sx={{ flexShrink: 0 }}>
            {builds.length > 1 && (
              <Typography variant="body2" sx={{ fontWeight: 600, mb: 0.5 }}>
                <Mono>{b.workflow}</Mono>
              </Typography>
            )}
            <Box
              sx={{
                // Room for the inspector and log below on a short window.
                height:
                  builds.length > 1
                    ? "clamp(140px, 24vh, 220px)"
                    : "clamp(170px, 32vh, 300px)",
              }}
            >
              {/* The same graph the Builds page and the daemon's run page
                  draw, with the selected step outlined. */}
              <StepGraph
                height="100%"
                onStepClick={(name) => select(name === selected ? null : name)}
                focus={selected}
                fold={folded}
                steps={shown(b.steps).map((s) => ({
                  ...graphStep(s),
                  highlight: s.name === selected,
                  group: workflowOf(s.name),
                }))}
              />
            </Box>
          </Box>
        ))}
        {running && builds.length === 0 && (
          <Typography variant="body2" color="text.secondary">
            The flowchart appears once the run has started.
          </Typography>
        )}

        <Box
          sx={{
            display: "flex",
            gap: 1.5,
            flex: "1 1 0",
            minHeight: 200,
            flexDirection: { xs: "column", md: "row" },
          }}
        >
          <Box
            // A new step starts at the top of its own details.
            key={selected ?? ""}
            sx={{
              width: { xs: "100%", md: 380 },
              flexShrink: 0,
              overflow: "auto",
              border: 1,
              borderColor: "divider",
              borderRadius: 1,
              p: 1.5,
              maxHeight: { xs: 260, md: "none" },
            }}
          >
            {step && build ? (
              <StepInspector
                step={step}
                build={build}
                lines={lines}
                onSelect={select}
                onJump={(n) => jumpTo(n)}
              />
            ) : (
              <Typography variant="body2" color="text.secondary">
                Click a step to see what it is, what the cache made of it, what
                it waits for, and its own log.
                {firstFailure(builds) === null && builds.length > 0
                  ? " Nothing has failed."
                  : ""}
              </Typography>
            )}
          </Box>
          <LogPane
            lines={lines}
            step={selected}
            queued={job?.state === "JOB_STATE_QUEUED"}
            follow={running}
          />
        </Box>

        {job?.outputs && job.outputs.length > 0 && (
          <Typography variant="body2" color="text.secondary">
            Sent back {job.outputs.length} output file
            {job.outputs.length === 1 ? "" : "s"}:{" "}
            <Mono>{job.outputs.map((o) => o.path).join(", ")}</Mono>
          </Typography>
        )}
      </DialogContent>
      <DialogActions>
        {running && (
          <Button
            color="error"
            disabled={job?.cancel_requested}
            onClick={async () => {
              try {
                setJob(
                  await api<Job>(`/api/oven/jobs/${id}/cancel`, {
                    method: "POST",
                  }),
                );
              } catch (e) {
                onError((e as Error).message);
              }
            }}
          >
            Cancel job
          </Button>
        )}
        <Button onClick={close}>Close</Button>
      </DialogActions>
    </Dialog>
  );
}

/** Scroll the log to line `n` and flash it. */
function jumpTo(n: number) {
  // After the log has re-rendered for a newly selected step.
  requestAnimationFrame(() => {
    const row = document.querySelector<HTMLElement>(`[data-line="${n}"]`);
    if (!row) return;
    row.scrollIntoView({ block: "center" });
    row.animate(
      [
        { backgroundColor: "rgba(255, 193, 7, 0.45)" },
        { backgroundColor: "transparent" },
      ],
      { duration: 1600 },
    );
  });
}

// ─── The inspector ──────────────────────────────────────────────────────────

const CACHE_WORDS: Record<string, string> = {
  hit: "reused from the cache",
  fresh: "already up to date",
  rebuild: "ran — the cache had nothing for these inputs",
  uncached: "not cached",
};

function StepInspector({
  step,
  build,
  lines,
  onSelect,
  onJump,
}: {
  step: Step;
  build: Build;
  lines: Line[];
  onSelect: (name: string) => void;
  onJump: (n: number) => void;
}) {
  const steps = build.steps;
  const needs = step.needs.filter((n) => steps.some((s) => s.name === n));
  const neededBy = steps
    .filter((s) => s.needs.includes(step.name))
    .map((s) => s.name);
  const upstream = closure(steps, step.name);
  const own = lines.filter((l) => l.step === step.name);
  const problems = own.filter((l) => l.problem).slice(-30);
  const error =
    build.error && build.error.includes(step.name) ? build.error : null;
  // A skipped step's note is why; anyone else's is what the cache said.
  const cacheWord = step.cache ? (CACHE_WORDS[step.cache] ?? step.cache) : null;

  return (
    <Stack spacing={1.25}>
      <Stack
        direction="row"
        spacing={1}
        alignItems="center"
        sx={{ minWidth: 0 }}
      >
        <StatusIcon
          status={step.status === "reused" ? "success" : step.status}
        />
        <Typography
          variant="body2"
          sx={{
            fontFamily: monoFontStack,
            fontWeight: 600,
            minWidth: 0,
            wordBreak: "break-all",
          }}
        >
          {step.name}
        </Typography>
      </Stack>

      <Stack direction="row" spacing={0.75} flexWrap="wrap" useFlexGap>
        <Chip size="small" label={step.status} />
        {step.kind && (
          <Chip size="small" variant="outlined" label={step.kind} />
        )}
        {step.workspace && (
          <Chip
            size="small"
            variant="outlined"
            label={`workspace ${step.workspace}`}
          />
        )}
        {step.duration_ms !== null && (
          <Chip
            size="small"
            variant="outlined"
            label={duration(step.duration_ms)}
          />
        )}
        <Chip
          size="small"
          variant="outlined"
          label={`${own.length} log lines`}
        />
      </Stack>

      {error && (
        <Alert severity="error" icon={<ErrorOutlineIcon fontSize="small" />}>
          <Typography
            variant="body2"
            sx={{ fontFamily: monoFontStack, wordBreak: "break-word" }}
          >
            {error}
          </Typography>
        </Alert>
      )}

      {(cacheWord || step.note) && (
        <Field label="Cache">
          {cacheWord && (
            <Typography variant="body2">
              {cacheWord}
              {step.cache_source ? ` (${step.cache_source})` : ""}
            </Typography>
          )}
          {step.note && step.note !== cacheWord && (
            <Typography variant="body2" color="text.secondary">
              {step.note}
            </Typography>
          )}
          {step.cache_key && (
            <Tooltip title={step.cache_key}>
              <Typography
                variant="caption"
                sx={{ fontFamily: monoFontStack }}
                color="text.secondary"
              >
                key {step.cache_key.slice(0, 16)}
              </Typography>
            </Tooltip>
          )}
        </Field>
      )}

      <Field
        label={
          needs.length === 0
            ? "Waits for nothing"
            : `Waits for ${needs.length}` +
              (upstream.size > needs.length ? ` (${upstream.size} in all)` : "")
        }
      >
        <StepChips names={needs} steps={steps} onSelect={onSelect} />
      </Field>
      {neededBy.length > 0 && (
        <Field label={`Needed by ${neededBy.length}`}>
          <StepChips names={neededBy} steps={steps} onSelect={onSelect} />
        </Field>
      )}

      {problems.length > 0 && (
        <Field label={`Lines that look like the problem (${problems.length})`}>
          <Stack spacing={0.25}>
            {problems.map((l) => (
              <Box
                key={l.n}
                onClick={() => onJump(l.n)}
                title="Show in the log"
                sx={{
                  fontFamily: monoFontStack,
                  fontSize: "0.75rem",
                  color: "error.main",
                  cursor: "pointer",
                  wordBreak: "break-word",
                  borderRadius: 0.5,
                  px: 0.5,
                  "&:hover": { bgcolor: "action.hover" },
                }}
              >
                <Box component="span" sx={{ color: "text.disabled", mr: 1 }}>
                  {l.n}
                </Box>
                {l.body}
              </Box>
            ))}
          </Stack>
        </Field>
      )}
    </Stack>
  );
}

/** Every step `name` waits for, however indirectly. */
function closure(steps: Step[], name: string): Set<string> {
  const byName = new Map(steps.map((s) => [s.name, s]));
  const seen = new Set<string>();
  const stack = [...(byName.get(name)?.needs ?? [])];
  while (stack.length) {
    const n = stack.pop()!;
    if (seen.has(n) || !byName.has(n)) continue;
    seen.add(n);
    stack.push(...byName.get(n)!.needs);
  }
  return seen;
}

function Field({
  label,
  children,
}: {
  label: string;
  children?: React.ReactNode;
}) {
  return (
    <Box>
      <Typography
        variant="caption"
        color="text.secondary"
        sx={{ display: "block", mb: 0.25 }}
      >
        {label}
      </Typography>
      {children}
    </Box>
  );
}

function StepChips({
  names,
  steps,
  onSelect,
}: {
  names: string[];
  steps: Step[];
  onSelect: (name: string) => void;
}) {
  if (names.length === 0) return null;
  return (
    <Stack direction="row" spacing={0.5} flexWrap="wrap" useFlexGap>
      {names.map((n) => {
        const s = steps.find((x) => x.name === n);
        return (
          <Chip
            key={n}
            size="small"
            variant="outlined"
            onClick={() => onSelect(n)}
            icon={
              s ? (
                <Box sx={{ display: "flex", pl: 0.5 }}>
                  <StatusIcon
                    status={s.status === "reused" ? "success" : s.status}
                  />
                </Box>
              ) : undefined
            }
            label={n.split(":").pop()}
            title={n}
            sx={{ fontFamily: monoFontStack }}
          />
        );
      })}
    </Stack>
  );
}

// ─── The log pane ───────────────────────────────────────────────────────────

/** More than this many lines and only the newest are drawn, with a way back. */
const WINDOW = 4000;

function LogPane({
  lines,
  step,
  queued,
  follow,
}: {
  lines: Line[];
  step: string | null;
  queued: boolean;
  follow: boolean;
}) {
  const [scope, setScope] = useState<"step" | "job">("step");
  const [query, setQuery] = useState("");
  const [all, setAll] = useState(false);
  const box = useRef<HTMLDivElement | null>(null);
  // Following the tail until someone scrolls up to read.
  const pinned = useRef(true);

  const narrowed = step !== null && scope === "step";
  const filtered = useMemo(() => {
    let out = narrowed ? lines.filter((l) => l.step === step) : lines;
    const q = query.trim().toLowerCase();
    if (q) out = out.filter((l) => l.raw.toLowerCase().includes(q));
    return out;
  }, [lines, narrowed, step, query]);
  const hidden = all ? 0 : Math.max(0, filtered.length - WINDOW);
  const visible = hidden ? filtered.slice(hidden) : filtered;

  // A new step starts at the top of its own log, unless the job is still
  // being followed — then the tail is what's interesting.
  useEffect(() => {
    pinned.current = follow;
    const el = box.current;
    if (el) el.scrollTop = follow ? el.scrollHeight : 0;
  }, [step, scope, follow]);
  useEffect(() => {
    const el = box.current;
    if (el && pinned.current) el.scrollTop = el.scrollHeight;
  }, [visible.length]);

  return (
    <Box
      sx={{
        flex: "1 1 0",
        minWidth: 0,
        display: "flex",
        flexDirection: "column",
        gap: 1,
      }}
    >
      <Stack
        direction="row"
        spacing={1}
        alignItems="center"
        flexWrap="wrap"
        useFlexGap
      >
        <ToggleButtonGroup
          size="small"
          exclusive
          value={narrowed ? "step" : "job"}
          onChange={(_, v) => v && setScope(v)}
        >
          <ToggleButton value="step" disabled={step === null}>
            This step
          </ToggleButton>
          <ToggleButton value="job">Whole job</ToggleButton>
        </ToggleButtonGroup>
        <TextField
          size="small"
          placeholder="Find in log"
          value={query}
          onChange={(e) => setQuery(e.target.value)}
          sx={{ flex: "1 1 200px", maxWidth: 360 }}
          InputProps={{
            startAdornment: (
              <InputAdornment position="start">
                <SearchIcon fontSize="small" />
              </InputAdornment>
            ),
          }}
        />
        <Typography variant="caption" color="text.secondary">
          {filtered.length} of {lines.length} lines
          {narrowed ? ` · ${step!.split(":").pop()}` : ""}
        </Typography>
      </Stack>
      <Box
        ref={box}
        onScroll={(e) => {
          const el = e.currentTarget;
          pinned.current =
            el.scrollHeight - el.scrollTop - el.clientHeight < 40;
        }}
        sx={{
          fontFamily: monoFontStack,
          fontSize: "0.78rem",
          bgcolor: "action.hover",
          borderRadius: 1,
          py: 1,
          flex: "1 1 0",
          minHeight: 120,
          overflow: "auto",
        }}
      >
        {hidden > 0 && (
          <Button
            size="small"
            sx={{ ml: 1, mb: 0.5 }}
            onClick={() => setAll(true)}
          >
            Show {hidden} earlier lines
          </Button>
        )}
        {visible.length === 0 ? (
          <Typography variant="body2" color="text.secondary" sx={{ px: 1.5 }}>
            {lines.length === 0
              ? queued
                ? "Waiting for an executor…"
                : "No output yet."
              : query
                ? "No line matches."
                : "This step hasn't written anything."}
          </Typography>
        ) : (
          visible.map((l) => (
            <Box
              key={l.n}
              data-line={l.n}
              sx={{
                display: "flex",
                px: 1,
                color: l.problem ? "error.main" : undefined,
                bgcolor: l.problem ? "rgba(244, 67, 54, 0.07)" : undefined,
              }}
            >
              <Box
                component="span"
                sx={{
                  color: "text.disabled",
                  width: "5ch",
                  flexShrink: 0,
                  textAlign: "right",
                  mr: 1.5,
                  userSelect: "none",
                }}
              >
                {l.n}
              </Box>
              <Box
                component="span"
                sx={{
                  whiteSpace: "pre-wrap",
                  wordBreak: "break-word",
                  minWidth: 0,
                }}
              >
                {narrowed ? l.body : l.raw}
              </Box>
            </Box>
          ))
        )}
      </Box>
    </Box>
  );
}
