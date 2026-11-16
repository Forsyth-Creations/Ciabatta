/**
 * Triage: put two watch sessions side by side and find what sets them apart.
 *
 * Pick a log that went wrong and one that didn't, and the daemon groups every
 * line by its shape — timestamps, numbers, ids and addresses masked out, so
 * two runs of the same program line up even though no line is byte-identical
 * — then sorts the shapes into:
 *
 * - **Only in A / Only in B**: printed by one log and nothing like it in the
 *   other. Usually the reason you're here.
 * - **Reworded**: the same message saying something different (`connected to
 *   primary` / `connected to replica`), paired by fuzzy similarity and shown
 *   with a word diff.
 * - **Count changed**: in both, but far more often in one.
 * - **Shared**: in both, about as often. The noise, set aside.
 *
 * Each line links back into its session's log at the place it was printed,
 * to be read with the lines around it.
 *
 * Reached from the Watch page (tick two sessions, press Triage) or from
 * `ciabatta watch --triage`, rather than from a nav entry of its own: it's a
 * view over watch sessions, not a separate tool.
 */

import { useEffect, useMemo, useState } from "react";
import type { ReactNode } from "react";
import {
  Alert,
  Box,
  Button,
  Card,
  Chip,
  IconButton,
  LinearProgress,
  MenuItem,
  Slider,
  Stack,
  Tab,
  Tabs,
  TextField,
  Tooltip,
  Typography,
} from "@mui/material";
import { alpha, styled, useTheme } from "@mui/material/styles";
import ArrowBackIcon from "@mui/icons-material/ArrowBack";
import RefreshIcon from "@mui/icons-material/Refresh";
import SwapHorizIcon from "@mui/icons-material/SwapHoriz";
import { Link, useNavigate, useSearch } from "@tanstack/react-router";

import {
  useTriage,
  useWatchSessions,
  type DiffPart,
  type Occurrences,
  type SessionSummary,
  type TriageGroup,
} from "../api/watch";
import { ErrorNote, Loading, PageHeader, RequireProject } from "../components/Page";
import { monoFontStack } from "../theme";

/** Groups rendered at once; "Show more" adds this many again. */
const PAGE = 200;
/** Line links shown per side before "+N more". */
const SEQ_LINKS = 6;

type TabKey = "only_a" | "only_b" | "similar" | "changed" | "common";

export function TriagePage() {
  return (
    <>
      <PageHeader
        title="Triage"
        description="Compare two watch sessions. Lines are matched by their shape, with timestamps, numbers and ids ignored, so you can see what one log printed that the other didn't."
        actions={
          <Button component={Link} to="/watch" startIcon={<ArrowBackIcon />}>
            Watch sessions
          </Button>
        }
      />
      <RequireProject>{() => <Triage />}</RequireProject>
    </>
  );
}

/** `value`, once it has stopped changing for `ms`. */
function useDebounced<T>(value: T, ms: number): T {
  const [settled, setSettled] = useState(value);
  useEffect(() => {
    const timer = window.setTimeout(() => setSettled(value), ms);
    return () => window.clearTimeout(timer);
  }, [value, ms]);
  return settled;
}

function sessionName(session: SessionSummary): string {
  return session.label ?? session.command;
}

