/**
 * The banner a step's `ciabatta input` question appears in.
 *
 * Mounted once, in the app shell, so a question reaches the operator whatever
 * page they are on. It stays up until the question is settled, but it doesn't
 * take the page: the logs behind it are often what the answer depends on. A
 * click opens it to answer, and a second click or Escape puts it back with
 * whatever was typed still in the field. The ways out are answering, the
 * question timing out, or the run being stopped. Cancel is that last one, said
 * where the question is: it stops the whole run, as the run page's Stop does.
 * Its state lives in the daemon, not here, so reloading the page brings it
 * straight back.
 *
 * A `--yes-no` question is answered with Yes or No, sent as `y` / `n`.
 */

import { useEffect, useMemo, useState } from "react";
import {
  Box,
  Button,
  ButtonBase,
  Chip,
  Collapse,
  LinearProgress,
  Paper,
  Stack,
  TextField,
  Typography,
} from "@mui/material";
import ExpandLessIcon from "@mui/icons-material/ExpandLess";
import ExpandMoreIcon from "@mui/icons-material/ExpandMore";
import HelpOutlineIcon from "@mui/icons-material/HelpOutline";
import { Link } from "@tanstack/react-router";

import { useAnswerInput, useStopRun, useWaitingInputs } from "../api/run";
import type { WaitingInput } from "../api/run";
import { monoFontStack } from "../theme";

/** `4:59`, `1:02:03` — the same clock the terminal prompt shows. */
function clock(ms: number): string {
  const total = Math.max(0, Math.ceil(ms / 1000));
  const h = Math.floor(total / 3600);
  const m = Math.floor((total % 3600) / 60);
  const s = total % 60;
  const pad = (n: number) => String(n).padStart(2, "0");
  return h > 0 ? `${h}:${pad(m)}:${pad(s)}` : `${m}:${pad(s)}`;
}

/** Milliseconds until `deadline`, re-rendered as it ticks; null for none. */
function useRemaining(deadline: string | null): number | null {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    if (!deadline) return;
    const timer = window.setInterval(() => setNow(Date.now()), 250);
    return () => window.clearInterval(timer);
  }, [deadline]);
  return deadline ? new Date(deadline).getTime() - now : null;
}

export function InputPrompt() {
  const { data } = useWaitingInputs();
  // Oldest first: the step that has been waiting longest is answered first.
  const waiting = useMemo(
    () =>
      [...(data?.inputs ?? [])].sort((a, b) =>
        a.asked_at.localeCompare(b.asked_at),
      ),
    [data],
  );
  const current = waiting[0];
  if (!current) return null;
  // Keyed by question, so the next one starts closed with an empty field.
  return (
    <Question
      key={`${current.run}:${current.id}`}
      input={current}
      queued={waiting.length - 1}
    />
  );
}

