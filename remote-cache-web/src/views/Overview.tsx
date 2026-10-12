import { Fragment, useState } from "react";
import {
  Alert,
  Box,
  Card,
  CardContent,
  Chip,
  IconButton,
  LinearProgress,
  Stack,
  Table,
  TableBody,
  TableCell,
  TableContainer,
  TableHead,
  TableRow,
  ToggleButton,
  ToggleButtonGroup,
  Typography,
} from "@mui/material";
import CheckCircleIcon from "@mui/icons-material/CheckCircleOutline";
import ExpandLessIcon from "@mui/icons-material/KeyboardArrowUp";
import ExpandMoreIcon from "@mui/icons-material/KeyboardArrowDown";
import InfoIcon from "@mui/icons-material/InfoOutlined";
import WarningIcon from "@mui/icons-material/WarningAmberRounded";

import { api, type EntrySummary, type ProjectStats, type Stats } from "../api";
import {
  CapacityGauge,
  DailyHitRate,
  DailyLookups,
  DailySaved,
  MissingTargets,
  ProjectLookups,
  ProjectSaved,
  RateGauge,
} from "../components/charts";
import {
  ChartCard,
  EmptyRow,
  Mono,
  Prose,
  Section,
  StatTile,
  Tag,
  When,
} from "../components/ui";
import { ago, human, rate, saved } from "../format";
import { useLoad } from "../useLoad";

const SPANS = [7, 30, 90] as const;

