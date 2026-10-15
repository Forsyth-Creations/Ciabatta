/**
 * The dialog a step's `ciabatta input` question appears in.
 *
 * Mounted once, in the app shell, so a question reaches the operator whatever
 * page they are on. It can't be dismissed — no Escape, no backdrop click, no
 * Cancel — because a step is blocked on it: the only ways out are answering,
 * the question timing out, or the run being stopped. Its state lives in the
 * daemon, not here, so reloading the page brings it straight back.
 */

import { useEffect, useMemo, useState } from "react";
import {
  Box,
  Button,
  Chip,
  Dialog,
  DialogActions,
  DialogContent,
  DialogTitle,
  LinearProgress,
  Stack,
  TextField,
  Typography,
} from "@mui/material";
import HelpOutlineIcon from "@mui/icons-material/HelpOutline";
import { Link } from "@tanstack/react-router";

import { useAnswerInput, useWaitingInputs } from "../api/run";
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
    () => [...(data?.inputs ?? [])].sort((a, b) => a.asked_at.localeCompare(b.asked_at)),
    [data],
  );
  const current = waiting[0];
  if (!current) return null;
  // Keyed by question, so the next one starts with an empty field.
  return (
    <Question key={`${current.run}:${current.id}`} input={current} queued={waiting.length - 1} />
  );
}

function Question({ input, queued }: { input: WaitingInput; queued: number }) {
  const [value, setValue] = useState("");
  const answer = useAnswerInput();
  const remaining = useRemaining(input.deadline);
  const total = input.deadline
    ? new Date(input.deadline).getTime() - new Date(input.asked_at).getTime()
    : null;
  const expired = remaining !== null && remaining <= 0;

  const submit = () => {
    if (expired || answer.isPending) return;
    answer.mutate({ run: input.run, id: input.id, value });
  };

  return (
    <Dialog
      open
      fullWidth
      maxWidth="sm"
      disableEscapeKeyDown
      // Deliberately ignores every close request: the step is waiting.
      onClose={() => {}}
      aria-labelledby="input-prompt-title"
    >
      {remaining !== null && total !== null && total > 0 && (
        <LinearProgress
          variant="determinate"
          value={Math.max(0, Math.min(100, (remaining / total) * 100))}
          color={remaining < 30_000 ? "warning" : "primary"}
        />
      )}
      <DialogTitle id="input-prompt-title" sx={{ pb: 1 }}>
        <Stack direction="row" spacing={1} alignItems="center">
          <HelpOutlineIcon color="primary" />
          <Box sx={{ flexGrow: 1 }}>{input.prompt}</Box>
          {remaining !== null && (
            <Chip
              size="small"
              color={remaining < 30_000 ? "warning" : "default"}
              label={expired ? "timing out…" : clock(remaining)}
              sx={{ fontFamily: monoFontStack }}
            />
          )}
        </Stack>
      </DialogTitle>
      <DialogContent>
        <Typography variant="body2" color="text.secondary" sx={{ mb: 2 }}>
          <Box component="span" sx={{ fontFamily: monoFontStack }}>
            {input.step}
          </Box>{" "}
          in {input.workflow} is waiting for an answer ·{" "}
          <Link to="/run/$runId" params={{ runId: String(input.run) }}>
            run #{input.run}
          </Link>
          {queued > 0 && ` · ${queued} more waiting`}
        </Typography>
        {input.details && (
          <Typography variant="body2" sx={{ mb: 2, whiteSpace: "pre-wrap" }}>
            {input.details}
          </Typography>
        )}
        {!input.enter && (
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
            placeholder={input.default && !input.secret ? input.default : undefined}
            disabled={expired}
            helperText={
              [
                input.default !== null &&
                  (input.secret
                    ? "Leave blank for the default."
                    : `Leave blank for "${input.default}".`),
                input.env && `Set ${input.env} to skip this question next time.`,
                remaining === null && "This question waits until it's answered.",
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
            The step carries on when you press Continue, or by itself when the time runs out.
          </Typography>
        )}
        {answer.error && (
          <Typography variant="body2" color="error" sx={{ mt: 1 }}>
            {answer.error.message}
          </Typography>
        )}
      </DialogContent>
      <DialogActions>
        <Button
          variant="contained"
          onClick={submit}
          autoFocus={input.enter}
          disabled={
            expired || answer.isPending || (!input.enter && !value && input.default === null)
          }
        >
          {input.enter ? "Continue" : "Answer"}
        </Button>
      </DialogActions>
    </Dialog>
  );
}
