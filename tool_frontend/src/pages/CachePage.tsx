/**
 * The build cache: what it holds, what it would reuse, and why it wouldn't.
 *
 * Three things, in the order somebody needs them:
 *
 * 1. **Would my next build reuse anything?** The plan, stage by stage, with the
 *    input and output files each stage is judged on and — for a stage that
 *    would rebuild — the diff that explains it.
 * 2. **Is the shared cache working?** Hit rate, storage, retention, and which
 *    ciabatta build it hands out.
 * 3. **What is configured?** The inputs and outputs this workspace declared,
 *    because a cache that never hits is nearly always a cache whose inputs are
 *    wrong.
 */

import { useState } from "react";
import {
  Alert,
  Box,
  Button,
  Card,
  CardContent,
  Chip,
  Divider,
  LinearProgress,
  MenuItem,
  Paper,
  Select,
  Stack,
  Tab,
  Tabs,
  Tooltip,
  Typography,
} from "@mui/material";
import CheckCircleIcon from "@mui/icons-material/CheckCircle";
import CloudOffIcon from "@mui/icons-material/CloudOff";
import CloudQueueIcon from "@mui/icons-material/CloudQueue";
import OpenInNewIcon from "@mui/icons-material/OpenInNew";
import ReplayIcon from "@mui/icons-material/Replay";
import BuildIcon from "@mui/icons-material/Build";
import RemoveCircleOutlineIcon from "@mui/icons-material/RemoveCircleOutline";

import { humanizeBytes, humanizeMs, useCachePlan, useCacheStatus, useRemoteCache } from "../api/cache";
import { useWorkspace } from "../api/workspace";
import type {
  CachePlan,
  Decision,
  PlannedStep,
  RebuildReason,
  Release,
  RemoteStatus,
} from "../api/types";
import { CacheDiffView } from "../components/CacheDiff";
import { ErrorNote, Loading, PageHeader, RequireProject } from "../components/Page";

export function CachePage() {
  return <RequireProject>{(projectId) => <Cache projectId={projectId} />}</RequireProject>;
}

function Cache({ projectId }: { projectId: string }) {
  const [tab, setTab] = useState(0);

  return (
    <>
      <PageHeader
        title="Cache"
        description="What your next build would reuse, and — when it wouldn't — exactly which files, variables, or upstream stages changed."
      />

      <Tabs value={tab} onChange={(_, next) => setTab(next)} sx={{ mb: 3 }}>
        <Tab label="Plan" />
        <Tab label="Remote" />
        <Tab label="Configuration" />
      </Tabs>

      {tab === 0 && <PlanTab projectId={projectId} />}
      {tab === 1 && <RemoteTab projectId={projectId} />}
      {tab === 2 && <ConfigTab projectId={projectId} />}
    </>
  );
}

// ─── Plan ────────────────────────────────────────────────────────────────────

function PlanTab({ projectId }: { projectId: string }) {
  const workspace = useWorkspace(projectId);
  const [target, setTarget] = useState<string>("");
  const { data: plan, isLoading, error } = useCachePlan(projectId, target || null);

  const workflows = workspace.data?.workflows ?? [];

  return (
    <>
      <Stack direction="row" spacing={2} alignItems="center" sx={{ mb: 3 }}>
        <Select
          size="small"
          displayEmpty
          value={target}
          onChange={(event) => setTarget(event.target.value)}
          sx={{ minWidth: 220 }}
        >
          <MenuItem value="">Every runnable recipe</MenuItem>
          {workflows.map((name) => (
            <MenuItem key={name} value={name}>
              {name}
            </MenuItem>
          ))}
        </Select>
        <Typography variant="body2" color="text.secondary">
          This is the same answer <code>ciabatta dry-run</code> gives — nothing is run.
        </Typography>
      </Stack>

      {error && <ErrorNote error={error} />}
      {isLoading && <Loading label="Working out what would be reused…" />}
      {plan && <PlanSummary plan={plan} />}
      {plan && (
        <Stack spacing={2} sx={{ mt: 3 }}>
          {plan.steps.map((step) => (
            <StageCard key={step.name} step={step} />
          ))}
        </Stack>
      )}
    </>
  );
}

