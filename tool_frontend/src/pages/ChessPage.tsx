/**
 * The chess lobby, at `/chess` — deliberately left out of the nav.
 *
 * Open a game with whatever rules you like and anyone on the network running
 * ciabatta sees it here, under your name. The first to join gets the seat,
 * and the game closes to everyone else. You can host and play several games
 * at once; each has its own page at `/chess/<id>`.
 */

import {
  Alert,
  Box,
  Button,
  Card,
  CardContent,
  Chip,
  Collapse,
  Divider,
  FormControl,
  FormControlLabel,
  InputLabel,
  MenuItem,
  Select,
  Stack,
  Switch,
  TextField,
  ToggleButton,
  ToggleButtonGroup,
  Typography,
} from "@mui/material";
import { useQueryClient } from "@tanstack/react-query";
import { useNavigate } from "@tanstack/react-router";
import { useEffect, useState } from "react";

import {
  type ColorChoice,
  type GameView,
  type Lobby,
  type OpenGame,
  type Rules,
  type Variant,
  REASON_LABEL,
  VARIANT_LABEL,
  chessApi,
  chessKeys,
  rulesSummary,
  useLobby,
} from "../api/chess";
import { ErrorNote, Loading, PageHeader } from "../components/Page";

/** Common time controls, as "minutes+increment". */
const PRESETS: { label: string; time: Rules["time"] }[] = [
  { label: "1+0", time: { initial_secs: 60, increment_secs: 0 } },
  { label: "3+2", time: { initial_secs: 180, increment_secs: 2 } },
  { label: "5+0", time: { initial_secs: 300, increment_secs: 0 } },
  { label: "10+5", time: { initial_secs: 600, increment_secs: 5 } },
  { label: "15+10", time: { initial_secs: 900, increment_secs: 10 } },
  { label: "30+0", time: { initial_secs: 1800, increment_secs: 0 } },
  { label: "Unlimited", time: null },
];

export function ChessPage() {
  const lobby = useLobby();

  return (
    <>
      <PageHeader
        title="Chess"
        description="Play anyone on your network who's running ciabatta. Open a game and it shows up on their /chess page under your name; the first to join takes the seat."
      />
      {lobby.isLoading && <Loading label="Looking around the network…" />}
      {lobby.error && <ErrorNote error={lobby.error} />}
      {lobby.data && <LobbyBody lobby={lobby.data} />}
    </>
  );
}

function LobbyBody({ lobby }: { lobby: Lobby }) {
  return (
    <Stack spacing={3} sx={{ maxWidth: 1200 }}>
      <NameCard lobby={lobby} />
      {lobby.discovery_error && (
        <Alert severity="warning" variant="outlined">
          {lobby.discovery_error}
        </Alert>
      )}
      <Box
        sx={{
          display: "grid",
          gap: 3,
          gridTemplateColumns: { xs: "1fr", md: "minmax(320px, 420px) 1fr" },
          alignItems: "start",
        }}
      >
        <NewGameCard />
        <Stack spacing={3}>
          <NearbyCard lobby={lobby} />
          <JoinByAddressCard />
        </Stack>
      </Box>
      <YourGames lobby={lobby} />
      <HowItWorks lobby={lobby} />
    </Stack>
  );
}

function NameCard({ lobby }: { lobby: Lobby }) {
  const queryClient = useQueryClient();
  const [draft, setDraft] = useState(lobby.name);
  const [error, setError] = useState<unknown>(null);
  const [saved, setSaved] = useState(false);
  useEffect(() => setDraft(lobby.name), [lobby.name]);

  const save = async (name: string) => {
    try {
      await chessApi.setName(name);
      setError(null);
      setSaved(true);
      setTimeout(() => setSaved(false), 1500);
      await queryClient.invalidateQueries({ queryKey: chessKeys.lobby });
    } catch (e) {
      setError(e);
    }
  };

  return (
    <Card variant="outlined">
      <CardContent>
        <Stack
          direction={{ xs: "column", sm: "row" }}
          spacing={2}
          alignItems={{ sm: "center" }}
          component="form"
          onSubmit={(e) => {
            e.preventDefault();
            void save(draft);
          }}
        >
          <TextField
            label="Your name"
            size="small"
            value={draft}
            onChange={(e) => setDraft(e.target.value)}
            inputProps={{ maxLength: 40 }}
            helperText={`What other players see. This machine is ${lobby.hostname}.`}
            sx={{ minWidth: 280 }}
          />
          <Stack direction="row" spacing={1}>
            <Button
              type="submit"
              variant="contained"
              disabled={!draft.trim() || draft.trim() === lobby.name}
            >
              {saved ? "Saved" : "Save"}
            </Button>
            {lobby.name !== lobby.hostname && (
              <Button onClick={() => void save(lobby.hostname)}>Use hostname</Button>
            )}
          </Stack>
        </Stack>
        {error !== null && <ErrorNote error={error} />}
      </CardContent>
    </Card>
  );
}