export function Overview({ refreshKey }: { refreshKey: number }) {
  const { data: s, error, loading } = useLoad(() => api<Stats>("/api/stats"), refreshKey);
  const [span, setSpan] = useState<number>(30);

  if (error) return <Alert severity="error" sx={{ mt: 3 }}>{error}</Alert>;
  if (!s) return loading ? <LinearProgress sx={{ mt: 3 }} /> : null;

  const a = s.activity;
  const lookups = s.counters.hits + s.counters.misses;
  const ev = a.last_eviction;

  return (
    <Box>
      {a.since && (
        <Typography variant="body2" color="text.secondary" sx={{ mt: 3 }}>
          Counted since {new Date(a.since).toLocaleDateString()} — these survive restarts.
        </Typography>
      )}

      {/* The headline: the hit rate, and how it got there. */}
      <Box
        sx={{
          mt: 2,
          display: "grid",
          gap: 2,
          gridTemplateColumns: { xs: "1fr", md: "280px 1fr" },
        }}
      >
        <Card>
          <CardContent sx={{ textAlign: "center" }}>
            <Typography variant="h3" sx={{ textAlign: "left" }}>
              Hit rate
            </Typography>
            <Typography
              variant="caption"
              color="text.secondary"
              component="div"
              sx={{ textAlign: "left" }}
            >
              Every lookup, all projects
            </Typography>
            <RateGauge value={s.hit_rate} height={170} />
            <Typography variant="body2" color="text.secondary">
              {s.counters.hits.toLocaleString()} of {lookups.toLocaleString()} lookups reused a
              build
            </Typography>
            <Stack direction="row" spacing={3} justifyContent="center" sx={{ mt: 2 }}>
              <Box>
                <Typography sx={{ fontSize: "1.3rem", fontWeight: 600 }}>
                  {s.counters.hits.toLocaleString()}
                </Typography>
                <Typography variant="caption" color="text.secondary">
                  hits
                </Typography>
              </Box>
              <Box>
                <Typography sx={{ fontSize: "1.3rem", fontWeight: 600 }}>
                  {s.counters.misses.toLocaleString()}
                </Typography>
                <Typography variant="caption" color="text.secondary">
                  misses
                </Typography>
              </Box>
              <Box>
                <Typography sx={{ fontSize: "1.3rem", fontWeight: 600 }}>
                  {s.counters.uploads.toLocaleString()}
                </Typography>
                <Typography variant="caption" color="text.secondary">
                  uploads
                </Typography>
              </Box>
            </Stack>
            {/* What the hit rate is worth: the build time nobody had to sit through. */}
            <Box sx={{ mt: 2.5, pt: 2, borderTop: 1, borderColor: "divider" }}>
              <Typography variant="overline" color="text.secondary" component="div">
                Build time saved
              </Typography>
              <Typography sx={{ fontSize: "2rem", fontWeight: 600, lineHeight: 1.1 }}>
                {s.counters.saved_ms === undefined ? "—" : saved(s.counters.saved_ms)}
              </Typography>
              <Typography variant="caption" color="text.secondary">
                {s.counters.saved_ms === undefined
                  ? "upgrade the server to count this"
                  : "summed over every hit, from each build's original time"}
              </Typography>
            </Box>
          </CardContent>
        </Card>

        <ChartCard
          title="Lookups per day"
          sub="Hits and misses across every project, by UTC day"
          action={
            <ToggleButtonGroup
              size="small"
              exclusive
              value={span}
              onChange={(_, next: number | null) => next && setSpan(next)}
              aria-label="Days shown"
            >
              {SPANS.map((d) => (
                <ToggleButton key={d} value={d} sx={{ px: 1.25, py: 0.25 }}>
                  {d}d
                </ToggleButton>
              ))}
            </ToggleButtonGroup>
          }
        >
          <DailyLookups daily={a.daily} span={span} />
          <Typography variant="overline" color="text.secondary" component="div" sx={{ mt: 1 }}>
            Daily hit rate
          </Typography>
          <DailyHitRate daily={a.daily} span={span} height={150} />
        </ChartCard>
      </Box>

      {/* The rest of the vital signs. */}
      <Box
        sx={{
          mt: 2,
          display: "grid",
          gap: 2,
          gridTemplateColumns: "repeat(auto-fit, minmax(150px, 1fr))",
        }}
      >
        <StatTile
          label="Last used"
          value={ago(a.last_used_at)}
          detail={a.last_used_at ? new Date(a.last_used_at).toLocaleString() : "nothing yet"}
        />
        <StatTile
          label="Last saved to"
          value={ago(a.last_saved_at)}
          detail={
            a.last_saved_at ? new Date(a.last_saved_at).toLocaleString() : "nothing uploaded yet"
          }
        />
        <StatTile
          label="Last miss"
          value={ago(a.last_miss_at)}
          detail={`${s.counters.misses.toLocaleString()} in all`}
        />
        <StatTile
          label="Stored"
          value={s.storage.human}
          detail={
            `${s.storage.entries.toLocaleString()} entries` +
            (s.storage.max_bytes ? ` · limit ${human(s.storage.max_bytes)}` : "")
          }
        >
          {s.storage.percent != null && (
            <LinearProgress
              variant="determinate"
              value={Math.min(100, s.storage.percent)}
              color={
                s.storage.percent >= 100 ? "error" : s.storage.percent >= 80 ? "warning" : "primary"
              }
              sx={{ mt: 1, height: 6, borderRadius: 3 }}
            />
          )}
        </StatTile>
        <StatTile
          label="Served"
          value={human(s.counters.bytes_served)}
          detail={`${s.counters.uploads.toLocaleString()} uploads`}
        />
        <StatTile
          label="Projects"
          value={s.projects.length}
          detail={`${s.sessions} live session(s)`}
        />
        <StatTile
          label="Connection"
          value={
            <Box component="span" sx={{ color: s.tls ? "success.main" : "text.primary" }}>
              {s.tls ? "HTTPS" : "HTTP"}
            </Box>
          }
          detail={s.tls ? "TLS terminated here" : "set server.tls to encrypt"}
        />
      </Box>

      <Section title="Worth a look" sub="What to change to get more out of this cache, most costly first.">
        <Stack spacing={1}>
          {s.insights.length === 0 ? (
            <Alert icon={<CheckCircleIcon />} severity="success" variant="outlined">
              Nothing needs attention.
            </Alert>
          ) : (
            s.insights.map((i, n) => (
              <Alert
                key={n}
                severity={i.severity === "warn" ? "warning" : "info"}
                variant="outlined"
                icon={i.severity === "warn" ? <WarningIcon /> : <InfoIcon />}
              >
                {i.project && (
                  <Typography variant="caption" color="text.secondary" sx={{ mr: 1 }}>
                    {i.project}
                    {i.target ? ` · ${i.target}` : ""}
                  </Typography>
                )}
                <Typography component="span" sx={{ fontWeight: 600 }}>
                  <Prose text={i.message} />
                </Typography>
                <Typography variant="body2" color="text.secondary">
                  <Prose text={i.action} />
                </Typography>
              </Alert>
            ))
          )}
        </Stack>
      </Section>

      <Section
        title="Time saved"
        sub="Every hit hands back a build somebody would otherwise have waited for. Summed across every runner, so it can outpace the clock."
      >
        <Box
          sx={{ display: "grid", gap: 2, gridTemplateColumns: { xs: "1fr", lg: "3fr 2fr" } }}
        >
          <ChartCard title="Build time saved per day" sub={`Last ${span} days, by UTC day`}>
            <DailySaved daily={a.daily} span={span} />
          </ChartCard>
          <ChartCard title="By project" sub="Since counting began">
            <ProjectSaved projects={s.projects} />
          </ChartCard>
        </Box>
      </Section>

      <Section title="Where the misses are" sub="Lookups per project, and the targets that missed most.">
        <Box
          sx={{ display: "grid", gap: 2, gridTemplateColumns: { xs: "1fr", lg: "1fr 1fr" } }}
        >
          <ChartCard title="Projects" sub="Busiest ten, by lookups">
            <ProjectLookups projects={s.projects} />
          </ChartCard>
          <ChartCard title="Targets that miss most" sub="Each miss was a full build somebody waited for">
            <MissingTargets projects={s.projects} />
          </ChartCard>
        </Box>
      </Section>

      <Section title="Projects" sub="Expand a project for the targets that miss most — the ones to look at first.">
        <ProjectsTable projects={s.projects} />
      </Section>

      <Section title="Recent activity" sub="The latest lookups and uploads, newest first.">
        <TableContainer component={Card}>
          <Table size="small">
            <TableHead>
              <TableRow>
                <TableCell>When</TableCell>
                <TableCell>Project</TableCell>
                <TableCell>What</TableCell>
                <TableCell>Target</TableCell>
                <TableCell>Who</TableCell>
                <TableCell align="right">Size</TableCell>
              </TableRow>
            </TableHead>
            <TableBody>
              {a.recent.length === 0 && <EmptyRow colSpan={6}>Nothing yet.</EmptyRow>}
              {a.recent.map((e, n) => (
                <TableRow key={n} hover>
                  <TableCell>
                    <When at={e.at} />
                  </TableCell>
                  <TableCell>{e.project_name}</TableCell>
                  <TableCell>
                    <Tag kind={e.kind} label={e.kind === "touch" ? `kept ${e.count} alive` : e.kind} />
                  </TableCell>
                  <TableCell>
                    <Mono>{e.target ?? ""}</Mono>
                  </TableCell>
                  <TableCell>{e.user ?? ""}</TableCell>
                  <TableCell align="right">{e.bytes ? human(e.bytes) : ""}</TableCell>
                </TableRow>
              ))}
            </TableBody>
          </Table>
        </TableContainer>
      </Section>

      <Section
        title="Storage"
        sub={
          `Retention: ${s.retention.description}.` +
          (ev
            ? ` Last eviction ${ago(ev.at)}: ${ev.removed} entr${ev.removed === 1 ? "y" : "ies"}, ${human(ev.freed)}.`
            : " Nothing evicted yet.")
        }
      >
        <Box
          sx={{
            display: "grid",
            gap: 2,
            gridTemplateColumns: {
              xs: "1fr",
              lg: s.storage.percent != null ? "240px 1fr 1fr" : "1fr 1fr",
            },
          }}
        >
          {s.storage.percent != null && (
            <ChartCard title="Capacity" sub={`${s.storage.human} of ${human(s.storage.max_bytes)}`}>
              <CapacityGauge percent={s.storage.percent} />
            </ChartCard>
          )}
          <EntryTable title="Largest entries" entries={s.largest} />
          <EntryTable title="Least recently used — first to be evicted" entries={s.least_recently_used} />
        </Box>
      </Section>
    </Box>
  );
}

