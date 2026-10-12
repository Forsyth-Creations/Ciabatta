import { useState } from "react";
import {
  Alert,
  Box,
  Card,
  Dialog,
  IconButton,
  LinearProgress,
  Table,
  TableBody,
  TableCell,
  TableContainer,
  TableHead,
  TableRow,
  Typography,
  useTheme,
} from "@mui/material";
import CloseIcon from "@mui/icons-material/Close";
import { StepGraph, type GraphStep } from "@ciabatta/graphs";
import { BarChart } from "@mui/x-charts/BarChart";
import { PieChart } from "@mui/x-charts/PieChart";

import { api, type Build, type BuildSummary, type Step } from "../api";
import {
  ChartCard,
  EmptyRow,
  Mono,
  NoData,
  Prose,
  Section,
  Tag,
  When,
  useChartColors,
} from "../components/ui";
import { duration } from "../format";
import { monoFontStack } from "../theme";
import { useLoad } from "../useLoad";

/** How many of the latest builds the per-build charts show. */
const RECENT = 30;

export function Builds({ refreshKey }: { refreshKey: number }) {
  const { data, error, loading } = useLoad(
    () => api<{ builds: BuildSummary[] }>("/api/builds?limit=200"),
    refreshKey,
  );
  const [open, setOpen] = useState<string | null>(null);

  if (error)
    return (
      <Alert severity="error" sx={{ mt: 3 }}>
        {error}
      </Alert>
    );
  if (!data) return loading ? <LinearProgress sx={{ mt: 3 }} /> : null;
  const builds = data.builds ?? [];

  return (
    <Box>
      <Section
        title="Builds"
        sub="Every run reported by a checkout using this cache. The cache's job is the green: steps restored instead of run."
      >
        <Box
          sx={{
            display: "grid",
            gap: 2,
            gridTemplateColumns: { xs: "1fr", lg: "320px 1fr" },
          }}
        >
          <ChartCard title="Outcomes" sub={`All ${builds.length} reported builds`}>
            <Outcomes builds={builds} />
          </ChartCard>
          <ChartCard
            title="Steps reused per build"
            sub={`The latest ${Math.min(RECENT, builds.length)}, oldest on the left`}
          >
            <Reuse builds={builds} />
          </ChartCard>
        </Box>
      </Section>

      <Section title="History" sub="Newest first. Click one to open the flow it ran.">
        <TableContainer component={Card}>
          <Table size="small">
            <TableHead>
              <TableRow>
                <TableCell>Finished</TableCell>
                <TableCell>Project</TableCell>
                <TableCell>Workflow</TableCell>
                <TableCell>Outcome</TableCell>
                <TableCell>Branch · commit</TableCell>
                <TableCell>Who</TableCell>
                <TableCell align="right">Steps</TableCell>
                <TableCell align="right">Took</TableCell>
              </TableRow>
            </TableHead>
            <TableBody>
              {builds.length === 0 && (
                <EmptyRow colSpan={8}>
                  No builds reported yet. Any <Mono>ciabatta</Mono> run in a workspace whose{" "}
                  <Mono>cache.remote</Mono> points here is recorded when it finishes.
                </EmptyRow>
              )}
              {builds.map((b) => {
                const key = `${b.project}/${b.id}`;
                const ref = [b.branch, b.commit?.slice(0, 8)].filter(Boolean).join(" · ");
                const who = [b.user, b.host].filter(Boolean).join("@") || b.reported_by || "";
                return (
                  <TableRow
                    key={key}
                    hover
                    selected={open === key}
                    onClick={() => setOpen(open === key ? null : key)}
                    sx={{ cursor: "pointer" }}
                  >
                    <TableCell>
                      <When at={b.finished_at} />
                    </TableCell>
                    <TableCell>{b.project_name || b.project}</TableCell>
                    <TableCell>
                      <Mono>{b.workflow}</Mono>
                    </TableCell>
                    <TableCell>
                      <Tag kind={b.outcome} />
                    </TableCell>
                    <TableCell>
                      <Mono>{ref || "—"}</Mono>
                      {b.tag && <Tag kind="tag" label={b.tag} />}
                    </TableCell>
                    <TableCell>
                      {who}
                      {b.ci && (
                        <Typography component="span" variant="body2" color="text.secondary">
                          {" "}
                          · {b.ci}
                        </Typography>
                      )}
                    </TableCell>
                    <TableCell align="right">
                      {b.step_count}{" "}
                      <Typography component="span" variant="body2" color="text.secondary">
                        · {b.reused} reused
                      </Typography>
                    </TableCell>
                    <TableCell align="right">{duration(b.duration_ms)}</TableCell>
                  </TableRow>
                );
              })}
            </TableBody>
          </Table>
        </TableContainer>
      </Section>

      <BuildDialog id={open} onClose={() => setOpen(null)} />
    </Box>
  );
}

