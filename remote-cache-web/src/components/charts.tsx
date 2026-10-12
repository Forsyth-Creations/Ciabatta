/**
 * The charts on the cache's overview.
 *
 * Rules every chart here follows, so they read as one set:
 *
 * - Hits and misses always wear the same two colours (see `ChartColors`), on
 *   every chart, and always in that order — hits first, at the baseline.
 * - One y-scale per chart. Lookup counts and the hit rate are different units,
 *   so they're two charts stacked over the same days rather than one with two
 *   axes.
 * - Every multi-series chart has a legend; text stays in text colours, never
 *   the series colour; and every chart has a table somewhere on the page with
 *   the same numbers, for anyone the colours don't work for.
 */

import { Box, useMediaQuery, useTheme } from "@mui/material";
import { BarChart } from "@mui/x-charts/BarChart";
import { Gauge, gaugeClasses } from "@mui/x-charts/Gauge";
import { LineChart } from "@mui/x-charts/LineChart";

import type { Day, ProjectStats } from "../api";
import { compact, lastDays, rate, saved, shortDay, timeUnit } from "../format";
import { NoData, useChartColors } from "./ui";

const count = (v: number | null) => (v === null ? "" : v.toLocaleString());
const percent = (v: number | null) => (v === null ? "no lookups" : `${v.toFixed(0)}%`);

/** Recessive gridlines and axes, so the marks carry the chart. */
function useAxisSx() {
  const theme = useTheme();
  return {
    "& .MuiChartsAxis-line, & .MuiChartsAxis-tick": { stroke: theme.palette.divider },
    "& .MuiChartsAxis-tickLabel": { fill: theme.palette.text.secondary },
    "& .MuiChartsGrid-line": { stroke: theme.palette.divider, strokeDasharray: "2 4" },
  };
}

/** Every nth day labelled, so the labels never collide on a narrow screen. */
function useTickEvery(span: number) {
  const narrow = useMediaQuery(useTheme().breakpoints.down("sm"));
  return Math.ceil(span / (narrow ? 4 : 8));
}

function daysOf(daily: Record<string, Day> | undefined, span: number) {
  const days = lastDays(span);
  const rows = days.map((day) => daily?.[day] ?? null);
  return { days, rows };
}

