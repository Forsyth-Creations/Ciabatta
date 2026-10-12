import { useState } from "react";
import {
  Alert,
  Box,
  Card,
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

  if (error) return <Alert severity="error" sx={{ mt: 3 }}>{error}</Alert>;
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

      <Section title="History" sub="Newest first. Click one for the flow it ran.">
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

      {open && <BuildDetail id={open} />}
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

function BuildDetail({ id }: { id: string }) {
  const [project, build] = id.split("/");
  const { data, error } = useLoad(
    () =>
      api<{ build: Build; project_name?: string }>(
        `/api/projects/${encodeURIComponent(project)}/builds/${encodeURIComponent(build)}`,
      ),
    id,
  );
  if (error) return <Alert severity="error" sx={{ mt: 3 }}>{error}</Alert>;
  if (!data) return <LinearProgress sx={{ mt: 3 }} />;
  const b = data.build;
  const facts: [string, React.ReactNode][] = [
    ["started", new Date(b.started_at).toLocaleString()],
    ["took", duration(b.duration_ms)],
    ["branch", b.branch && <Mono>{b.branch}</Mono>],
    ["commit", b.commit && <Mono>{b.commit}</Mono>],
    ["tag", b.tag && <Mono>{b.tag}</Mono>],
    ["build", b.build_number],
    ["by", [b.user, b.host].filter(Boolean).join("@")],
    ["ci", b.ci],
    ["ciabatta", b.version],
  ];
  return (
    <Section
      title={`${data.project_name || project} · ${b.workflow}`}
      action={<Tag kind={b.outcome} />}
    >
      <Box sx={{ display: "flex", flexWrap: "wrap", columnGap: 3, rowGap: 0.5, mb: 2 }}>
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
        <Alert severity="error" sx={{ mb: 2, fontFamily: monoFontStack }}>
          {b.error}
        </Alert>
      )}
      {/* A recovery node that was never entered is a branch the run didn't
          take; it stays in the table, but drawing it would hang it off nothing. */}
      <Flow steps={b.steps.filter((s) => !(s.kind === "recover" && s.status === "not run"))} />
      <TableContainer component={Card} sx={{ mt: 2 }}>
        <Table size="small">
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
              <TableRow key={s.name}>
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
    </Section>
  );
}

/**
 * The flow as layers: each step one column to the right of the furthest step
 * it needs, so everything a step waits for sits to its left.
 */
function Flow({ steps }: { steps: Step[] }) {
  const theme = useTheme();
  const colors = useChartColors();
  const byName = new Map(steps.map((s) => [s.name, s]));
  const depth = new Map<string, number>();
  const depthOf = (name: string, seen = new Set<string>()): number => {
    if (depth.has(name)) return depth.get(name)!;
    if (seen.has(name)) return 0;
    seen.add(name);
    const needs = (byName.get(name)?.needs ?? []).filter((n) => byName.has(n));
    const d = needs.length ? 1 + Math.max(...needs.map((n) => depthOf(n, seen))) : 0;
    depth.set(name, d);
    return d;
  };
  steps.forEach((s) => depthOf(s.name));
  const columns: Step[][] = [];
  steps.forEach((s) => (columns[depth.get(s.name)!] ??= []).push(s));

  const W = 190;
  const H = 62;
  const GX = 60;
  const GY = 14;
  const PAD = 16;
  const at = new Map<string, { x: number; y: number }>();
  columns.forEach((column, x) =>
    column.forEach((s, y) => at.set(s.name, { x: PAD + x * (W + GX), y: PAD + y * (H + GY) })),
  );
  const width = PAD * 2 + columns.length * (W + GX) - GX;
  const height = PAD * 2 + Math.max(1, ...columns.map((c) => c.length)) * (H + GY) - GY;

  const edge = (s: Step) => {
    switch (s.status) {
      case "success":
      case "reused":
        return colors.success;
      case "failed":
        return colors.failed;
      default:
        return theme.palette.divider;
    }
  };

  return (
    <Box
      sx={{
        overflow: "auto",
        maxHeight: 520,
        border: 1,
        borderColor: "divider",
        borderRadius: 1,
        bgcolor: "background.default",
      }}
    >
      <Box sx={{ position: "relative", width, height }}>
        <svg width={width} height={height} style={{ position: "absolute", inset: 0 }}>
          {steps.flatMap((s) =>
            (s.needs ?? []).map((n) => {
              const from = at.get(n);
              const to = at.get(s.name);
              if (!from || !to) return null;
              const x1 = from.x + W;
              const y1 = from.y + H / 2;
              const x2 = to.x;
              const y2 = to.y + H / 2;
              const mid = (x1 + x2) / 2;
              return (
                <path
                  key={`${n}->${s.name}`}
                  d={`M${x1},${y1} C${mid},${y1} ${mid},${y2} ${x2},${y2}`}
                  fill="none"
                  stroke={theme.palette.divider}
                  strokeWidth={1.5}
                />
              );
            }),
          )}
        </svg>
        {steps.map((s) => {
          const p = at.get(s.name)!;
          const faded = s.status === "skipped" || s.status === "not run";
          const detail = [
            s.status,
            s.cache === "hit" || s.cache === "fresh" ? "from cache" : "",
            duration(s.duration_ms),
          ]
            .filter(Boolean)
            .join(" · ");
          return (
            <Box
              key={s.name}
              title={s.note ?? ""}
              sx={{
                position: "absolute",
                left: p.x,
                top: p.y,
                width: W,
                height: H,
                px: 1,
                py: 0.75,
                borderRadius: 1,
                fontSize: "0.8rem",
                bgcolor: "background.paper",
                border: 1,
                borderColor: "divider",
                borderLeft: `4px solid ${edge(s)}`,
                borderStyle: s.status === "reused" ? "dashed" : "solid",
                borderLeftStyle: "solid",
                opacity: faded ? 0.65 : 1,
                overflow: "hidden",
              }}
            >
              <Box
                sx={{
                  fontFamily: monoFontStack,
                  fontWeight: 600,
                  whiteSpace: "nowrap",
                  overflow: "hidden",
                  textOverflow: "ellipsis",
                }}
              >
                {s.name}
              </Box>
              <Box sx={{ color: "text.secondary", whiteSpace: "nowrap", overflow: "hidden", textOverflow: "ellipsis" }}>
                {s.workspace || s.kind || ""}
              </Box>
              <Box sx={{ color: "text.secondary", whiteSpace: "nowrap", overflow: "hidden", textOverflow: "ellipsis" }}>
                {detail}
              </Box>
            </Box>
          );
        })}
      </Box>
    </Box>
  );
}