function Outcomes({ builds }: { builds: BuildSummary[] }) {
  const colors = useChartColors();
  const theme = useTheme();
  if (builds.length === 0) return <NoData height={220}>No builds yet.</NoData>;
  const tally = new Map<string, number>();
  builds.forEach((b) => tally.set(b.outcome, (tally.get(b.outcome) ?? 0) + 1));
  // Fixed order and fixed colours: an outcome keeps its colour whether or not
  // the others are present.
  const order = ["success", "failed", "stopped", "skipped"];
  const color = (o: string) =>
    o === "success" ? colors.success : o === "failed" ? colors.failed : colors.other;
  const data = [...tally.entries()]
    .sort((a, b) => {
      const ia = order.indexOf(a[0]);
      const ib = order.indexOf(b[0]);
      return (ia < 0 ? 99 : ia) - (ib < 0 ? 99 : ib);
    })
    .map(([outcome, value]) => ({ id: outcome, label: outcome, value, color: color(outcome) }));
  return (
    <PieChart
      height={220}
      margin={{ top: 8, bottom: 8, left: 8, right: 110 }}
      series={[
        {
          data,
          innerRadius: 52,
          outerRadius: 90,
          paddingAngle: 1.5,
          cornerRadius: 4,
          highlightScope: { fade: "global", highlight: "item" },
          valueFormatter: (item) =>
            `${item.value} (${((item.value * 100) / builds.length).toFixed(0)}%)`,
        },
      ]}
      sx={{ "& .MuiPieArc-root": { stroke: theme.palette.background.paper, strokeWidth: 2 } }}
      slotProps={{
        legend: {
          direction: "column",
          position: { vertical: "middle", horizontal: "right" },
          itemMarkWidth: 10,
          itemMarkHeight: 10,
          labelStyle: { fontSize: 13 },
        },
      }}
    />
  );
}

function Reuse({ builds }: { builds: BuildSummary[] }) {
  const colors = useChartColors();
  const theme = useTheme();
  const latest = builds.slice(0, RECENT).reverse();
  if (latest.length === 0) return <NoData height={220}>No builds yet.</NoData>;
  const label = (b: BuildSummary) =>
    `${b.project_name || b.project} · ${b.workflow} · ${new Date(b.finished_at).toLocaleString()}`;
  return (
    <BarChart
      height={220}
      borderRadius={4}
      grid={{ horizontal: true }}
      margin={{ top: 40, right: 12, bottom: 12, left: 40 }}
      sx={{
        "& .MuiChartsAxis-line, & .MuiChartsAxis-tick": { stroke: theme.palette.divider },
        "& .MuiChartsAxis-tickLabel": { fill: theme.palette.text.secondary },
        "& .MuiChartsGrid-line": { stroke: theme.palette.divider, strokeDasharray: "2 4" },
      }}
      xAxis={[
        {
          scaleType: "band" as const,
          data: latest.map((b) => `${b.project}/${b.id}`),
          valueFormatter: (key: string, ctx) => {
            if (ctx.location === "tick") return "";
            const b = latest.find((x) => `${x.project}/${x.id}` === key);
            return b ? label(b) : key;
          },
          disableTicks: true,
        },
      ]}
      yAxis={[{ tickMinStep: 1 }]}
      series={[
        {
          id: "reused",
          label: "Reused",
          stack: "steps",
          color: colors.hit,
          data: latest.map((b) => b.reused),
        },
        {
          id: "built",
          label: "Ran",
          stack: "steps",
          color: colors.miss,
          data: latest.map((b) => b.built + b.failed),
        },
      ]}
      slotProps={{
        legend: {
          direction: "row",
          position: { vertical: "top", horizontal: "right" },
          itemMarkWidth: 10,
          itemMarkHeight: 10,
          labelStyle: { fontSize: 13 },
        },
      }}
    />
  );
}