function NewGameCard() {
  const navigate = useNavigate();
  const [variant, setVariant] = useState<Variant>("standard");
  const [preset, setPreset] = useState("10+5");
  const [minutes, setMinutes] = useState(10);
  const [increment, setIncrement] = useState(5);
  const [color, setColor] = useState<ColorChoice>("random");
  const [takebacks, setTakebacks] = useState(true);
  const [drawOffers, setDrawOffers] = useState(true);
  const [error, setError] = useState<unknown>(null);
  const [busy, setBusy] = useState(false);

  const time =
    preset === "custom"
      ? { initial_secs: Math.round(minutes * 60), increment_secs: increment }
      : (PRESETS.find((p) => p.label === preset)?.time ?? null);

  const create = async () => {
    setBusy(true);
    try {
      const game = await chessApi.create({
        variant,
        time,
        host_color: color,
        takebacks,
        draw_offers: drawOffers,
      });
      await navigate({ to: "/chess/$gameId", params: { gameId: game.id } });
    } catch (e) {
      setError(e);
    } finally {
      setBusy(false);
    }
  };

  return (
    <Card variant="outlined">
      <CardContent>
        <Typography variant="h3" sx={{ mb: 2 }}>
          New game
        </Typography>
        <Stack spacing={2.5}>
          <FormControl size="small" fullWidth>
            <InputLabel id="chess-variant">Variant</InputLabel>
            <Select
              labelId="chess-variant"
              label="Variant"
              value={variant}
              onChange={(e) => setVariant(e.target.value as Variant)}
            >
              {(Object.keys(VARIANT_LABEL) as Variant[]).map((v) => (
                <MenuItem key={v} value={v}>
                  {VARIANT_LABEL[v]}
                </MenuItem>
              ))}
            </Select>
          </FormControl>
          <Typography variant="caption" color="text.secondary" sx={{ mt: "6px !important" }}>
            {VARIANT_HINT[variant]}
          </Typography>

          <Box>
            <Typography variant="body2" color="text.secondary" sx={{ mb: 1 }}>
              Time control (minutes + seconds per move)
            </Typography>
            <Stack direction="row" flexWrap="wrap" gap={1}>
              {PRESETS.map((p) => (
                <Chip
                  key={p.label}
                  label={p.label}
                  color={preset === p.label ? "primary" : "default"}
                  variant={preset === p.label ? "filled" : "outlined"}
                  onClick={() => setPreset(p.label)}
                />
              ))}
              <Chip
                label="Custom"
                color={preset === "custom" ? "primary" : "default"}
                variant={preset === "custom" ? "filled" : "outlined"}
                onClick={() => setPreset("custom")}
              />
            </Stack>
            <Collapse in={preset === "custom"}>
              <Stack direction="row" spacing={2} sx={{ mt: 2 }}>
                <TextField
                  label="Minutes"
                  type="number"
                  size="small"
                  value={minutes}
                  onChange={(e) => setMinutes(Math.max(0, Number(e.target.value)))}
                  inputProps={{ min: 0.5, max: 180, step: 0.5 }}
                />
                <TextField
                  label="Increment (s)"
                  type="number"
                  size="small"
                  value={increment}
                  onChange={(e) => setIncrement(Math.max(0, Math.round(Number(e.target.value))))}
                  inputProps={{ min: 0, max: 180 }}
                />
              </Stack>
            </Collapse>
          </Box>

          <Box>
            <Typography variant="body2" color="text.secondary" sx={{ mb: 1 }}>
              You play
            </Typography>
            <ToggleButtonGroup
              size="small"
              exclusive
              value={color}
              onChange={(_, next: ColorChoice | null) => next && setColor(next)}
            >
              <ToggleButton value="white">White</ToggleButton>
              <ToggleButton value="random">Random</ToggleButton>
              <ToggleButton value="black">Black</ToggleButton>
            </ToggleButtonGroup>
          </Box>

          <Stack>
            <FormControlLabel
              control={<Switch checked={takebacks} onChange={(e) => setTakebacks(e.target.checked)} />}
              label="Allow takeback requests"
            />
            <FormControlLabel
              control={<Switch checked={drawOffers} onChange={(e) => setDrawOffers(e.target.checked)} />}
              label="Allow draw offers"
            />
          </Stack>

          <Button variant="contained" size="large" onClick={() => void create()} disabled={busy}>
            Open game
          </Button>
          {error !== null && <ErrorNote error={error} />}
        </Stack>
      </CardContent>
    </Card>
  );
}