function ProjectsTable({ projects }: { projects: ProjectStats[] }) {
  const [expanded, setExpanded] = useState<Set<string>>(new Set());
  const toggle = (id: string) =>
    setExpanded((prev) => {
      const next = new Set(prev);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });

  return (
    <TableContainer component={Card}>
      <Table size="small">
        <TableHead>
          <TableRow>
            <TableCell padding="checkbox" />
            <TableCell>Project</TableCell>
            <TableCell align="right">Hit rate</TableCell>
            <TableCell align="right">Hits / misses</TableCell>
            <TableCell align="right">Saved</TableCell>
            <TableCell align="right">Stored</TableCell>
            <TableCell>Last used</TableCell>
            <TableCell>Last saved</TableCell>
          </TableRow>
        </TableHead>
        <TableBody>
          {projects.length === 0 && (
            <EmptyRow colSpan={8}>
              No projects yet. Point a workspace here with{" "}
              <Mono>ciabatta cache init --remote {location.origin}</Mono>.
            </EmptyRow>
          )}
          {projects.map((p) => {
            const id = p.project.id;
            const open = expanded.has(id);
            return (
              <Fragment key={id}>
                <TableRow hover onClick={() => toggle(id)} sx={{ cursor: "pointer" }}>
                  <TableCell padding="checkbox">
                    <IconButton size="small" aria-label={open ? "Collapse" : "Expand"}>
                      {open ? <ExpandLessIcon /> : <ExpandMoreIcon />}
                    </IconButton>
                  </TableCell>
                  <TableCell>
                    <b>{p.project.name}</b>{" "}
                    <Typography component="span" variant="caption" color="text.secondary">
                      <Mono>{id.slice(0, 8)}</Mono>
                    </Typography>
                    {p.stale_workflows > 0 && (
                      <Chip
                        size="small"
                        variant="outlined"
                        label={`${p.stale_workflows} stale`}
                        title="Workflows nobody has run lately"
                        sx={{ ml: 1, height: 20 }}
                      />
                    )}
                  </TableCell>
                  <TableCell align="right">
                    <HitBar rate={p.hit_rate} />
                  </TableCell>
                  <TableCell align="right">
                    {p.counters.hits.toLocaleString()} / {p.counters.misses.toLocaleString()}
                  </TableCell>
                  <TableCell align="right">
                    {p.counters.saved_ms === undefined ? "—" : saved(p.counters.saved_ms)}
                  </TableCell>
                  <TableCell align="right">
                    {human(p.bytes)}{" "}
                    <Typography component="span" variant="caption" color="text.secondary">
                      · {p.entries}
                    </Typography>
                  </TableCell>
                  <TableCell>
                    <When at={p.last_used_at} />
                  </TableCell>
                  <TableCell>
                    <When at={p.last_saved_at || p.newest_entry_at} />
                  </TableCell>
                </TableRow>
                {open && (
                  <TableRow>
                    <TableCell colSpan={8} sx={{ bgcolor: "action.hover", py: 1.5 }}>
                      {p.targets.length === 0 ? (
                        <Typography variant="body2" color="text.secondary">
                          No per-target traffic yet — clients older than this server don't name
                          their targets.
                        </Typography>
                      ) : (
                        <Table size="small">
                          <TableHead>
                            <TableRow>
                              <TableCell>Target</TableCell>
                              <TableCell align="right">Hits</TableCell>
                              <TableCell align="right">Misses</TableCell>
                              <TableCell align="right">Uploads</TableCell>
                              <TableCell>Last hit</TableCell>
                              <TableCell>Last miss</TableCell>
                              <TableCell>Last saved</TableCell>
                            </TableRow>
                          </TableHead>
                          <TableBody>
                            {p.targets.map((t) => (
                              <TableRow key={t.name}>
                                <TableCell>
                                  <Mono>{t.name}</Mono>
                                </TableCell>
                                <TableCell align="right">{t.hits}</TableCell>
                                <TableCell align="right">{t.misses}</TableCell>
                                <TableCell align="right">{t.uploads}</TableCell>
                                <TableCell>
                                  <When at={t.last_hit_at} />
                                </TableCell>
                                <TableCell>
                                  <When at={t.last_miss_at} />
                                </TableCell>
                                <TableCell>
                                  <When at={t.last_upload_at} />
                                </TableCell>
                              </TableRow>
                            ))}
                          </TableBody>
                        </Table>
                      )}
                    </TableCell>
                  </TableRow>
                )}
              </Fragment>
            );
          })}
        </TableBody>
      </Table>
    </TableContainer>
  );
}