/** A build, opened over the list: its flow up top, its steps below. */
function BuildDialog({ id, onClose }: { id: string | null; onClose: () => void }) {
  return (
    <Dialog
      open={id !== null}
      onClose={onClose}
      maxWidth={false}
      PaperProps={{
        sx: {
          width: "90vw",
          height: "90vh",
          maxWidth: "none",
          maxHeight: "none",
          m: 0,
          display: "flex",
          flexDirection: "column",
        },
      }}
    >
      {id && <BuildDetail id={id} onClose={onClose} />}
    </Dialog>
  );
}

function BuildDetail({ id, onClose }: { id: string; onClose: () => void }) {
  const [project, build] = id.split("/");
  const { data, error } = useLoad(
    () =>
      api<{ build: Build; project_name?: string }>(
        `/api/projects/${encodeURIComponent(project)}/builds/${encodeURIComponent(build)}`,
      ),
    id,
  );
  const [picked, setPicked] = useState<string | null>(null);

  const header = (title: React.ReactNode, outcome?: string) => (
    <Box
      sx={{
        display: "flex",
        alignItems: "center",
        gap: 1.5,
        px: 2.5,
        py: 1.25,
        borderBottom: 1,
        borderColor: "divider",
      }}
    >
      <Typography variant="h2" noWrap sx={{ fontSize: "1.15rem", flex: 1 }}>
        {title}
      </Typography>
      {outcome && <Tag kind={outcome} />}
      <IconButton size="small" onClick={onClose} aria-label="Close">
        <CloseIcon fontSize="small" />
      </IconButton>
    </Box>
  );

  if (error) {
    return (
      <>
        {header("Build")}
        <Alert severity="error" sx={{ m: 2 }}>
          {error}
        </Alert>
      </>
    );
  }
  if (!data) {
    return (
      <>
        {header("Build")}
        <LinearProgress />
      </>
    );
  }

  const b = data.build;
  const stepsReused = b.steps.filter((s) => s.status === "reused").length;
  const facts: [string, React.ReactNode][] = [
    ["started", new Date(b.started_at).toLocaleString()],
    ["took", duration(b.duration_ms)],
    ["steps", `${b.steps.length} · ${stepsReused} reused`],
    ["branch", b.branch && <Mono>{b.branch}</Mono>],
    ["commit", b.commit && <Mono title={b.commit}>{b.commit.slice(0, 12)}</Mono>],
    ["tag", b.tag && <Mono>{b.tag}</Mono>],
    ["build", b.build_number],
    ["by", [b.user, b.host].filter(Boolean).join("@") || b.reported_by],
    ["ci", b.ci],
    ["ciabatta", b.version],
  ];

  return (
    <>
      {header(
        <>
          {data.project_name || project}
          <Box component="span" sx={{ color: "text.secondary", mx: 1 }}>
            ·
          </Box>
          <Mono>{b.workflow}</Mono>
        </>,
        b.outcome,
      )}
      <Box
        sx={{
          display: "flex",
          flexWrap: "wrap",
          columnGap: 3,
          rowGap: 0.5,
          px: 2.5,
          py: 1,
          borderBottom: 1,
          borderColor: "divider",
          bgcolor: "action.hover",
        }}
      >
        {facts
          .filter(([, v]) => v)
          .map(([k, v]) => (
            <Typography key={k} variant="body2">
              <Typography component="span" variant="body2" color="text.secondary">
                {k}
              </Typography>{" "}
              {v}
            </Typography>
          ))}
      </Box>
      {b.error && (
        <Alert severity="error" sx={{ mx: 2.5, mt: 1.5, fontFamily: monoFontStack }}>
          {b.error}
        </Alert>
      )}
      <Box sx={{ flex: "1 1 60%", minHeight: 240, p: 2, pb: 1 }}>
        {/* The same graph the daemon's run page draws, from @ciabatta/graphs.
            A recovery node that was never entered is a branch the run didn't
            take; it stays in the table, but drawing it would hang it off nothing. */}
        <StepGraph
          height="100%"
          steps={b.steps
            .filter((s) => !(s.kind === "recover" && s.status === "not run"))
            .map(graphStep)}
          onStepClick={(name) => {
            setPicked(name);
            document
              .getElementById(stepRowId(name))
              ?.scrollIntoView({ block: "nearest", behavior: "smooth" });
          }}
        />
      </Box>
      <TableContainer
        sx={{ flex: "1 1 40%", minHeight: 120, borderTop: 1, borderColor: "divider" }}
      >
        <Table size="small" stickyHeader>
          <TableHead>
            <TableRow>
              <TableCell>Step</TableCell>
              <TableCell>Status</TableCell>
              <TableCell>Cache</TableCell>
              <TableCell>Key</TableCell>
              <TableCell align="right">Took</TableCell>
              <TableCell>Note</TableCell>
            </TableRow>
          </TableHead>
          <TableBody>
            {b.steps.map((s) => (
              <TableRow key={s.name} id={stepRowId(s.name)} hover selected={picked === s.name}>
                <TableCell>
                  <Mono>{s.name}</Mono>{" "}
                  <Typography component="span" variant="caption" color="text.secondary">
                    {s.workspace}
                  </Typography>
                </TableCell>
                <TableCell>
                  <Tag kind={s.status.replace(" ", "-")} label={s.status} />
                </TableCell>
                <TableCell>
                  {s.cache}
                  {s.cache_source && (
                    <Typography component="span" variant="body2" color="text.secondary">
                      {" "}
                      · {s.cache_source}
                    </Typography>
                  )}
                </TableCell>
                <TableCell>
                  <Mono title={s.cache_key ?? ""}>{(s.cache_key ?? "").slice(0, 12)}</Mono>
                </TableCell>
                <TableCell align="right">{duration(s.duration_ms)}</TableCell>
                <TableCell sx={{ color: "text.secondary" }}>
                  <Prose text={s.note ?? ""} />
                </TableCell>
              </TableRow>
            ))}
          </TableBody>
        </Table>
      </TableContainer>
    </>
  );
}

const stepRowId = (name: string) => `step-row-${name}`;

/**
 * A reported step, as the shared graph wants it.
 *
 * The server's record calls a cache hit `reused` and a recovery branch nobody
 * entered `not run`; the graph's vocabulary is the daemon's, where a reused
 * step succeeded (and is marked as reused) and an unreached one is pending.
 */
function graphStep(s: Step): GraphStep {
  const reused = s.status === "reused";
  const status = reused ? "success" : s.status === "not run" ? "pending" : s.status;
  return {
    name: s.name,
    workspace: s.workspace,
    status,
    needs: s.needs,
    reused,
    recover: s.kind === "recover",
    detail: [reused ? "from cache" : s.status, duration(s.duration_ms)].filter(Boolean).join(" · "),
  };
}