function Triage() {
  const { a, b } = useSearch({ from: "/triage" });
  const navigate = useNavigate({ from: "/triage" });
  const sessions = useWatchSessions();

  const [threshold, setThreshold] = useState(0.75);
  const [thresholdDraft, setThresholdDraft] = useState(0.75);
  const [ignoreDraft, setIgnoreDraft] = useState("");
  const ignore = useDebounced(ignoreDraft, 400);
  const [revision, setRevision] = useState(0);
  const [tab, setTab] = useState<TabKey>("only_a");
  const [filter, setFilter] = useState("");
  const [shown, setShown] = useState(PAGE);

  const pick = (next: { a?: number; b?: number }) =>
    navigate({ search: (prev) => ({ ...prev, ...next }), replace: true });

  // With nothing chosen, start on the two newest sessions, older one as A:
  // the usual case is "it worked before, now it doesn't".
  const list = sessions.data;
  useEffect(() => {
    if (a === undefined && b === undefined && list && list.length >= 2) {
      pick({ a: list[1].id, b: list[0].id });
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [list, a, b]);

  const triage = useTriage(a, b, threshold, ignore, revision);
  const report = triage.data;

  useEffect(() => setShown(PAGE), [tab, filter, report]);

  const groups = useMemo(() => {
    if (!report) return [];
    const byTab = report.groups.filter((g) =>
      tab === "changed"
        ? g.kind === "common" && g.count_changed
        : tab === "common"
          ? g.kind === "common" && !g.count_changed
          : g.kind === tab,
    );
    const needle = filter.trim().toLowerCase();
    if (!needle) return byTab;
    const has = (o: Occurrences | null) =>
      !!o &&
      (o.sample.toLowerCase().includes(needle) || o.template.toLowerCase().includes(needle));
    return byTab.filter((g) => has(g.a) || has(g.b));
  }, [report, tab, filter]);

  if (sessions.isLoading) return <Loading label="Loading sessions…" />;
  if (sessions.error) return <ErrorNote error={sessions.error} />;

  if (!list || list.length < 2) {
    return (
      <Typography variant="body2" color="text.secondary">
        Triage compares two watch sessions, and there{" "}
        {list?.length === 1 ? "is only one" : "are none"} yet. Start them with{" "}
        <code style={{ fontFamily: monoFontStack }}>ciabatta watch &lt;command&gt;</code> — for
        example once on a build that works and once on one that doesn't.
      </Typography>
    );
  }

  const summary = report?.summary;
  const tabs: { key: TabKey; label: string; count?: number; hint: string }[] = [
    {
      key: "only_a",
      label: "Only in A",
      count: summary?.only_a,
      hint: summary ? `${summary.only_a_lines.toLocaleString()} lines` : "",
    },
    {
      key: "only_b",
      label: "Only in B",
      count: summary?.only_b,
      hint: summary ? `${summary.only_b_lines.toLocaleString()} lines` : "",
    },
    { key: "similar", label: "Reworded", count: summary?.similar, hint: "Similar, not the same" },
    {
      key: "changed",
      label: "Count changed",
      count: summary?.count_changed,
      hint: "In both, far more often in one",
    },
    {
      key: "common",
      label: "Shared",
      count: summary ? summary.common - summary.count_changed : undefined,
      hint: "In both, about as often",
    },
  ];
  const cappedHere =
    summary &&
    summary.capped.some((k) =>
      tab === "changed" || tab === "common" ? k === "common" : k === tab,
    );

  return (
    <Stack spacing={2} sx={{ maxWidth: 1200 }}>
      <Card sx={{ p: 2 }}>
        <Stack direction={{ xs: "column", md: "row" }} spacing={1.5} alignItems="center">
          <SessionPicker
            label="Log A"
            sessions={list}
            value={a}
            other={b}
            onChange={(id) => pick({ a: id })}
          />
          <Tooltip title="Swap A and B">
            <IconButton onClick={() => pick({ a: b, b: a })} aria-label="Swap A and B">
              <SwapHorizIcon />
            </IconButton>
          </Tooltip>
          <SessionPicker
            label="Log B"
            sessions={list}
            value={b}
            other={a}
            onChange={(id) => pick({ b: id })}
          />
        </Stack>

        <Stack
          direction={{ xs: "column", md: "row" }}
          spacing={3}
          alignItems={{ md: "center" }}
          sx={{ mt: 2 }}
        >
          <Box sx={{ minWidth: 220 }}>
            <Typography variant="caption" color="text.secondary">
              Reworded when at least {Math.round(thresholdDraft * 100)}% alike
            </Typography>
            <Slider
              size="small"
              min={0.5}
              max={0.95}
              step={0.05}
              value={thresholdDraft}
              onChange={(_, v) => setThresholdDraft(v as number)}
              onChangeCommitted={(_, v) => setThreshold(v as number)}
              aria-label="Similarity threshold"
            />
          </Box>
          <TextField
            size="small"
            label="Also ignore (regex)"
            placeholder="e.g. ^\[worker-\d+\]"
            value={ignoreDraft}
            onChange={(e) => setIgnoreDraft(e.target.value)}
            helperText="Cut from every line before comparing"
            sx={{ flexGrow: 1 }}
            slotProps={{ input: { sx: { fontFamily: monoFontStack } } }}
          />
          <Button
            startIcon={<RefreshIcon />}
            onClick={() => setRevision((r) => r + 1)}
            disabled={triage.isFetching}
          >
            Compare again
          </Button>
        </Stack>
      </Card>

      {a !== undefined && a === b && (
        <Alert severity="info">Pick two different sessions to compare.</Alert>
      )}
      {triage.error && <ErrorNote error={triage.error} />}
      {triage.isFetching && <LinearProgress />}

      {report && summary && (
        <>
          <Typography variant="body2" color="text.secondary">
            A has {summary.lines_a.toLocaleString()} lines in {summary.templates_a.toLocaleString()}{" "}
            shapes; B has {summary.lines_b.toLocaleString()} in{" "}
            {summary.templates_b.toLocaleString()}.
            {(report.session_a.running || report.session_b.running) &&
              " A session is still running — compare again to include its newer output."}
          </Typography>
          {summary.fuzzy_incomplete && (
            <Alert severity="warning">
              These logs are too varied to pair every reworded line in reasonable time, so some
              lines counted as unique may have a close match on the other side.
            </Alert>
          )}

          <Box sx={{ borderBottom: 1, borderColor: "divider" }}>
            <Tabs
              value={tab}
              onChange={(_, v) => setTab(v as TabKey)}
              variant="scrollable"
              scrollButtons="auto"
            >
              {tabs.map((t) => (
                <Tab
                  key={t.key}
                  value={t.key}
                  label={
                    <Tooltip title={t.hint}>
                      <span>
                        {t.label}
                        {t.count !== undefined && ` (${t.count.toLocaleString()})`}
                      </span>
                    </Tooltip>
                  }
                />
              ))}
            </Tabs>
          </Box>

          <TextField
            size="small"
            placeholder="Filter these lines"
            value={filter}
            onChange={(e) => setFilter(e.target.value)}
            sx={{ maxWidth: 420 }}
          />

          {cappedHere && (
            <Alert severity="info">
              Only the first {(5000).toLocaleString()} groups here are listed. Narrow the
              comparison with an ignore pattern to see the rest.
            </Alert>
          )}

          {groups.length === 0 ? (
            <Typography variant="body2" color="text.secondary" sx={{ py: 2 }}>
              {filter ? "Nothing here matches the filter." : emptyMessage(tab)}
            </Typography>
          ) : (
            <Stack spacing={1}>
              {groups.slice(0, shown).map((group, i) => (
                <GroupRow key={i} group={group} sessionA={a!} sessionB={b!} />
              ))}
              {groups.length > shown && (
                <Button onClick={() => setShown((n) => n + PAGE)} sx={{ alignSelf: "flex-start" }}>
                  Show more ({(groups.length - shown).toLocaleString()} left)
                </Button>
              )}
            </Stack>
          )}
        </>
      )}
    </Stack>
  );
}

function emptyMessage(tab: TabKey): string {
  switch (tab) {
    case "only_a":
      return "Every line in A has a match in B.";
    case "only_b":
      return "Every line in B has a match in A.";
    case "similar":
      return "No reworded lines at this threshold.";
    case "changed":
      return "No line appears much more often in one log than the other.";
    case "common":
      return "The logs share nothing.";
  }
}

function SessionPicker({
  label,
  sessions,
  value,
  other,
  onChange,
}: {
  label: string;
  sessions: SessionSummary[];
  value: number | undefined;
  other: number | undefined;
  onChange: (id: number) => void;
}) {
  // A session closed since the URL was made can't be selected; show nothing
  // rather than an out-of-range value MUI would warn about.
  const known = sessions.some((s) => s.id === value);
  return (
    <TextField
      select
      size="small"
      label={label}
      value={known ? value : ""}
      onChange={(e) => onChange(Number(e.target.value))}
      sx={{ flex: 1, minWidth: 0, width: "100%" }}
    >
      {sessions.map((s) => (
        <MenuItem key={s.id} value={s.id} disabled={s.id === other}>
          <Box sx={{ minWidth: 0 }}>
            <Typography
              variant="body2"
              noWrap
              sx={{ fontFamily: s.label ? undefined : monoFontStack }}
            >
              #{s.id} · {sessionName(s)}
            </Typography>
            <Typography variant="caption" color="text.secondary">
              {s.lines.toLocaleString()} lines · {s.running ? "running" : "finished"} · started{" "}
              {new Date(s.created_at).toLocaleString()}
            </Typography>
          </Box>
        </MenuItem>
      ))}
    </TextField>
  );
}

const Mono = styled("div")({
  fontFamily: monoFontStack,
  fontSize: 13,
  lineHeight: "20px",
  whiteSpace: "pre-wrap",
  wordBreak: "break-word",
});

function GroupRow({
  group,
  sessionA,
  sessionB,
}: {
  group: TriageGroup;
  sessionA: number;
  sessionB: number;
}) {
  const theme = useTheme();
  const colorA = theme.palette.warning.main;
  const colorB = theme.palette.info.main;

  const side = (tag: "A" | "B", occ: Occurrences, session: number, color: string, body: ReactNode) => (
    <Stack direction="row" spacing={1.5} alignItems="flex-start">
      <Chip
        size="small"
        label={tag}
        sx={{ bgcolor: alpha(color, 0.15), color, fontWeight: 700, minWidth: 32 }}
      />
      <Box sx={{ minWidth: 0, flex: 1 }}>
        {body}
        <Meta occ={occ} session={session} />
      </Box>
    </Stack>
  );

  const a = group.a;
  const b = group.b;

  return (
    <Card variant="outlined" sx={{ px: 2, py: 1.25 }}>
      <Stack spacing={1}>
        {group.kind === "similar" && group.diff && a && b ? (
          <>
            <Typography variant="caption" color="text.secondary">
              {Math.round(group.similarity * 100)}% alike
            </Typography>
            {side("A", a, sessionA, colorA, <Diff parts={group.diff} side="a" color={colorA} />)}
            {side("B", b, sessionB, colorB, <Diff parts={group.diff} side="b" color={colorB} />)}
          </>
        ) : group.kind === "common" && a && b ? (
          <>
            <Mono>{a.sample}</Mono>
            <Stack direction="row" spacing={3}>
              <CountLink tag="A" occ={a} session={sessionA} color={colorA} />
              <CountLink tag="B" occ={b} session={sessionB} color={colorB} />
            </Stack>
          </>
        ) : a ? (
          side("A", a, sessionA, colorA, <Mono>{a.sample}</Mono>)
        ) : b ? (
          side("B", b, sessionB, colorB, <Mono>{b.sample}</Mono>)
        ) : null}
      </Stack>
    </Card>
  );
}

/** One side of a word diff: the shared words plain, this side's own marked. */
function Diff({ parts, side, color }: { parts: DiffPart[]; side: "a" | "b"; color: string }) {
  return (
    <Mono>
      {parts
        .filter((p) => p.op === "same" || p.op === side)
        .map((p, i) =>
          p.op === "same" ? (
            <span key={i}>{p.text}</span>
          ) : (
            <Box
              component="mark"
              key={i}
              sx={{ bgcolor: alpha(color, 0.25), color: "inherit", borderRadius: 0.5, px: 0.25 }}
            >
              {p.text}
            </Box>
          ),
        )}
    </Mono>
  );
}

/** A link into a session's log at line `seq`. Plain `Link`, not `styled`:
 *  wrapping it loses the route typing that checks `params` and `search`. */
function lineLink(session: number, seq: number, text: ReactNode) {
  return (
    <Link
      key={seq}
      to="/watch/$sessionId"
      params={{ sessionId: String(session) }}
      search={{ line: seq }}
      style={{ color: "inherit" }}
    >
      {text}
    </Link>
  );
}

/** How often, where, and the masked shape when it differs from the sample. */
function Meta({ occ, session }: { occ: Occurrences; session: number }) {
  const more = occ.count - Math.min(occ.seqs.length, SEQ_LINKS);
  return (
    <Typography component="div" variant="caption" color="text.secondary" sx={{ mt: 0.25 }}>
      {occ.count === 1 ? "once" : `${occ.count.toLocaleString()} times`}
      {occ.stderr > 0 && ` · ${occ.stderr === occ.count ? "" : `${occ.stderr} `}on stderr`}
      {" · line "}
      {occ.seqs.slice(0, SEQ_LINKS).map((seq, i) => (
        <span key={seq}>
          {i > 0 && ", "}
          {lineLink(session, seq, seq)}
        </span>
      ))}
      {more > 0 && ` +${more.toLocaleString()} more`}
    </Typography>
  );
}

function CountLink({
  tag,
  occ,
  session,
  color,
}: {
  tag: string;
  occ: Occurrences;
  session: number;
  color: string;
}) {
  return (
    <Typography variant="caption" color="text.secondary">
      <Box component="span" sx={{ color, fontWeight: 700 }}>
        {tag}
      </Box>{" "}
      {occ.count.toLocaleString()}× · first at line{" "}
      {occ.seqs[0] !== undefined && lineLink(session, occ.seqs[0], occ.seqs[0])}
    </Typography>
  );
}
