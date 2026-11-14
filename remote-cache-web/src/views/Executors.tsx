import { useEffect, useRef, useState, type ReactNode } from "react";
import AutorenewIcon from "@mui/icons-material/Autorenew";
import CheckCircleIcon from "@mui/icons-material/CheckCircleOutline";
import CloudOffIcon from "@mui/icons-material/CloudOffOutlined";
import LinkIcon from "@mui/icons-material/Link";
import WifiTetheringIcon from "@mui/icons-material/WifiTethering";
import {
  Alert,
  Box,
  Button,
  Card,
  Chip,
  CardContent,
  Dialog,
  DialogActions,
  DialogContent,
  DialogContentText,
  DialogTitle,
  LinearProgress,
  Link,
  Table,
  TableBody,
  TableCell,
  TableContainer,
  TableHead,
  TableRow,
  TextField,
  ToggleButton,
  ToggleButtonGroup,
  Tooltip,
  Typography,
} from "@mui/material";

import {
  api,
  isDone,
  word,
  type Discovered,
  type ExecutorHistory,
  type ExecutorList,
  type ExecutorView,
  type Job,
  type JobList,
  type LogPage,
  type Paired,
  type PairWith,
} from "../api";
import { EmptyRow, Mono, Prose, Section, Tag, When } from "../components/ui";
import { ago, duration } from "../format";
import { monoFontStack } from "../theme";
import { useLoad } from "../useLoad";

/**
 * Executors, and the jobs offloaded to them.
 *
 * Unlike the rest of the page this one refreshes itself, every few seconds and
 * only while the tab is in view: an executor's state and a running job's log
 * are the point of looking, and both go stale in seconds.
 */