function PlanSummary({ plan }: { plan: CachePlan }) {
  if (!plan.caching) {
    return (
      <Alert severity="info">
        Caching is off for every stage here, so all {plan.steps.length} would run. Turn it
        on with <code>ciabatta cache init</code> — it proposes the inputs and outputs from
        what's actually in the directory.
      </Alert>
    );
  }

  const total = plan.reused + plan.rebuilt;
  const percent = total > 0 ? (plan.reused / total) * 100 : 0;

  return (
    <Paper variant="outlined" sx={{ p: 2 }}>
      <Stack direction="row" spacing={3} alignItems="center" flexWrap="wrap" useFlexGap>
        <Stat label="Reused" value={String(plan.reused)} />
        <Stat label="Would run" value={String(plan.rebuilt)} />
        {plan.saved_ms > 0 && (
          <Stat
            label="Build time saved"
            value={humanizeMs(plan.saved_ms)}
            hint="Measured from what those stages cost the last time they actually ran."
          />
        )}
      </Stack>
      <LinearProgress
        variant="determinate"
        value={percent}
        color="success"
        sx={{ mt: 2, height: 6, borderRadius: 3 }}
      />
    </Paper>
  );
}

function Stat({ label, value, hint }: { label: string; value: string; hint?: string }) {
  const body = (
    <Box>
      <Typography variant="h2" sx={{ fontSize: 24, lineHeight: 1.2 }}>
        {value}
      </Typography>
      <Typography variant="caption" color="text.secondary">
        {label}
      </Typography>
    </Box>
  );
  return hint ? <Tooltip title={hint}>{body}</Tooltip> : body;
}

/**
 * One stage: what it reads, what it writes, and what it would do.
 *
 * The files come first because they're the contract the stage is being judged
 * against — the spec's point exactly: show the inputs before the graph and the
 * outputs after it, so the caching decision is legible rather than magic.
 */
function StageCard({ step }: { step: PlannedStep }) {
  const { icon, color, label } = describe(step.decision);
  const inputBytes = step.inputs.reduce((total, file) => total + file.size, 0);
  const outputBytes = step.outputs.reduce((total, file) => total + file.size, 0);

  return (
    <Card variant="outlined">
      <CardContent>
        <Stack direction="row" spacing={1.5} alignItems="center" sx={{ mb: 1 }}>
          <Box sx={{ color, display: "flex" }}>{icon}</Box>
          <Typography variant="subtitle1" sx={{ fontFamily: "monospace", flexGrow: 1 }}>
            {step.name}
          </Typography>
          <Chip size="small" variant="outlined" label={label} />
        </Stack>

        {step.needs.length > 0 && (
          <Typography variant="caption" color="text.secondary" sx={{ display: "block", mb: 1.5 }}>
            needs {step.needs.join(", ")}
          </Typography>
        )}

        <Stack direction={{ xs: "column", sm: "row" }} spacing={2} sx={{ mb: 1 }}>
          <FileList
            title="Inputs"
            subtitle={`${step.inputs.length} file(s), ${humanizeBytes(inputBytes)} — a change to any of these means a rebuild`}
            files={step.inputs.map((file) => file.path)}
          />
          <FileList
            title="Outputs"
            subtitle={`${step.outputs.length} file(s), ${humanizeBytes(outputBytes)} — restored on a hit, and verified before one is granted`}
            files={step.outputs.map((file) => file.path)}
          />
        </Stack>

        {step.diff && (
          <>
            <Divider sx={{ my: 2 }} />
            <CacheDiffView diff={step.diff} />
          </>
        )}
      </CardContent>
    </Card>
  );
}

