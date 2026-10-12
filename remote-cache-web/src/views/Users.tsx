import { useState } from "react";
import {
  Alert,
  Box,
  Button,
  Card,
  CardContent,
  Checkbox,
  Chip,
  Dialog,
  DialogActions,
  DialogContent,
  DialogContentText,
  DialogTitle,
  FormControlLabel,
  LinearProgress,
  Table,
  TableBody,
  TableCell,
  TableContainer,
  TableHead,
  TableRow,
  TextField,
  Typography,
} from "@mui/material";

import { api, type CreatedUser, type Users as UsersData } from "../api";
import { EmptyRow, Mono, Prose, Section } from "../components/ui";
import { monoFontStack } from "../theme";
import { useLoad } from "../useLoad";

export function Users({ refreshKey }: { refreshKey: number }) {
  const { data, error, loading, reload } = useLoad(
    () => api<UsersData>("/api/users"),
    refreshKey,
  );
  const [name, setName] = useState("");
  const [readOnly, setReadOnly] = useState(false);
  const [admin, setAdmin] = useState(false);
  const [created, setCreated] = useState<CreatedUser | null>(null);
  const [revoking, setRevoking] = useState<string | null>(null);
  const [actionError, setActionError] = useState<string | null>(null);

  if (error) return <Alert severity="error" sx={{ mt: 3 }}>{error}</Alert>;
  if (!data) return loading ? <LinearProgress sx={{ mt: 3 }} /> : null;

  const open = data.mode === "open" || data.mode === "none";
  const sub = data.locked_out
    ? "This cache requires credentials but has none, so nobody can sign in. Add a user with `admin: true` under `auth.users` in the server's config and restart."
    : open
      ? "This cache is in `open` mode: anyone who can reach it can read, write, and create users. Create the credentials you want, then set `auth.mode: token` in the server's config and restart — from then on only these will work."
      : "Credentials for this cache. Tokens are shown once, when they're created, and only their hashes are kept.";

  return (
    <Section title="Users" sub={<Prose text={sub} />}>
      {actionError && (
        <Alert severity="error" sx={{ mb: 2 }} onClose={() => setActionError(null)}>
          {actionError}
        </Alert>
      )}

      {created && (
        // The one moment this value exists in readable form, so give it room.
        <Alert severity="success" variant="outlined" sx={{ mb: 2 }} onClose={() => setCreated(null)}>
          <Typography sx={{ fontWeight: 600 }}>Token for {created.user.name}</Typography>
          <Box
            component="code"
            sx={{
              display: "block",
              my: 1,
              p: 1.25,
              bgcolor: "action.hover",
              borderRadius: 1,
              fontFamily: monoFontStack,
              wordBreak: "break-all",
              userSelect: "all",
            }}
          >
            {created.token}
          </Box>
          <Typography variant="body2" color="text.secondary">
            {created.note}
          </Typography>
          <Typography variant="body2" color="text.secondary" sx={{ mt: 1 }}>
            <Mono>{created.login}</Mono>
          </Typography>
        </Alert>
      )}

      <Card sx={{ mb: 2 }}>
        <CardContent>
          <Box
            component="form"
            sx={{ display: "flex", flexWrap: "wrap", gap: 2, alignItems: "center" }}
            onSubmit={async (event) => {
              event.preventDefault();
              try {
                const result = await api<CreatedUser>("/api/users", {
                  method: "POST",
                  body: JSON.stringify({ name, read_only: readOnly, admin }),
                });
                setCreated(result);
                setActionError(null);
                setName("");
                setReadOnly(false);
                setAdmin(false);
                reload();
              } catch (e) {
                setActionError((e as Error).message);
              }
            }}
          >
            <TextField
              size="small"
              label="Username"
              required
              value={name}
              onChange={(e) => setName(e.target.value)}
            />
            <FormControlLabel
              control={<Checkbox checked={readOnly} onChange={(e) => setReadOnly(e.target.checked)} />}
              label="read-only"
            />
            {/* An admin can only be granted by the operator's config on an open
                server — so don't offer a checkbox that will be refused. */}
            {!open && (
              <FormControlLabel
                control={<Checkbox checked={admin} onChange={(e) => setAdmin(e.target.checked)} />}
                label="admin"
              />
            )}
            <Button type="submit" variant="contained">
              Create user
            </Button>
          </Box>
        </CardContent>
      </Card>

      <TableContainer component={Card}>
        <Table size="small">
          <TableHead>
            <TableRow>
              <TableCell>Name</TableCell>
              <TableCell>Access</TableCell>
              <TableCell>Created</TableCell>
              <TableCell />
            </TableRow>
          </TableHead>
          <TableBody>
            {data.users.length === 0 && <EmptyRow colSpan={4}>No users yet.</EmptyRow>}
            {data.users.map((u) => (
              <TableRow key={u.name} hover>
                <TableCell>
                  <Mono>{u.name}</Mono>
                  {u.from_config && (
                    <Chip
                      size="small"
                      variant="outlined"
                      label="config"
                      title="Declared in the server's config file"
                      sx={{ ml: 1, height: 20 }}
                    />
                  )}
                </TableCell>
                <TableCell>{u.admin ? "admin" : u.read_only ? "read-only" : "read/write"}</TableCell>
                <TableCell sx={{ color: "text.secondary" }}>
                  {u.created_at ? new Date(u.created_at).toLocaleString() : "—"}
                </TableCell>
                <TableCell align="right">
                  {u.from_config ? (
                    <Typography
                      component="span"
                      color="text.secondary"
                      title="Remove it from the server's config instead"
                    >
                      —
                    </Typography>
                  ) : (
                    <Button size="small" color="error" onClick={() => setRevoking(u.name)}>
                      Revoke
                    </Button>
                  )}
                </TableCell>
              </TableRow>
            ))}
          </TableBody>
        </Table>
      </TableContainer>

      <Dialog open={revoking !== null} onClose={() => setRevoking(null)}>
        <DialogTitle>Revoke {revoking}?</DialogTitle>
        <DialogContent>
          <DialogContentText>Anything using its token stops working.</DialogContentText>
        </DialogContent>
        <DialogActions>
          <Button onClick={() => setRevoking(null)}>Cancel</Button>
          <Button
            color="error"
            variant="contained"
            onClick={async () => {
              const who = revoking!;
              setRevoking(null);
              try {
                await api(`/api/users/${encodeURIComponent(who)}`, { method: "DELETE" });
                setActionError(null);
                reload();
              } catch (e) {
                setActionError((e as Error).message);
              }
            }}
          >
            Revoke
          </Button>
        </DialogActions>
      </Dialog>
    </Section>
  );
}