export function Executors({ refreshKey }: { refreshKey: number }) {
  const [tick, setTick] = useState(0);
  useEffect(() => {
    const timer = setInterval(() => {
      if (document.visibilityState === "visible") setTick((t) => t + 1);
    }, 3000);
    return () => clearInterval(timer);
  }, []);

  const executors = useLoad(
    () => api<ExecutorList>("/api/oven/executors"),
    `${refreshKey}:${tick}`,
  );
  const jobs = useLoad(() => api<JobList>("/api/oven/jobs?limit=50"), `${refreshKey}:${tick}`);

  const [pairing, setPairing] = useState<PairTarget | null>(null);
  const [manualAddress, setManualAddress] = useState("");
  const [manualCode, setManualCode] = useState("");
  const [manualName, setManualName] = useState("");
  const [removing, setRemoving] = useState<{ id: string; name: string } | null>(null);
  const [viewing, setViewing] = useState<number | null>(null);
  const [inspecting, setInspecting] = useState<string | null>(null);
  const [actionError, setActionError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);

  const pair = async (address: string, code: string, name: string) => {
    try {
      // The address the executor should use for this cache: the one this
      // page was loaded from, which a machine on the same network can reach.
      // Left out when blank: the cache keeps its name for it, or uses the one it
      // gave itself.
      const request: PairWith = { address, code, server: location.origin };
      if (name.trim()) request.name = name.trim();
      const result = await api<Paired>("/api/oven/executors", {
        method: "POST",
        body: JSON.stringify(request),
      });
      setNotice(`Paired with ${result.executor?.name ?? address}. It takes jobs from now on.`);
      setActionError(null);
      setPairing(null);
      setManualAddress("");
      setManualCode("");
      setManualName("");
      executors.reload();
    } catch (e) {
      setActionError((e as Error).message);
    }
  };

  if (executors.error)
    return (
      <Alert severity="error" sx={{ mt: 3 }}>
        {executors.error}
      </Alert>
    );
  if (!executors.data) return executors.loading ? <LinearProgress sx={{ mt: 3 }} /> : null;

  // The ones paired here are in the table already.
  const nearby = executors.data.discovered.filter(
    (d) => d.availability !== "AVAILABILITY_PAIRED_HERE",
  );

  return (
    <>
      <Section
        title="Executors"
        sub={
          <Prose text="Machines that run workflows offloaded with `--offload`. Start one with `ciabatta oven executor`: it prints an address and a pairing code, and shows up below when it's on this network." />
        }
      >
        {actionError && (
          <Alert severity="error" sx={{ mb: 2 }} onClose={() => setActionError(null)}>
            {actionError}
          </Alert>
        )}
        {notice && (
          <Alert severity="success" sx={{ mb: 2 }} onClose={() => setNotice(null)}>
            {notice}
          </Alert>
        )}

        <TableContainer component={Card} sx={{ mb: 2 }}>
          <Table size="small">
            <TableHead>
              <TableRow>
                <TableCell>Name</TableCell>
                <TableCell>Status</TableCell>
                <TableCell>Platform</TableCell>
                <TableCell>Added</TableCell>
                <TableCell />
              </TableRow>
            </TableHead>
            <TableBody>
              {executors.data.executors.length === 0 && (
                <EmptyRow colSpan={5}>No executors are paired yet.</EmptyRow>
              )}
              {executors.data.executors.map((e) => (
                <TableRow
                  key={e.id}
                  hover
                  sx={{ cursor: "pointer" }}
                  onClick={() => setInspecting(e.id)}
                >
                  <TableCell>
                    <Mono title={e.address}>{e.name}</Mono>
                  </TableCell>
                  <TableCell>
                    <ExecutorStatus executor={e} onJob={setViewing} />
                  </TableCell>
                  <TableCell sx={{ color: "text.secondary" }}>
                    {e.platform} · {e.version}
                  </TableCell>
                  <TableCell sx={{ color: "text.secondary" }}>
                    <When at={e.added_at} />
                    {e.added_by && ` by ${e.added_by}`}
                  </TableCell>
                  <TableCell align="right">
                    <Button
                      size="small"
                      color="error"
                      onClick={(event) => {
                        event.stopPropagation();
                        setRemoving({ id: e.id, name: e.name });
                      }}
                    >
                      Remove
                    </Button>
                  </TableCell>
                </TableRow>
              ))}
            </TableBody>
          </Table>
        </TableContainer>

        {nearby.length > 0 && (
          <Card sx={{ mb: 2 }}>
            <CardContent>
              <Typography sx={{ fontWeight: 600, mb: 1 }}>On this network</Typography>
              {nearby.map((d: Discovered) => (
                <Box
                  key={d.id}
                  sx={{
                    display: "flex",
                    alignItems: "center",
                    gap: 2,
                    flexWrap: "wrap",
                    py: 0.5,
                  }}
                >
                  <Availability found={d} />
                  <Mono>{d.name}</Mono>
                  <Typography variant="body2" color="text.secondary">
                    {d.address} · {[d.platform, d.version].filter(Boolean).join(" · ")}
                  </Typography>
                  <Box sx={{ flex: 1 }} />
                  {taken(d) ? (
                    <Button
                      size="small"
                      variant="outlined"
                      color="warning"
                      onClick={() =>
                        setPairing({
                          address: d.address,
                          name: d.name,
                          taken: true,
                        })
                      }
                    >
                      Move here
                    </Button>
                  ) : (
                    <Button
                      size="small"
                      variant="contained"
                      onClick={() => setPairing({ address: d.address, name: d.name })}
                    >
                      Add
                    </Button>
                  )}
                </Box>
              ))}
            </CardContent>
          </Card>
        )}

        <Card>
          <CardContent>
            <Typography sx={{ fontWeight: 600, mb: 1 }}>Add manually</Typography>
            <Box
              component="form"
              sx={{
                display: "flex",
                flexWrap: "wrap",
                gap: 2,
                alignItems: "center",
              }}
              onSubmit={(event) => {
                event.preventDefault();
                void pair(manualAddress, manualCode, manualName);
              }}
            >
              <TextField
                size="small"
                label="Address"
                placeholder="192.168.1.20:8381"
                required
                value={manualAddress}
                onChange={(e) => setManualAddress(e.target.value)}
              />
              <TextField
                size="small"
                label="Pairing code"
                placeholder="123-456"
                required
                value={manualCode}
                onChange={(e) => setManualCode(e.target.value)}
              />
              <TextField
                size="small"
                label="Name (optional)"
                placeholder="what it calls itself"
                inputProps={{ maxLength: 64 }}
                value={manualName}
                onChange={(e) => setManualName(e.target.value)}
              />
              <Button type="submit" variant="contained">
                Pair
              </Button>
            </Box>
          </CardContent>
        </Card>
      </Section>

      <Section title="Jobs" sub="What has been offloaded, newest first.">
        <TableContainer component={Card}>
          <Table size="small">
            <TableHead>
              <TableRow>
                <TableCell>Job</TableCell>
                <TableCell>State</TableCell>
                <TableCell>Workflow</TableCell>
                <TableCell>Commit</TableCell>
                <TableCell>Executor</TableCell>
                <TableCell>By</TableCell>
                <TableCell>Submitted</TableCell>
              </TableRow>
            </TableHead>
            <TableBody>
              {(jobs.data?.jobs.length ?? 0) === 0 && (
                <EmptyRow colSpan={7}>Nothing has been offloaded yet.</EmptyRow>
              )}
              {jobs.data?.jobs.map((j) => (
                <TableRow
                  key={j.id}
                  hover
                  sx={{ cursor: "pointer" }}
                  onClick={() => setViewing(j.id)}
                >
                  <TableCell>
                    <Mono>{j.id}</Mono>
                  </TableCell>
                  <TableCell>
                    <Tag kind={word(j.state)} />
                  </TableCell>
                  <TableCell>{j.spec?.workflows.join(" ")}</TableCell>
                  <TableCell>
                    <Mono title={j.spec?.repo}>
                      {j.spec?.branch ? `${j.spec?.branch} · ` : ""}
                      {j.spec?.commit.slice(0, 10)}
                    </Mono>
                  </TableCell>
                  <TableCell>{j.executor ?? j.spec?.executor ?? "any"}</TableCell>
                  <TableCell sx={{ color: "text.secondary" }}>{j.requested_by}</TableCell>
                  <TableCell sx={{ color: "text.secondary" }}>
                    <When at={j.created_at} />
                  </TableCell>
                </TableRow>
              ))}
            </TableBody>
          </Table>
        </TableContainer>
      </Section>

      <PairDialog
        target={pairing}
        onClose={() => setPairing(null)}
        onPair={(code, name) => pairing && void pair(pairing.address, code, name)}
      />

      <Dialog open={removing !== null} onClose={() => setRemoving(null)}>
        <DialogTitle>Remove {removing?.name}?</DialogTitle>
        <DialogContent>
          <DialogContentText>
            Its token stops working at once. It can be paired again with a new code.
          </DialogContentText>
        </DialogContent>
        <DialogActions>
          <Button onClick={() => setRemoving(null)}>Cancel</Button>
          <Button
            color="error"
            variant="contained"
            onClick={async () => {
              const target = removing!;
              setRemoving(null);
              try {
                await api(`/api/oven/executors/${encodeURIComponent(target.id)}`, {
                  method: "DELETE",
                });
                setActionError(null);
                executors.reload();
              } catch (e) {
                setActionError((e as Error).message);
              }
            }}
          >
            Remove
          </Button>
        </DialogActions>
      </Dialog>

      {inspecting !== null && (
        <ExecutorDialog
          executor={executors.data.executors.find((e) => e.id === inspecting) ?? null}
          tick={tick}
          onClose={() => setInspecting(null)}
          onJob={setViewing}
        />
      )}

      {viewing !== null && (
        <JobDialog id={viewing} onClose={() => setViewing(null)} onError={setActionError} />
      )}
    </>
  );
}