const VARIANT_HINT: Record<Variant, string> = {
  standard: "Regular chess.",
  chess960: "The back rank is shuffled — the same for both sides. Castle by dropping the king on its rook.",
  king_of_the_hill: "Also win by walking your king onto d4, e4, d5 or e5.",
  three_check: "Also win by giving check three times.",
};

function OpenGameRow({
  game,
  host,
  address,
}: {
  game: OpenGame;
  host: string;
  address: string;
}) {
  const navigate = useNavigate();
  const [error, setError] = useState<unknown>(null);
  const [busy, setBusy] = useState(false);
  const hostColor = game.rules.host_color;
  const youPlay =
    hostColor === "random" ? "random colour" : hostColor === "white" ? "you play black" : "you play white";

  const join = async () => {
    setBusy(true);
    try {
      const view = await chessApi.join(address, game.id);
      await navigate({ to: "/chess/$gameId", params: { gameId: view.id } });
    } catch (e) {
      setError(e);
      setBusy(false);
    }
  };

  return (
    <Box>
      <Stack direction="row" spacing={2} alignItems="center" justifyContent="space-between">
        <Box sx={{ minWidth: 0 }}>
          <Typography variant="body1" sx={{ fontWeight: 600 }}>
            {host}
          </Typography>
          <Typography variant="body2" color="text.secondary">
            {rulesSummary(game.rules)} · {youPlay}
          </Typography>
        </Box>
        <Button variant="contained" onClick={() => void join()} disabled={busy}>
          Join
        </Button>
      </Stack>
      {error !== null && <ErrorNote error={error} />}
    </Box>
  );
}

function NearbyCard({ lobby }: { lobby: Lobby }) {
  const open = lobby.peers.flatMap((peer) =>
    peer.games.map((game) => ({ peer, game })),
  );
  return (
    <Card variant="outlined">
      <CardContent>
        <Typography variant="h3" sx={{ mb: 2 }}>
          Open games nearby
        </Typography>
        {open.length === 0 ? (
          <Typography variant="body2" color="text.secondary">
            Nobody on the network has a game open right now. This list updates by itself — or open
            one yourself and wait for a challenger.
          </Typography>
        ) : (
          <Stack spacing={2} divider={<Divider flexItem />}>
            {open.map(({ peer, game }) => (
              <OpenGameRow key={game.id} game={game} host={peer.name} address={peer.address} />
            ))}
          </Stack>
        )}
      </CardContent>
    </Card>
  );
}

function JoinByAddressCard() {
  const [address, setAddress] = useState("");
  const [found, setFound] = useState<{ name: string; address: string; games: OpenGame[] } | null>(
    null,
  );
  const [error, setError] = useState<unknown>(null);
  const [busy, setBusy] = useState(false);

  const look = async () => {
    setBusy(true);
    try {
      setFound(await chessApi.browse(address));
      setError(null);
    } catch (e) {
      setError(e);
      setFound(null);
    } finally {
      setBusy(false);
    }
  };

  return (
    <Card variant="outlined">
      <CardContent>
        <Typography variant="h3" sx={{ mb: 1 }}>
          Join by address
        </Typography>
        <Typography variant="body2" color="text.secondary" sx={{ mb: 2 }}>
          If a host doesn&apos;t show up above (some networks block broadcasts), ask them for the
          address on their chess page.
        </Typography>
        <Stack
          direction="row"
          spacing={1}
          component="form"
          onSubmit={(e) => {
            e.preventDefault();
            void look();
          }}
        >
          <TextField
            size="small"
            placeholder="192.168.1.20:47652"
            value={address}
            onChange={(e) => setAddress(e.target.value)}
            sx={{ flex: 1 }}
          />
          <Button type="submit" variant="outlined" disabled={busy || !address.trim()}>
            Look
          </Button>
        </Stack>
        {error !== null && <ErrorNote error={error} />}
        {found && (
          <Stack spacing={2} sx={{ mt: 2 }} divider={<Divider flexItem />}>
            {found.games.length === 0 && (
              <Typography variant="body2" color="text.secondary">
                {found.name} has no open games.
              </Typography>
            )}
            {found.games.map((game) => (
              <OpenGameRow key={game.id} game={game} host={found.name} address={found.address} />
            ))}
          </Stack>
        )}
      </CardContent>
    </Card>
  );
}