/** A capped list of paths — enough to recognize the set, not enough to bury the page. */
function FileList({
  title,
  subtitle,
  files,
}: {
  title: string;
  subtitle: string;
  files: string[];
}) {
  const shown = files.slice(0, 6);

  return (
    <Box sx={{ flex: 1, minWidth: 0 }}>
      <Typography variant="subtitle2">{title}</Typography>
      <Typography variant="caption" color="text.secondary" sx={{ display: "block", mb: 0.5 }}>
        {subtitle}
      </Typography>
      {files.length === 0 ? (
        <Typography variant="body2" color="text.disabled">
          none
        </Typography>
      ) : (
        <Box component="ul" sx={{ m: 0, pl: 2 }}>
          {shown.map((path) => (
            <Typography
              component="li"
              key={path}
              variant="body2"
              sx={{
                fontFamily: "monospace",
                fontSize: 12,
                overflow: "hidden",
                textOverflow: "ellipsis",
                whiteSpace: "nowrap",
              }}
            >
              {path}
            </Typography>
          ))}
          {files.length > shown.length && (
            <Typography component="li" variant="body2" color="text.secondary" sx={{ fontSize: 12 }}>
              … and {files.length - shown.length} more
            </Typography>
          )}
        </Box>
      )}
    </Box>
  );
}

/** How to show a decision: icon, colour, and the sentence explaining it. */
function describe(decision: Decision): {
  icon: React.ReactNode;
  color: string;
  label: string;
} {
  switch (decision.outcome) {
    case "fresh":
      return {
        icon: <CheckCircleIcon fontSize="small" />,
        color: "success.main",
        label: `up to date · ${decision.outputs} output(s) already correct`,
      };
    case "hit":
      return {
        icon: <ReplayIcon fontSize="small" />,
        color: "success.main",
        label: `restore from ${decision.source === "remote" ? "the remote cache" : "the local cache"}`,
      };
    case "rebuild":
      return {
        icon: <BuildIcon fontSize="small" />,
        color: "warning.main",
        label: rebuildLabel(decision.reason),
      };
    case "uncached":
      return {
        icon: <RemoveCircleOutlineIcon fontSize="small" />,
        color: "text.disabled",
        label: decision.reason,
      };
  }
}

function rebuildLabel(reason: RebuildReason): string {
  switch (reason.kind) {
    case "never_built":
      return "never built with these inputs";
    case "inputs_changed":
      return `${reason.total} input file(s) changed`;
    case "outputs_missing":
      return "expected outputs are missing";
    case "outputs_modified":
      return "outputs were modified since they were built";
    case "no_outputs":
      return "no outputs declared, so there's nothing to restore";
  }
}

// ─── Remote ──────────────────────────────────────────────────────────────────

function RemoteTab({ projectId }: { projectId: string }) {
  const { data, isLoading, error } = useRemoteCache(projectId);

  if (isLoading) return <Loading label="Asking the remote cache…" />;
  if (error) return <ErrorNote error={error} />;
  if (!data) return null;

  return <RemoteStatusView status={data} />;
}