/** A hit rate as a number with a thin bar under it, so a column of them scans. */
function HitBar({ rate: value }: { rate: number | null }) {
  return (
    <Box sx={{ display: "inline-flex", alignItems: "center", gap: 1 }}>
      <Box sx={{ width: 64, display: { xs: "none", sm: "block" } }}>
        {value !== null && (
          <LinearProgress
            variant="determinate"
            value={value}
            color="success"
            sx={{ height: 6, borderRadius: 3 }}
          />
        )}
      </Box>
      <Box component="span" sx={{ minWidth: 36 }}>
        {rate(value)}
      </Box>
    </Box>
  );
}

function EntryTable({ title, entries }: { title: string; entries: EntrySummary[] }) {
  return (
    <TableContainer component={Card}>
      <Typography variant="h3" sx={{ px: 2, pt: 2, pb: 1 }}>
        {title}
      </Typography>
      <Table size="small">
        <TableBody>
          {entries.length === 0 && <EmptyRow colSpan={3}>Empty.</EmptyRow>}
          {entries.map((e, n) => (
            <TableRow key={n}>
              <TableCell>
                <Mono>{e.target}</Mono>
                <Typography variant="caption" color="text.secondary" component="div">
                  {e.project}
                </Typography>
              </TableCell>
              <TableCell align="right">{human(e.size)}</TableCell>
              <TableCell>
                <When at={e.last_used_at} />
              </TableCell>
            </TableRow>
          ))}
        </TableBody>
      </Table>
    </TableContainer>
  );
}