interface PairTarget {
  address: string;
  name?: string;
  /** Paired with another cache, which loses it if it's paired here. */
  taken?: boolean;
}

function taken(found: Discovered): boolean {
  return found.availability === "AVAILABILITY_PAIRED_ELSEWHERE";
}

/** How a nearby executor stands: free to add, or someone else's. */
function Availability({ found }: { found: Discovered }) {
  return taken(found) ? (
    <Tooltip title="It's paired with another cache. Pairing it here moves it, and that cache stops reaching it.">
      <Chip
        size="small"
        variant="outlined"
        color="warning"
        icon={<LinkIcon />}
        label="paired with another cache"
        sx={{ height: 22 }}
      />
    </Tooltip>
  ) : (
    <Tooltip title="Not paired with any cache. Add it with the code it printed.">
      <Chip
        size="small"
        variant="outlined"
        color="success"
        icon={<WifiTetheringIcon />}
        label="available"
        sx={{ height: 22 }}
      />
    </Tooltip>
  );
}

const SPIN = {
  "@keyframes ciabatta-spin": { to: { transform: "rotate(360deg)" } },
  animation: "ciabatta-spin 2s linear infinite",
};

/**
 * Whether a paired executor is there and what it's doing: an icon and a word
 * to read at a glance, and a line under them saying why.
 */