function RemoteStatusView({ status }: { status: RemoteStatus }) {
  if (!status.configured) {
    return (
      <Alert severity="info" icon={<CloudOffIcon />}>
        This workspace isn't pointed at a remote cache. Stand one up with{" "}
        <code>ciabatta remote-cache init</code>, then connect this workspace with{" "}
        <code>ciabatta cache init --remote &lt;URL&gt;</code>.
      </Alert>
    );
  }

  if (!status.reachable) {
    return (
      <Alert severity="warning" icon={<CloudOffIcon />}>
        <Typography variant="body2">
          {status.url} isn't answering. Builds carry on using the local cache.
        </Typography>
        <Typography variant="caption" sx={{ display: "block", mt: 1 }}>
          {status.error}
        </Typography>
      </Alert>
    );
  }

  const { stats } = status;
  const hitRate = stats.hit_rate;

  return (
    <Stack spacing={3}>
      <Paper variant="outlined" sx={{ p: 2 }}>
        <Stack direction="row" spacing={1} alignItems="center" sx={{ mb: 2 }}>
          <CloudQueueIcon fontSize="small" color="success" />
          <Typography variant="subtitle1" sx={{ fontFamily: "monospace" }}>
            {status.url}
          </Typography>
          {status.read_only && <Chip size="small" label="read-only" />}
          <Box sx={{ flexGrow: 1 }} />
          {/* The server's own admin page — where credentials are minted. This
              app can't do it: it talks to the cache with the daemon's saved
              session, and user management belongs to whoever administers the
              cache, not to everyone who can open this page. */}
          <Button
            size="small"
            endIcon={<OpenInNewIcon fontSize="small" />}
            href={status.url}
            target="_blank"
            rel="noreferrer"
          >
            Manage users
          </Button>
        </Stack>

        <Stack direction="row" spacing={4} flexWrap="wrap" useFlexGap>
          <Stat
            label="Hit rate"
            value={hitRate === null ? "—" : `${hitRate.toFixed(1)}%`}
            hint="A rate near zero usually means the keys aren't stable — an undeclared input, or something like a timestamp baked into a build — rather than that nothing is reusable."
          />
          <Stat label="Hits" value={String(stats.counters.hits)} />
          <Stat label="Misses" value={String(stats.counters.misses)} />
          <Stat label="Stored" value={stats.storage.human} />
          <Stat label="Entries" value={String(stats.storage.entries)} />
          <Stat label="Served" value={humanizeBytes(stats.counters.bytes_served)} />
          <Stat label="Sessions" value={String(stats.sessions)} />
        </Stack>

        <Typography variant="caption" color="text.secondary" sx={{ display: "block", mt: 2 }}>
          Retention: {stats.retention.description}. Counters are since the server started
          ({new Date(stats.started_at).toLocaleString()}).
        </Typography>
      </Paper>

      <ReleaseCard release={stats.release} />

      {stats.projects.length > 0 && (
        <Box>
          <Typography variant="subtitle2" sx={{ mb: 1 }}>
            Projects on this cache
          </Typography>
          <Stack spacing={1}>
            {stats.projects.map((entry) => (
              <Paper key={entry.project.id} variant="outlined" sx={{ p: 1.5 }}>
                <Stack direction="row" spacing={2} alignItems="center" flexWrap="wrap" useFlexGap>
                  <Typography variant="body2" sx={{ fontWeight: 600, minWidth: 140 }}>
                    {entry.project.name}
                  </Typography>
                  <Typography
                    variant="caption"
                    color="text.secondary"
                    sx={{ fontFamily: "monospace" }}
                  >
                    {entry.project.id}
                  </Typography>
                  <Box sx={{ flexGrow: 1 }} />
                  <Typography variant="body2" color="text.secondary">
                    {entry.counters.hits} hit · {entry.counters.misses} miss
                    {entry.hit_rate !== null && ` · ${entry.hit_rate.toFixed(0)}%`}
                  </Typography>
                </Stack>
              </Paper>
            ))}
          </Stack>
        </Box>
      )}
    </Stack>
  );
}

/**
 * The ciabatta build this cache hands out.
 *
 * Shown here because the cache is the thing everybody's builds already talk to,
 * which makes it the natural place to notice the team has drifted onto four
 * different versions.
 */
function ReleaseCard({ release }: { release: Release }) {
  const platforms = Object.keys(release.builds);

  return (
    <Paper variant="outlined" sx={{ p: 2 }}>
      <Typography variant="subtitle2">Ciabatta {release.version}</Typography>
      {platforms.length === 0 ? (
        <Typography variant="body2" color="text.secondary" sx={{ mt: 0.5 }}>
          This cache doesn't hand out binaries. Point <code>releases.binaries</code> at them in
          the server's config to let everyone update with <code>ciabatta self update</code>.
        </Typography>
      ) : (
        <>
          <Typography variant="caption" color="text.secondary" sx={{ display: "block", mb: 1 }}>
            Update with <code>ciabatta self update</code>. The download is checked against these
            hashes before anything is replaced.
          </Typography>
          <Stack spacing={0.5}>
            {platforms.map((platform) => (
              <Stack key={platform} direction="row" spacing={2} alignItems="center">
                <Chip size="small" label={platform} sx={{ minWidth: 80 }} />
                <Typography variant="caption" color="text.secondary">
                  {humanizeBytes(release.builds[platform].size)}
                </Typography>
                <Typography
                  variant="caption"
                  color="text.disabled"
                  sx={{
                    fontFamily: "monospace",
                    overflow: "hidden",
                    textOverflow: "ellipsis",
                    whiteSpace: "nowrap",
                  }}
                >
                  {release.builds[platform].sha256.slice(0, 16)}…
                </Typography>
              </Stack>
            ))}
          </Stack>
          {release.notes && (
            <Typography variant="body2" sx={{ mt: 1.5 }}>
              {release.notes}
            </Typography>
          )}
        </>
      )}
    </Paper>
  );
}