/** Hits and misses per day, stacked: the height is the day's traffic. */
export function DailyLookups({
  daily,
  span,
  height = 260,
}: {
  daily: Record<string, Day> | undefined;
  span: number;
  height?: number;
}) {
  const colors = useChartColors();
  const sx = useAxisSx();
  const every = useTickEvery(span);
  const { days, rows } = daysOf(daily, span);
  if (!daily || rows.every((r) => !r || r.hits + r.misses === 0)) {
    return (
      <NoData height={height}>
        {daily
          ? "No lookups in this window yet."
          : "This server doesn't keep a daily history — upgrade it to see this chart."}
      </NoData>
    );
  }
  return (
    <BarChart
      height={height}
      borderRadius={4}
      sx={sx}
      grid={{ horizontal: true }}
      margin={{ top: 40, right: 12, bottom: 30, left: 48 }}
      xAxis={[
        {
          scaleType: "band" as const,
          data: days,
          valueFormatter: (d: string) => shortDay(d),
          tickLabelInterval: (_v: string, i: number) => i % every === 0,
        },
      ]}
      yAxis={[{ valueFormatter: (v: number) => compact(v) }]}
      series={[
        {
          id: "hits",
          label: "Hits",
          stack: "lookups",
          color: colors.hit,
          data: rows.map((r) => r?.hits ?? 0),
          valueFormatter: count,
        },
        {
          id: "misses",
          label: "Misses",
          stack: "lookups",
          color: colors.miss,
          data: rows.map((r) => r?.misses ?? 0),
          valueFormatter: count,
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

/** The share of each day's lookups that hit. Days with none are gaps, not 0%. */
export function DailyHitRate({
  daily,
  span,
  height = 180,
}: {
  daily: Record<string, Day> | undefined;
  span: number;
  height?: number;
}) {
  const colors = useChartColors();
  const sx = useAxisSx();
  const every = useTickEvery(span);
  const { days, rows } = daysOf(daily, span);
  const data = rows.map((r) => {
    const lookups = r ? r.hits + r.misses : 0;
    return lookups ? (r!.hits * 100) / lookups : null;
  });
  if (!daily || data.every((v) => v === null)) {
    return <NoData height={height}>The hit rate appears here once there are lookups.</NoData>;
  }
  return (
    <LineChart
      height={height}
      sx={{
        ...sx,
        "& .MuiAreaElement-root": { fillOpacity: 0.12 },
        "& .MuiLineElement-root": { strokeWidth: 2 },
      }}
      grid={{ horizontal: true }}
      margin={{ top: 12, right: 12, bottom: 30, left: 48 }}
      xAxis={[
        {
          scaleType: "point" as const,
          data: days,
          valueFormatter: (d: string) => shortDay(d),
          tickLabelInterval: (_v: string, i: number) => i % every === 0,
        },
      ]}
      yAxis={[{ min: 0, max: 100, valueFormatter: (v: number) => `${v}%` }]}
      series={[
        {
          id: "rate",
          label: "Hit rate",
          color: colors.hit,
          data,
          area: true,
          connectNulls: false,
          showMark: span <= 31,
          valueFormatter: percent,
        },
      ]}
      slotProps={{ legend: { hidden: true } }}
    />
  );
}

/** A half-dial: the one number this page exists to move. */
export function RateGauge({
  value,
  height = 150,
  label,
}: {
  value: number | null;
  height?: number;
  label?: string;
}) {
  const colors = useChartColors();
  const theme = useTheme();
  return (
    <Gauge
      height={height}
      value={value ?? 0}
      startAngle={-110}
      endAngle={110}
      innerRadius="72%"
      outerRadius="100%"
      cornerRadius={4}
      text={() => label ?? rate(value)}
      aria-label={`Hit rate ${rate(value)}`}
      sx={{
        [`& .${gaugeClasses.valueText}`]: {
          fontSize: 26,
          fontWeight: 600,
          transform: "translate(0px, 6px)",
        },
        [`& .${gaugeClasses.valueText} text`]: { fill: theme.palette.text.primary },
        [`& .${gaugeClasses.valueArc}`]: { fill: value === null ? "transparent" : colors.hit },
        [`& .${gaugeClasses.referenceArc}`]: { fill: theme.palette.action.selected },
      }}
    />
  );
}

/** How full the store is against its retention limit. */
export function CapacityGauge({
  percent: pct,
  height = 150,
}: {
  percent: number;
  height?: number;
}) {
  const theme = useTheme();
  const clamped = Math.min(100, pct);
  const fill =
    clamped >= 100
      ? theme.palette.error.main
      : clamped >= 80
        ? theme.palette.warning.main
        : theme.palette.primary.main;
  return (
    <Gauge
      height={height}
      value={clamped}
      startAngle={-110}
      endAngle={110}
      innerRadius="72%"
      outerRadius="100%"
      cornerRadius={4}
      text={() => `${clamped.toFixed(0)}%`}
      aria-label={`Store ${clamped.toFixed(0)}% full`}
      sx={{
        [`& .${gaugeClasses.valueText}`]: {
          fontSize: 26,
          fontWeight: 600,
          transform: "translate(0px, 6px)",
        },
        [`& .${gaugeClasses.valueText} text`]: { fill: theme.palette.text.primary },
        [`& .${gaugeClasses.valueArc}`]: { fill },
        [`& .${gaugeClasses.referenceArc}`]: { fill: theme.palette.action.selected },
      }}
    />
  );
}

/** Truncate a long axis label; the tooltip carries the full name. */
const clip = (max: number) => (s: string) => (s.length > max ? `${s.slice(0, max - 1)}…` : s);

/** Each project's hits and misses side by side, busiest first. */
export function ProjectLookups({ projects }: { projects: ProjectStats[] }) {
  const colors = useChartColors();
  const sx = useAxisSx();
  const busy = projects
    .filter((p) => p.counters.hits + p.counters.misses > 0)
    .sort((a, b) => b.counters.hits + b.counters.misses - (a.counters.hits + a.counters.misses))
    .slice(0, 10);
  const height = Math.max(160, 56 + busy.length * 34);
  if (busy.length === 0) {
    return <NoData height={160}>No project has looked anything up yet.</NoData>;
  }
  return (
    <BarChart
      height={height}
      layout="horizontal"
      borderRadius={4}
      sx={sx}
      grid={{ vertical: true }}
      margin={{ top: 40, right: 16, bottom: 24, left: 130 }}
      yAxis={[
        {
          scaleType: "band" as const,
          data: busy.map((p) => p.project.name),
          valueFormatter: (name: string, ctx) => (ctx.location === "tick" ? clip(18)(name) : name),
        },
      ]}
      xAxis={[{ valueFormatter: (v: number) => compact(v) }]}
      series={[
        {
          id: "hits",
          label: "Hits",
          stack: "lookups",
          color: colors.hit,
          data: busy.map((p) => p.counters.hits),
          valueFormatter: count,
        },
        {
          id: "misses",
          label: "Misses",
          stack: "lookups",
          color: colors.miss,
          data: busy.map((p) => p.counters.misses),
          valueFormatter: count,
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

/** The targets that cost the most builds, across every project. */
export function MissingTargets({ projects }: { projects: ProjectStats[] }) {
  const colors = useChartColors();
  const sx = useAxisSx();
  const targets = projects
    .flatMap((p) =>
      (p.targets ?? []).map((t) => ({ ...t, label: `${t.name}`, project: p.project.name })),
    )
    .filter((t) => t.misses > 0)
    .sort((a, b) => b.misses - a.misses)
    .slice(0, 10);
  if (targets.length === 0) {
    return <NoData height={160}>No target has missed. Nothing to chase.</NoData>;
  }
  const height = Math.max(160, 40 + targets.length * 30);
  // Two projects can share a target name; the band axis needs distinct keys.
  const keys = targets.map((t, i) => `${i}\u0000${t.project} · ${t.name}`);
  const name = (key: string) => key.split("\u0000")[1] ?? key;
  return (
    <Box>
      <BarChart
        height={height}
        layout="horizontal"
        borderRadius={4}
        sx={sx}
        grid={{ vertical: true }}
        margin={{ top: 8, right: 16, bottom: 24, left: 170 }}
        yAxis={[
          {
            scaleType: "band" as const,
            data: keys,
            valueFormatter: (key: string, ctx) =>
              ctx.location === "tick" ? clip(24)(name(key).split(" · ")[1] ?? "") : name(key),
          },
        ]}
        xAxis={[{ valueFormatter: (v: number) => compact(v) }]}
        series={[
          {
            id: "misses",
            label: "Misses",
            color: colors.miss,
            data: targets.map((t) => t.misses),
            valueFormatter: count,
          },
        ]}
        slotProps={{ legend: { hidden: true } }}
      />
    </Box>
  );
}

/** Build time the cache handed back each day — the payoff, in hours. */
export function DailySaved({
  daily,
  span,
  height = 220,
}: {
  daily: Record<string, Day> | undefined;
  span: number;
  height?: number;
}) {
  const colors = useChartColors();
  const sx = useAxisSx();
  const every = useTickEvery(span);
  const { days, rows } = daysOf(daily, span);
  const ms = rows.map((r) => r?.saved_ms ?? 0);
  const unit = timeUnit(Math.max(0, ...ms));
  if (!daily || ms.every((v) => v === 0)) {
    return (
      <NoData height={height}>
        {daily
          ? "No build time saved in this window yet — it's counted from each hit's original build time."
          : "This server doesn't keep a daily history — upgrade it to see this chart."}
      </NoData>
    );
  }
  return (
    <BarChart
      height={height}
      borderRadius={4}
      sx={sx}
      grid={{ horizontal: true }}
      margin={{ top: 12, right: 12, bottom: 30, left: 48 }}
      xAxis={[
        {
          scaleType: "band" as const,
          data: days,
          valueFormatter: (d: string) => shortDay(d),
          tickLabelInterval: (_v: string, i: number) => i % every === 0,
        },
      ]}
      yAxis={[{ valueFormatter: (v: number) => `${compact(v)}${unit.suffix}` }]}
      series={[
        {
          id: "saved",
          label: "Build time saved",
          color: colors.hit,
          data: ms.map((v) => v / unit.ms),
          valueFormatter: (v: number | null) => (v === null ? "" : saved(v * unit.ms)),
        },
      ]}
      slotProps={{ legend: { hidden: true } }}
    />
  );
}

/** Build time saved per project, most first. */
export function ProjectSaved({ projects }: { projects: ProjectStats[] }) {
  const colors = useChartColors();
  const sx = useAxisSx();
  const top = projects
    .filter((p) => (p.counters.saved_ms ?? 0) > 0)
    .sort((a, b) => (b.counters.saved_ms ?? 0) - (a.counters.saved_ms ?? 0))
    .slice(0, 10);
  if (top.length === 0) {
    return <NoData height={160}>No project has been saved any build time yet.</NoData>;
  }
  const height = Math.max(160, 40 + top.length * 34);
  const unit = timeUnit(Math.max(...top.map((p) => p.counters.saved_ms ?? 0)));
  return (
    <BarChart
      height={height}
      layout="horizontal"
      borderRadius={4}
      sx={sx}
      grid={{ vertical: true }}
      margin={{ top: 8, right: 16, bottom: 24, left: 130 }}
      yAxis={[
        {
          scaleType: "band" as const,
          data: top.map((p) => p.project.name),
          valueFormatter: (name: string, ctx) => (ctx.location === "tick" ? clip(18)(name) : name),
        },
      ]}
      xAxis={[{ valueFormatter: (v: number) => `${compact(v)}${unit.suffix}` }]}
      series={[
        {
          id: "saved",
          label: "Build time saved",
          color: colors.hit,
          data: top.map((p) => (p.counters.saved_ms ?? 0) / unit.ms),
          valueFormatter: (v: number | null) => (v === null ? "" : saved(v * unit.ms)),
        },
      ]}
      slotProps={{ legend: { hidden: true } }}
    />
  );
}