function ExecutorStatus({
  executor: e,
  onJob,
}: {
  executor: ExecutorView;
  onJob: (job: number) => void;
}) {
  const icon = { fontSize: 20 };
  let head: ReactNode;
  let note: ReactNode;
  switch (e.state) {
    case "EXECUTOR_STATE_BUSY":
      head = (
        <>
          <AutorenewIcon color="warning" sx={{ ...icon, ...SPIN }} />
          <Typography component="span" sx={{ fontWeight: 600 }}>
            {e.job ? (
              <Link
                component="button"
                variant="body2"
                onClick={(event) => {
                  event.stopPropagation();
                  onJob(e.job!);
                }}
              >
                Running job {e.job}
              </Link>
            ) : (
              "Running a job"
            )}
          </Typography>
        </>
      );
      note = <>checked in {ago(e.last_seen)}</>;
      break;
    case "EXECUTOR_STATE_IDLE":
      head = (
        <>
          <CheckCircleIcon color="success" sx={icon} />
          <Typography component="span" sx={{ fontWeight: 600 }}>
            Ready
          </Typography>
        </>
      );
      note = <>waiting for jobs · checked in {ago(e.last_seen)}</>;
      break;
    default:
      head = (
        <>
          <CloudOffIcon color="disabled" sx={icon} />
          <Typography component="span" sx={{ fontWeight: 600 }} color="text.secondary">
            Offline
          </Typography>
        </>
      );
      // The cache only knows what it has heard since it started.
      note = e.last_seen ? (
        <>
          last heard from <When at={e.last_seen} /> — is it still running?
        </>
      ) : (
        <>hasn't checked in since this cache started</>
      );
  }
  return (
    <Box>
      <Box sx={{ display: "flex", alignItems: "center", gap: 0.75 }}>{head}</Box>
      <Typography variant="caption" color="text.secondary">
        {note}
      </Typography>
    </Box>
  );
}

const WINDOWS = {
  "24h": 24 * 3600 * 1000,
  "7d": 7 * 24 * 3600 * 1000,
  "30d": 30 * 24 * 3600 * 1000,
} as const;
type Range = keyof typeof WINDOWS;

/** A stretch of time an executor was up, down, or not yet watched. */
interface Span {
  from: number;
  to: number;
  online: boolean | null;
}