function Question({ input, queued }: { input: WaitingInput; queued: number }) {
  const [open, setOpen] = useState(false);
  const [value, setValue] = useState("");
  const [cancelling, setCancelling] = useState(false);
  const answer = useAnswerInput();
  const stop = useStopRun();
  const remaining = useRemaining(input.deadline);
  const total = input.deadline
    ? new Date(input.deadline).getTime() - new Date(input.asked_at).getTime()
    : null;
  const expired = remaining !== null && remaining <= 0;

  const submit = (given: string = value) => {
    if (expired || answer.isPending) return;
    answer.mutate({ run: input.run, id: input.id, value: given });
  };

  return (
    <Paper
      component="section"
      role="region"
      aria-label="A step is waiting for an answer"
      variant="outlined"
      onKeyDown={(e) => {
        if (e.key === "Escape") setOpen(false);
      }}
      sx={{
        // Stays in view as the page scrolls, just under the app bar.
        position: "sticky",
        top: { xs: 60, sm: 68 },
        zIndex: (t) => t.zIndex.appBar - 1,
        mb: 2,
        overflow: "hidden",
        borderColor: "primary.main",
        borderLeftWidth: 4,
      }}
    >
      <ButtonBase
        onClick={() => setOpen((was) => !was)}
        aria-expanded={open}
        sx={{
          display: "block",
          width: "100%",
          textAlign: "left",
          px: 1.5,
          py: 1,
        }}
      >
        <Stack direction="row" spacing={1} alignItems="center">
          <HelpOutlineIcon color="primary" fontSize="small" />
          <Typography
            variant="body2"
            noWrap={!open}
            sx={{ fontWeight: 600, flexGrow: 1, minWidth: 0 }}
          >
            {input.prompt}
          </Typography>
          {queued > 0 && (
            <Chip size="small" variant="outlined" label={`${queued} more`} />
          )}
          {remaining !== null && (
            <Chip
              size="small"
              color={remaining < 30_000 ? "warning" : "default"}
              label={expired ? "timing out…" : clock(remaining)}
              sx={{ fontFamily: monoFontStack }}
            />
          )}
          <Typography
            variant="button"
            color="primary"
            sx={{ display: "inline-flex", alignItems: "center", flexShrink: 0 }}
          >
            {open ? "Later" : "Answer"}
            {open ? (
              <ExpandLessIcon fontSize="small" />
            ) : (
              <ExpandMoreIcon fontSize="small" />
            )}
          </Typography>
        </Stack>
      </ButtonBase>

      <Collapse in={open} unmountOnExit>
        <Box sx={{ px: 1.5, pb: 1.5 }}>
          <Typography variant="body2" color="text.secondary" sx={{ mb: 1.5 }}>
            <Box component="span" sx={{ fontFamily: monoFontStack }}>
              {input.step}
            </Box>{" "}
            in {input.workflow} is waiting for an answer ·{" "}
            <Link to="/run/$runId" params={{ runId: String(input.run) }}>
              run #{input.run}
            </Link>
          </Typography>
          {input.details && (
            <Typography
              variant="body2"
              sx={{ mb: 1.5, whiteSpace: "pre-wrap" }}
            >
              {input.details}
            </Typography>
          )}
          {input.yes_no && (
            <Typography variant="body2" color="text.secondary">
              {[
                input.default === "y" && "No answer in time means yes.",
                input.default === "n" && "No answer in time means no.",
                input.env &&
                  `Set ${input.env} to y or n to skip this question next time.`,
              ]
                .filter(Boolean)
                .join(" ")}
            </Typography>
          )}
          {!input.enter && !input.yes_no && (
            <TextField
              autoFocus
              fullWidth
              size="small"
              type={input.secret ? "password" : "text"}
              value={value}
              onChange={(e) => setValue(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === "Enter") submit();
              }}
              placeholder={
                input.default && !input.secret ? input.default : undefined
              }
              disabled={expired}
              helperText={
                [
                  input.default !== null &&
                    (input.secret
                      ? "Leave blank for the default."
                      : `Leave blank for "${input.default}".`),
                  input.env &&
                    `Set ${input.env} to skip this question next time.`,
                  remaining === null &&
                    "This question waits until it's answered.",
                ]
                  .filter(Boolean)
                  .join(" ") || undefined
              }
              slotProps={{ input: { sx: { fontFamily: monoFontStack } } }}
            />
          )}
          {input.enter && remaining === null && (
            <Typography variant="body2" color="text.secondary">
              The step carries on when you press Continue.
            </Typography>
          )}
          {input.enter && remaining !== null && (
            <Typography variant="body2" color="text.secondary">
              The step carries on when you press Continue, or by itself when the
              time runs out.
            </Typography>
          )}
          {(answer.error || stop.error) && (
            <Typography variant="body2" color="error" sx={{ mt: 1 }}>
              {(answer.error ?? stop.error)?.message}
            </Typography>
          )}
          {cancelling && (
            <Typography variant="body2" color="warning.main" sx={{ mt: 1.5 }}>
              Cancel stops run #{input.run}: the step asking is killed and
              nothing further starts. What has already happened stays happened.
            </Typography>
          )}
          <Stack
            direction="row"
            spacing={1}
            justifyContent="flex-end"
            sx={{ mt: 1.5 }}
          >
            {cancelling ? (
              <>
                <Button onClick={() => setCancelling(false)}>
                  Keep waiting
                </Button>
                <Button
                  color="error"
                  variant="contained"
                  disabled={stop.isPending}
                  onClick={() => stop.mutate(input.run)}
                >
                  Stop the run
                </Button>
              </>
            ) : (
              <>
                <Button
                  color="error"
                  onClick={() => setCancelling(true)}
                  sx={{ mr: "auto" }}
                >
                  Cancel
                </Button>
                {input.yes_no ? (
                  <>
                    <Button
                      variant={input.default === "n" ? "contained" : "outlined"}
                      onClick={() => submit("n")}
                      autoFocus={input.default === "n"}
                      disabled={expired || answer.isPending}
                    >
                      No
                    </Button>
                    <Button
                      variant={input.default === "n" ? "outlined" : "contained"}
                      onClick={() => submit("y")}
                      autoFocus={input.default !== "n"}
                      disabled={expired || answer.isPending}
                    >
                      Yes
                    </Button>
                  </>
                ) : (
                  <Button
                    variant="contained"
                    onClick={() => submit()}
                    autoFocus={input.enter}
                    disabled={
                      expired ||
                      answer.isPending ||
                      (!input.enter && !value && input.default === null)
                    }
                  >
                    {input.enter ? "Continue" : "Answer"}
                  </Button>
                )}
              </>
            )}
          </Stack>
        </Box>
      </Collapse>

      {remaining !== null && total !== null && total > 0 && (
        <LinearProgress
          variant="determinate"
          value={Math.max(0, Math.min(100, (remaining / total) * 100))}
          color={remaining < 30_000 ? "warning" : "primary"}
        />
      )}
    </Paper>
  );
}