// ─── Configuration ───────────────────────────────────────────────────────────

function ConfigTab({ projectId }: { projectId: string }) {
  const { data, isLoading, error } = useCacheStatus(projectId);

  if (isLoading) return <Loading label="Reading the cache configuration…" />;
  if (error) return <ErrorNote error={error} />;
  if (!data) return null;

  return (
    <Stack spacing={3}>
      {!data.enabled && (
        <Alert severity="info">
          {data.why_disabled ?? "Caching is off for this workspace."} Run{" "}
          <code>ciabatta cache init</code> — it looks at what's actually in the directory and
          proposes the inputs and outputs for you.
        </Alert>
      )}

      <Paper variant="outlined" sx={{ p: 2 }}>
        <Typography variant="subtitle2" sx={{ mb: 1 }}>
          Local store
        </Typography>
        <Stack direction="row" spacing={4} flexWrap="wrap" useFlexGap>
          <Stat label="Entries" value={String(data.entries)} />
          <Stat label="Size" value={data.human} />
          {data.build_time_ms > 0 && (
            <Stat label="Build time stored" value={humanizeMs(data.build_time_ms)} />
          )}
        </Stack>
        <Typography
          variant="caption"
          color="text.secondary"
          sx={{ display: "block", mt: 1.5, fontFamily: "monospace" }}
        >
          {data.path}
        </Typography>
      </Paper>

      <Stack direction={{ xs: "column", md: "row" }} spacing={2}>
        <PatternList
          title="Inputs"
          subtitle="A build that reads a file not listed here will be handed a stale result when that file changes. This is the part that has to be right."
          patterns={data.inputs}
        />
        <PatternList
          title="Outputs"
          subtitle="Stored on a build, restored on a hit, and verified against their recorded hashes before a hit is granted."
          patterns={data.outputs}
        />
      </Stack>

      {(data.exclude.length > 0 || data.env.length > 0) && (
        <Stack direction={{ xs: "column", md: "row" }} spacing={2}>
          {data.exclude.length > 0 && (
            <PatternList
              title="Excluded"
              subtitle="Never counted as inputs, so a build can't invalidate itself with its own output."
              patterns={data.exclude}
            />
          )}
          {data.env.length > 0 && (
            <PatternList
              title="Environment"
              subtitle="Variables this build's result depends on; changing one is a rebuild."
              patterns={data.env}
            />
          )}
        </Stack>
      )}
    </Stack>
  );
}

function PatternList({
  title,
  subtitle,
  patterns,
}: {
  title: string;
  subtitle: string;
  patterns: string[];
}) {
  return (
    <Paper variant="outlined" sx={{ p: 2, flex: 1 }}>
      <Typography variant="subtitle2">{title}</Typography>
      <Typography variant="caption" color="text.secondary" sx={{ display: "block", mb: 1 }}>
        {subtitle}
      </Typography>
      {patterns.length === 0 ? (
        <Typography variant="body2" color="text.disabled">
          none declared
        </Typography>
      ) : (
        <Stack spacing={0.5}>
          {patterns.map((pattern) => (
            <Typography key={pattern} variant="body2" sx={{ fontFamily: "monospace", fontSize: 13 }}>
              {pattern}
            </Typography>
          ))}
        </Stack>
      )}
    </Paper>
  );
}
