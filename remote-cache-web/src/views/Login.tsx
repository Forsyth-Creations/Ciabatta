import { useState } from "react";
import { Alert, Box, Button, Card, CardContent, TextField, Typography } from "@mui/material";

import { api, login, type Users } from "../api";
import { useLoad } from "../useLoad";

export function Login({ onSignedIn }: { onSignedIn: () => void }) {
  const [username, setUsername] = useState("");
  const [password, setPassword] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  // Even signed out, say whether the server has anybody to sign in as. Needs a
  // session on most servers, so a failure here just means "don't know".
  const users = useLoad(() => api<Users>("/api/users"), null);

  return (
    <Box sx={{ maxWidth: 440, mx: "auto", mt: 8 }}>
      <Card>
        <CardContent sx={{ p: 3 }}>
          <Typography variant="h2" gutterBottom>
            Sign in
          </Typography>
          <Typography variant="body2" color="text.secondary" sx={{ mb: 2 }}>
            This cache authenticates. Sign in to see its activity and manage users.
          </Typography>
          {users.data?.locked_out && (
            <Alert severity="warning" sx={{ mb: 2 }}>
              This cache requires credentials but has none, so nobody can sign in. Add a user with{" "}
              <code>admin: true</code> under <code>auth.users</code> in the server's config and
              restart.
            </Alert>
          )}
          {error && (
            <Alert severity="error" sx={{ mb: 2 }}>
              {error}
            </Alert>
          )}
          <Box
            component="form"
            sx={{ display: "grid", gap: 2 }}
            onSubmit={async (event) => {
              event.preventDefault();
              setBusy(true);
              try {
                await login(username, password);
                setError(null);
                onSignedIn();
              } catch (e) {
                setError((e as Error).message || "Sign-in failed");
              } finally {
                setBusy(false);
              }
            }}
          >
            <TextField
              label="Username"
              autoComplete="username"
              value={username}
              onChange={(e) => setUsername(e.target.value)}
              size="small"
              autoFocus
            />
            <TextField
              label="Token"
              type="password"
              autoComplete="current-password"
              value={password}
              onChange={(e) => setPassword(e.target.value)}
              size="small"
            />
            <Button type="submit" variant="contained" disabled={busy}>
              Sign in
            </Button>
          </Box>
        </CardContent>
      </Card>
    </Box>
  );
}