/** The changes cut into spans covering `[start, now]`. */
function spans(history: ExecutorHistory, start: number, now: number): Span[] {
  const changes = history.changes.map((c) => ({
    at: Date.parse(c.at),
    online: c.online,
  }));
  const out: Span[] = [];
  let at = start;
  let online: boolean | null = null;
  for (const c of changes) {
    if (c.at <= start) {
      online = c.online;
      continue;
    }
    if (c.at > now) break;
    out.push({ from: at, to: c.at, online });
    at = c.at;
    online = c.online;
  }
  out.push({ from: at, to: now, online });
  return out.filter((s) => s.to > s.from);
}

/**
 * One executor's comings and goings, as the cache saw them: how much of the
 * chosen window it was reachable, a strip of up and down, and the changes.
 */
function ExecutorDialog({
  executor,
  tick,
  onClose,
  onJob,
}: {
  executor: ExecutorView | null;
  tick: number;
  onClose: () => void;
  onJob: (job: number) => void;
}) {
  const [range, setRange] = useState<Range>("24h");
  const id = executor?.id;
  const history = useLoad(
    () =>
      id
        ? api<ExecutorHistory>(`/api/oven/executors/${encodeURIComponent(id)}/history`)
        : Promise.resolve(null),
    `${id}:${tick}`,
  );

  // Removed while open.
  if (!executor) return null;

  const now = Date.now();
  const start = now - WINDOWS[range];
  const parts = history.data ? spans(history.data, start, now) : [];
  const total = (online: boolean | null) =>
    parts.filter((p) => p.online === online).reduce((sum, p) => sum + (p.to - p.from), 0);
  const up = total(true);
  const down = total(false);
  const watched = up + down;
  const recent = [...(history.data?.changes ?? [])].reverse().slice(0, 20);

  return (
    <Dialog open onClose={onClose} fullWidth maxWidth="md">
      <DialogTitle
        sx={{
          display: "flex",
          alignItems: "center",
          gap: 1.5,
          flexWrap: "wrap",
        }}
      >
        <Mono>{executor.name}</Mono>
        <Typography variant="body2" color="text.secondary">
          {executor.address} · {executor.platform} · {executor.version}
        </Typography>
      </DialogTitle>
      <DialogContent dividers>
        <Box sx={{ mb: 2 }}>
          <ExecutorStatus executor={executor} onJob={onJob} />
        </Box>
        {history.error && (
          <Alert severity="error" sx={{ mb: 2 }}>
            {history.error}
          </Alert>
        )}
        {!history.data && history.loading && <LinearProgress />}
        {history.data && (
          <>
            <Box
              sx={{
                display: "flex",
                alignItems: "center",
                gap: 2,
                mb: 1,
                flexWrap: "wrap",
              }}
            >
              <Typography sx={{ fontWeight: 600 }}>
                {watched > 0
                  ? `${((100 * up) / watched).toFixed(watched === up ? 0 : 1)}% up`
                  : "No record yet"}
              </Typography>
              {watched > 0 && (
                <Typography variant="body2" color="text.secondary">
                  up {duration(up)} · down {duration(down)}
                  {watched < WINDOWS[range] - 1000 &&
                    ` · not watched for ${duration(WINDOWS[range] - watched)}`}
                </Typography>
              )}
              <Box sx={{ flex: 1 }} />
              <ToggleButtonGroup
                size="small"
                exclusive
                value={range}
                onChange={(_, value: Range | null) => value && setRange(value)}
              >
                {(Object.keys(WINDOWS) as Range[]).map((w) => (
                  <ToggleButton key={w} value={w} sx={{ py: 0.25, px: 1.25 }}>
                    {w}
                  </ToggleButton>
                ))}
              </ToggleButtonGroup>
            </Box>

            <Box
              sx={{
                display: "flex",
                height: 22,
                borderRadius: 1,
                overflow: "hidden",
                bgcolor: "action.hover",
              }}
            >
              {parts.map((p) => (
                <Tooltip
                  key={p.from}
                  title={`${p.online === null ? "Not watched" : p.online ? "Online" : "Offline"} · ${new Date(p.from).toLocaleString()} → ${p.to >= now ? "now" : new Date(p.to).toLocaleString()} (${duration(p.to - p.from)})`}
                >
                  <Box
                    sx={{
                      flex: `${p.to - p.from} 0 0`,
                      minWidth: 2,
                      bgcolor:
                        p.online === null
                          ? "transparent"
                          : p.online
                            ? "success.main"
                            : "error.main",
                      opacity: p.online === false ? 0.8 : 1,
                    }}
                  />
                </Tooltip>
              ))}
            </Box>
            <Box sx={{ display: "flex", justifyContent: "space-between", mt: 0.5 }}>
              <Typography variant="caption" color="text.secondary">
                {range} ago
              </Typography>
              <Typography variant="caption" color="text.secondary">
                now
              </Typography>
            </Box>

            <Typography sx={{ fontWeight: 600, mt: 3, mb: 1 }}>Recent changes</Typography>
            {recent.length === 0 ? (
              <Typography variant="body2" color="text.secondary">
                Nothing yet: the cache records a change when the executor checks in or goes quiet
                for a minute.
              </Typography>
            ) : (
              <Table size="small">
                <TableBody>
                  {recent.map((c, i) => {
                    // The next change (newer) ends this one.
                    const until = i === 0 ? now : Date.parse(recent[i - 1].at);
                    return (
                      <TableRow key={c.at + c.online}>
                        <TableCell sx={{ width: 140 }}>
                          {c.online ? (
                            <Box
                              sx={{
                                display: "flex",
                                alignItems: "center",
                                gap: 0.75,
                              }}
                            >
                              <CheckCircleIcon color="success" sx={{ fontSize: 18 }} /> Came online
                            </Box>
                          ) : (
                            <Box
                              sx={{
                                display: "flex",
                                alignItems: "center",
                                gap: 0.75,
                              }}
                            >
                              <CloudOffIcon color="error" sx={{ fontSize: 18 }} /> Went offline
                            </Box>
                          )}
                        </TableCell>
                        <TableCell sx={{ color: "text.secondary" }}>
                          <When at={c.at} />
                        </TableCell>
                        <TableCell sx={{ color: "text.secondary" }}>
                          {i === 0 ? "for " : "lasted "}
                          {duration(until - Date.parse(c.at))}
                        </TableCell>
                      </TableRow>
                    );
                  })}
                </TableBody>
              </Table>
            )}
          </>
        )}
      </DialogContent>
      <DialogActions>
        <Button onClick={onClose}>Close</Button>
      </DialogActions>
    </Dialog>
  );
}

