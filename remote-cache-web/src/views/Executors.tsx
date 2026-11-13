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
  Tooltip,
  Typography,
} from "@mui/material";

import {
  api,
  isDone,
  word,
  type Discovered,
  type ExecutorList,
  type ExecutorView,
  type Job,
  type JobList,
  type LogPage,
  type Paired,
  type PairWith,
} from "../api";
import { EmptyRow, Mono, Prose, Section, Tag, When } from "../components/ui";
import { ago } from "../format";
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
  const [removing, setRemoving] = useState<{ id: string; name: string } | null>(null);
  const [viewing, setViewing] = useState<number | null>(null);
  const [actionError, setActionError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);

  const pair = async (address: string, code: string) => {
    try {
      // The address the executor should use for this cache: the one this
      // page was loaded from, which a machine on the same network can reach.
      const request: PairWith = { address, code, server: location.origin };
      const result = await api<Paired>("/api/oven/executors", {
        method: "POST",
        body: JSON.stringify(request),
      });
      setNotice(`Paired with ${result.executor?.name ?? address}. It takes jobs from now on.`);
      setActionError(null);
      setPairing(null);
      setManualAddress("");
      setManualCode("");
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
                <TableRow key={e.id} hover>
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
                      onClick={() => setRemoving({ id: e.id, name: e.name })}
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
                      onClick={() => setPairing({ address: d.address, name: d.name, taken: true })}
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
                void pair(manualAddress, manualCode);
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
        onPair={(code) => pairing && void pair(pairing.address, code)}
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
              <Link component="button" variant="body2" onClick={() => onJob(e.job!)}>
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

function PairDialog({
  target,
  onClose,
  onPair,
}: {
  target: PairTarget | null;
  onClose: () => void;
  onPair: (code: string) => void;
}) {
  const [code, setCode] = useState("");
  useEffect(() => setCode(""), [target]);
  return (
    <Dialog open={target !== null} onClose={onClose}>
      <Box
        component="form"
        onSubmit={(event) => {
          event.preventDefault();
          onPair(code);
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
