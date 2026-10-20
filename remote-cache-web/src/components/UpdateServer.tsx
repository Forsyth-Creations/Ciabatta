import { useEffect, useState } from "react";
import {
  Alert,
  Box,
  Button,
  Checkbox,
  Dialog,
  DialogActions,
  DialogContent,
  DialogContentText,
  DialogTitle,
  FormControlLabel,
  MenuItem,
  Stack,
  TextField,
  Typography,
} from "@mui/material";
import SystemUpdateAltIcon from "@mui/icons-material/SystemUpdateAlt";

import { api, uploadBinary, type Release, type Stats } from "../api";
import { human } from "../format";

const PLATFORMS = ["linux", "macos", "windows"];

/**
 * Upload a new ciabatta: for this cache to hand out — clients on that platform
 * are told a newer build is available and `ciabatta self update` installs it —
 * and, optionally, to run itself, restarting in place.
 *
 * Only when the server's config sets an `update` password. Without one the
 * server refuses every upload, so the panel says how to turn it on instead of
 * offering a button that can only fail.
 */
export function UpdateServer({ stats, onUpdated }: { stats: Stats; onUpdated: () => void }) {
  const enabled = stats.update?.enabled ?? false;
  const here = stats.update?.platform ?? "linux";
  const [release, setRelease] = useState<Release | null>(null);
  const [open, setOpen] = useState(false);
  const [file, setFile] = useState<File | null>(null);
  const [platform, setPlatform] = useState(here);
  const [version, setVersion] = useState("");
  const [apply, setApply] = useState(false);
  const [password, setPassword] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [done, setDone] = useState<string | null>(null);

  const load = () =>
    api<Release>("/api/release")
      .then(setRelease)
      .catch(() => setRelease(null));
  useEffect(() => {
    load();
  }, []);

  const close = () => {
    setOpen(false);
    setFile(null);
    setVersion("");
    setApply(false);
    setPassword("");
    setError(null);
  };

  const upload = async () => {
    if (!file) return;
    setBusy(true);
    setError(null);
    try {
      // When the server last started, to tell when it has restarted.
      const before = (await api<{ started_at: string }>("/api/health")).started_at;
      const result = await uploadBinary(platform, file, password, {
        version: platform === here ? undefined : version || undefined,
        apply: apply && platform === here,
      });
      const restarting = result.applied;
      close();
      setDone(
        `This cache now hands out ciabatta ${result.version ?? "(version unchanged)"} for ${platform}.` +
          (restarting ? " It is restarting on it — the page reconnects in a few seconds." : ""),
      );
      if (restarting) {
        // Wait for it to come back — a new start time — before reloading
        // what it says.
        for (let tries = 0; tries < 60; tries++) {
          await new Promise((resolve) => setTimeout(resolve, 1000));
          try {
            const health = await api<{ started_at: string }>("/api/health");
            if (health.started_at !== before) break;
          } catch {
            /* still restarting */
          }
        }
      }
      load();
      onUpdated();
    } catch (e) {
      setError((e as Error).message);
    } finally {
      setBusy(false);
    }
  };

  const builds = Object.entries(release?.builds ?? {});
  return (
    <Box>
      <Stack direction={{ xs: "column", sm: "row" }} spacing={2} alignItems={{ sm: "center" }}>
        <Typography variant="body2" color="text.secondary" sx={{ flexGrow: 1 }}>
          {builds.length === 0
            ? "This cache hands out no ciabatta builds yet."
            : "Handing out ciabatta " +
              builds
                .map(([p, b]) => `${b.version ?? release?.version} for ${p} (${human(b.size)})`)
                .join(", ") +
              ". Clients on an older build are told, and `ciabatta self update` installs it."}
        </Typography>
        <Button
          variant="outlined"
          size="small"
          startIcon={<SystemUpdateAltIcon />}
          disabled={!enabled}
          onClick={() => {
            setDone(null);
            setOpen(true);
          }}
        >
          Upload a new ciabatta
        </Button>
      </Stack>
      {!enabled && (
        <Alert severity="info" sx={{ mt: 1.5 }}>
          Uploading is off: it needs an <code>update</code> password in the server's config (
          <code>update: {"{"} password_sha256: … {"}"}</code> or <code>password_env</code>).
        </Alert>
      )}
      {done && (
        <Alert severity="success" onClose={() => setDone(null)} sx={{ mt: 1.5 }}>
          {done}
        </Alert>
      )}

      <Dialog open={open} onClose={busy ? undefined : close} maxWidth="xs" fullWidth>
        <DialogTitle>Upload a new ciabatta</DialogTitle>
        <DialogContent>
          <DialogContentText sx={{ mb: 2 }}>
            The executable itself, not the release archive. Clients on its platform are offered it
            from then on.
          </DialogContentText>
          <Button variant="outlined" component="label" size="small" disabled={busy}>
            {file ? `${file.name} (${human(file.size)})` : "Choose the binary…"}
            <input hidden type="file" onChange={(e) => setFile(e.target.files?.[0] ?? null)} />
          </Button>
          <TextField
            select
            fullWidth
            size="small"
            label="Platform"
            value={platform}
            onChange={(e) => setPlatform(e.target.value)}
            disabled={busy}
            sx={{ mt: 2 }}
          >
            {PLATFORMS.map((p) => (
              <MenuItem key={p} value={p}>
                {p}
                {p === here ? " — this server's" : ""}
              </MenuItem>
            ))}
          </TextField>
          {platform !== here && (
            <TextField
              fullWidth
              size="small"
              label="Version"
              helperText="The server can't run this platform's binary to ask it."
              value={version}
              onChange={(e) => setVersion(e.target.value)}
              disabled={busy}
              sx={{ mt: 2 }}
            />
          )}
          <FormControlLabel
            sx={{ mt: 1 }}
            control={
              <Checkbox
                checked={apply && platform === here}
                disabled={busy || platform !== here}
                onChange={(e) => setApply(e.target.checked)}
              />
            }
            label="Run it on this server too (restarts it; the old binary is kept as .previous)"
          />
          <TextField
            fullWidth
            size="small"
            type="password"
            label="Update password"
            helperText="Set under `update` in the server's config."
            value={password}
            onChange={(e) => setPassword(e.target.value)}
            disabled={busy}
            sx={{ mt: 1 }}
          />
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
          <Button variant="contained" disabled={!file || !password || busy} onClick={upload}>
            {busy ? "Uploading…" : "Upload"}
          </Button>
        </DialogActions>
      </Dialog>
    </Box>
  );
}