function PairDialog({
  target,
  onClose,
  onPair,
}: {
  target: PairTarget | null;
  onClose: () => void;
  onPair: (code: string, name: string) => void;
}) {
  const [code, setCode] = useState("");
  const [name, setName] = useState("");
  useEffect(() => {
    setCode("");
    setName(target?.name ?? "");
  }, [target]);
  return (
    <Dialog open={target !== null} onClose={onClose}>
      <Box
        component="form"
        onSubmit={(event) => {
          event.preventDefault();
          onPair(code, name);
        }}
      >
        <DialogTitle>Pair with {target?.name ?? target?.address}</DialogTitle>
        <DialogContent>
          {target?.taken && (
            <Alert severity="warning" sx={{ mb: 2 }}>
              This executor is paired with another cache. Pairing it here moves it: that cache stops
              reaching it, and any job it's running for that cache is lost.
            </Alert>
          )}
          <DialogContentText sx={{ mb: 2 }}>
            Enter the pairing code the executor printed when it started.
          </DialogContentText>
          <TextField
            autoFocus
            fullWidth
            size="small"
            label="Pairing code"
            placeholder="123-456"
            value={code}
            onChange={(e) => setCode(e.target.value)}
          />
          <TextField
            fullWidth
            size="small"
            label="Name"
            helperText="What it's called on this cache, and what --offload-to takes."
            inputProps={{ maxLength: 64 }}
            sx={{ mt: 2 }}
            value={name}
            onChange={(e) => setName(e.target.value)}
          />
        </DialogContent>
        <DialogActions>
          <Button onClick={onClose}>Cancel</Button>
          <Button
            type="submit"
            variant="contained"
            color={target?.taken ? "warning" : "primary"}
            disabled={!code.trim()}
          >
            {target?.taken ? "Move here" : "Pair"}
          </Button>
        </DialogActions>
      </Box>
    </Dialog>
  );
}

