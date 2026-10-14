import { useState } from "react";
import {
  Alert,
  Button,
  Dialog,
  DialogActions,
  DialogContent,
  DialogContentText,
  DialogTitle,
  TextField,
} from "@mui/material";
import DeleteSweepIcon from "@mui/icons-material/DeleteSweep";

import { api, type Stats } from "../api";
import { human } from "../format";

/** What has to be typed before the button arms. */
const PHRASE = "clear";

/**
 * "Clear all": delete every artifact the cache holds, for when it has been
 * filled with something it shouldn't have.
 *
 * Two guards, because the action can't be undone. The phrase has to be typed —
 * a second click on a dialog is a reflex, typing a word is a decision — and,
 * when the server's config sets a `clear` password, the password too.
 *
 * What it keeps is said up front: projects, counters, charts and build history
 * all survive, so nobody hesitates thinking the hit-rate history goes with it.
 */
export function ClearCache({ stats, onCleared }: { stats: Stats; onCleared: () => void }) {
  const [open, setOpen] = useState(false);
  const [typed, setTyped] = useState("");
  const [password, setPassword] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [done, setDone] = useState<string | null>(null);

  const needsPassword = stats.clear?.password_required ?? false;
  const armed = typed.trim().toLowerCase() === PHRASE && (!needsPassword || password !== "");

  const close = () => {
    setOpen(false);
    setTyped("");
    setPassword("");
    setError(null);
  };

  const clear = async () => {
    setBusy(true);
    try {
      const result = await api<{ entries_removed: number; bytes_freed: number }>(
        "/api/cache/clear",
        {
          method: "POST",
          body: JSON.stringify({ confirm: true, password: needsPassword ? password : undefined }),
        },
      );
      close();
      setDone(
        `Cleared ${result.entries_removed.toLocaleString()} entr${result.entries_removed === 1 ? "y" : "ies"}, ${human(result.bytes_freed)}.`,
      );
      onCleared();
    } catch (e) {
      setError((e as Error).message);
    } finally {
      setBusy(false);
    }
  };

  return (
    <>
      <Button
        color="error"
        variant="outlined"
        size="small"
        startIcon={<DeleteSweepIcon />}
        onClick={() => {
          setDone(null);
          setOpen(true);
        }}
      >
        Clear all
      </Button>
      {done && (
        <Alert severity="success" onClose={() => setDone(null)} sx={{ py: 0 }}>
          {done}
        </Alert>
      )}

      <Dialog open={open} onClose={busy ? undefined : close} maxWidth="xs" fullWidth>
        <DialogTitle>Clear the whole cache?</DialogTitle>
        <DialogContent>
          <DialogContentText sx={{ mb: 1.5 }}>
            Deletes every artifact this server stores —{" "}
            <strong>
              {stats.storage.entries.toLocaleString()} entr
              {stats.storage.entries === 1 ? "y" : "ies"}, {stats.storage.human}
            </strong>
            . The next build of each target rebuilds and uploads it again. This can't be undone.
          </DialogContentText>
          <DialogContentText sx={{ mb: 2 }}>
            Projects, statistics, charts and build history are kept.
          </DialogContentText>
          <TextField
            autoFocus
            fullWidth
            size="small"
            label={`Type "${PHRASE}" to confirm`}
            value={typed}
            onChange={(e) => setTyped(e.target.value)}
            disabled={busy}
          />
          {needsPassword && (
            <TextField
              fullWidth
              size="small"
              type="password"
              label="Clear password"
              helperText="Set under `clear` in the server's config."
              value={password}
              onChange={(e) => setPassword(e.target.value)}
              onKeyDown={(e) => e.key === "Enter" && armed && !busy && clear()}
              disabled={busy}
              sx={{ mt: 2 }}
            />
          )}
          {error && (
            <Alert severity="error" sx={{ mt: 2 }}>
              {error}
            </Alert>
          )}
        </DialogContent>
        <DialogActions>
          <Button onClick={close} disabled={busy}>
            Cancel
          </Button>
          <Button color="error" variant="contained" disabled={!armed || busy} onClick={clear}>
            Clear all
          </Button>
        </DialogActions>
      </Dialog>
    </>
  );
}