function statusLine(game: GameView): string {
  if (game.status === "waiting") return "Waiting for someone to join";
  if (game.status === "finished") {
    const outcome =
      game.winner === null ? "Draw" : game.winner === game.you ? "You won" : "You lost";
    return `${outcome} — ${game.reason ? REASON_LABEL[game.reason] : ""}`;
  }
  return game.turn === game.you ? "Your move" : "Their move";
}

function YourGames({ lobby }: { lobby: Lobby }) {
  const navigate = useNavigate();
  const queryClient = useQueryClient();
  if (lobby.games.length === 0) return null;

  const leave = async (game: GameView) => {
    await chessApi.remove(game.id).catch(() => undefined);
    await queryClient.invalidateQueries({ queryKey: chessKeys.lobby });
  };

  return (
    <Card variant="outlined">
      <CardContent>
        <Typography variant="h3" sx={{ mb: 2 }}>
          Your games
        </Typography>
        <Stack spacing={1.5} divider={<Divider flexItem />}>
          {lobby.games.map((game) => (
            <Stack
              key={game.id}
              direction={{ xs: "column", sm: "row" }}
              spacing={2}
              alignItems={{ sm: "center" }}
              justifyContent="space-between"
            >
              <Box sx={{ minWidth: 0 }}>
                <Stack direction="row" spacing={1} alignItems="center">
                  <Typography variant="body1" sx={{ fontWeight: 600 }}>
                    {game.opponent ? `vs ${game.opponent}` : "Open game"}
                  </Typography>
                  <Chip size="small" label={game.role === "host" ? "Hosting" : "Joined"} />
                  {game.status === "playing" && game.turn === game.you && (
                    <Chip size="small" color="warning" label="Your move" />
                  )}
                </Stack>
                <Typography variant="body2" color="text.secondary">
                  {rulesSummary(game.rules)} · {statusLine(game)}
                  {game.moves.length > 0 && ` · ${game.moves.length} plies`}
                </Typography>
              </Box>
              <Stack direction="row" spacing={1}>
                <Button
                  variant="contained"
                  onClick={() => void navigate({ to: "/chess/$gameId", params: { gameId: game.id } })}
                >
                  Open
                </Button>
                <Button color="inherit" onClick={() => void leave(game)}>
                  {game.status === "playing" ? "Resign & leave" : game.status === "waiting" ? "Cancel" : "Remove"}
                </Button>
              </Stack>
            </Stack>
          ))}
        </Stack>
      </CardContent>
    </Card>
  );
}

function HowItWorks({ lobby }: { lobby: Lobby }) {
  const [open, setOpen] = useState(false);
  return (
    <Box>
      <Button size="small" onClick={() => setOpen(!open)}>
        {open ? "Hide" : "How this works"}
      </Button>
      <Collapse in={open}>
        <Stack spacing={1} sx={{ mt: 1, maxWidth: "75ch" }}>
          <Typography variant="body2">
            Opening a game makes this daemon announce it on the local network (UDP broadcast on port{" "}
            {lobby.discovery_port}) and accept connections on TCP port{" "}
            {lobby.port ?? "— chosen when you open your first game"}. Nothing is announced or listened
            for until this page is used.
          </Typography>
          <Typography variant="body2">
            The first player to join gets a private seat: the host hands them a secret, the game stops
            being announced, and anyone else who tries to join is turned away. If the connection drops,
            the guest&apos;s daemon reconnects with that secret on its own.
          </Typography>
          <Typography variant="body2">
            The host&apos;s daemon keeps the board and the clocks; the clocks start once both sides
            have moved, and a game can be aborted until then. Threefold repetition, the fifty-move rule
            and insufficient material end a game as a draw automatically. Games live in the daemon&apos;s
            memory, so restarting it ends them. Set <code>CIABATTA_CHESS_PORT</code>,{" "}
            <code>CIABATTA_CHESS_DISCOVERY_PORT</code> or <code>CIABATTA_CHESS_BROADCAST</code> (a
            comma-separated list of broadcast addresses) before the daemon starts to change the defaults.
          </Typography>
          <Typography variant="body2">
            Nobody can see your games? A firewall is the usual reason. On Linux with ufw, for
            example: <code>sudo ufw allow {lobby.discovery_port}/udp</code> and{" "}
            <code>sudo ufw allow {lobby.port ?? 47652}/tcp</code>. Joining by address only needs the
            TCP port.
          </Typography>
          <Typography variant="caption" color="text.secondary">
            ciabatta {lobby.version}
          </Typography>
        </Stack>
      </Collapse>
    </Box>
  );
}