/** Escape codes out: the log carries the terminal's colours. */
function plain(line: string): string {
  // eslint-disable-next-line no-control-regex
  return line.replace(/\x1b\[[0-9;?]*[ -/]*[@-~]/g, "");
}

function JobDialog({
  id,
  onClose,
  onError,
}: {
  id: number;
  onClose: () => void;
  onError: (message: string) => void;
}) {
  const [job, setJob] = useState<Job | null>(null);
  const [lines, setLines] = useState<string[]>([]);
  const next = useRef(0);
  const bottom = useRef<HTMLDivElement | null>(null);

  // Follows the log while the job runs; stops once it's over.
  useEffect(() => {
    let live = true;
    let timer: ReturnType<typeof setTimeout> | undefined;
    next.current = 0;
    setLines([]);
    const step = async () => {
      try {
        const current = await api<Job>(`/api/oven/jobs/${id}`);
        const page = await api<LogPage>(`/api/oven/jobs/${id}/log?from=${next.current}`);
        if (!live) return;
        setJob(current);
        if (page.lines.length) {
          next.current = page.next;
          setLines((old) => [...old, ...page.lines.map(plain)]);
        }
        const over = isDone(current.state);
        if (!over) timer = setTimeout(step, 1500);
      } catch (e) {
        if (live) onError((e as Error).message);
      }
    };
    void step();
    return () => {
      live = false;
      if (timer) clearTimeout(timer);
    };
  }, [id, onError]);

  useEffect(() => bottom.current?.scrollIntoView({ block: "end" }), [lines.length]);

  const running = job !== null && !isDone(job.state);

  return (
    <Dialog open onClose={onClose} fullWidth maxWidth="lg">
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
            {job.spec?.workflows.join(" ")} at <Mono>{job.spec?.commit.slice(0, 12)}</Mono>
            {job.executor ? ` on ${job.executor}` : ""}
            {job.cancel_requested && running ? " · stopping…" : ""}
          </Typography>
        )}
      </DialogTitle>
      <DialogContent dividers>
        {job?.message && (
          <Alert
            severity={job.state === "JOB_STATE_SUCCEEDED" ? "success" : "warning"}
            sx={{ mb: 1.5 }}
          >
            {job.message}
          </Alert>
        )}
        <Box
          sx={{
            fontFamily: monoFontStack,
            fontSize: "0.8rem",
            whiteSpace: "pre-wrap",
            wordBreak: "break-word",
            bgcolor: "action.hover",
            borderRadius: 1,
            p: 1.5,
            maxHeight: "60vh",
            overflow: "auto",
          }}
        >
          {lines.length === 0 ? (
            <Typography variant="body2" color="text.secondary">
              {job?.state === "JOB_STATE_QUEUED" ? "Waiting for an executor…" : "No output yet."}
            </Typography>
          ) : (
            lines.join("\n")
          )}
          <div ref={bottom} />
        </Box>
        {job?.outputs && job.outputs.length > 0 && (
          <Typography variant="body2" color="text.secondary" sx={{ mt: 1.5 }}>
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
        <Button onClick={onClose}>Close</Button>
      </DialogActions>
    </Dialog>
  );
}
