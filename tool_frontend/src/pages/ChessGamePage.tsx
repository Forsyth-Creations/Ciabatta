/**
 * One game, at `/chess/<id>`: the board, both clocks, the moves so far, and
 * whatever the two players have on the table (draw, takeback, rematch).
 */

import {
  Alert,
  Box,
  Button,
  Chip,
  Dialog,
  DialogActions,
  DialogContent,
  Paper,
  Stack,
  Tooltip,
  Typography,
} from "@mui/material";
import ArrowBackIcon from "@mui/icons-material/ArrowBack";
import BlockIcon from "@mui/icons-material/Block";
import EmojiEventsIcon from "@mui/icons-material/EmojiEvents";
import SentimentDissatisfiedIcon from "@mui/icons-material/SentimentDissatisfied";
import FlagIcon from "@mui/icons-material/Flag";
import HandshakeIcon from "@mui/icons-material/Handshake";
import SwapVertIcon from "@mui/icons-material/SwapVert";
import UndoIcon from "@mui/icons-material/Undo";
import ReplayIcon from "@mui/icons-material/Replay";
import { Link, useNavigate, useParams } from "@tanstack/react-router";
import { useEffect, useRef, useState } from "react";

import {
  type Action,
  type Color,
  type GameView,
  type Kind,
  REASON_LABEL,
  chessApi,
  rulesSummary,
  useGame,
  useLobby,
} from "../api/chess";
import { ChessBoard } from "../components/ChessBoard";
import { BOARD_THEMES, type BoardTheme, useBoardTheme } from "../components/ChessPieces";
import { ErrorNote, Loading } from "../components/Page";

export function ChessGamePage() {
  const { gameId } = useParams({ from: "/chess/$gameId" });
  const { view, receivedAt, error, accept } = useGame(gameId);

  if (!view) {
    return error ? (
      <Stack spacing={2}>
        <ErrorNote error={error} />
        <Link to="/chess">Back to the lobby</Link>
      </Stack>
    ) : (
      <Loading label="Loading the game…" />
    );
  }
  return <Game view={view} receivedAt={receivedAt} accept={accept} />;
}

/** Track the window so the board can take whatever room there is. */
function useBoardSize(): number {
  const measure = () => {
    const wide = window.innerWidth >= 1000;
    const across = wide ? window.innerWidth - 232 - 420 : window.innerWidth - 48;
    const down = window.innerHeight - 200;
    return Math.max(280, Math.min(720, across, wide ? down : 9999));
  };
  const [size, setSize] = useState(measure);
  useEffect(() => {
    const onResize = () => setSize(measure());
    window.addEventListener("resize", onResize);
    return () => window.removeEventListener("resize", onResize);
  }, []);
  return Math.floor(size / 8) * 8;
}

/** Re-render every `ms` while `active`, for the running clock. */
function useTicker(active: boolean, ms = 100) {
  const [, setTick] = useState(0);
  useEffect(() => {
    if (!active) return;
    const id = setInterval(() => setTick((t) => t + 1), ms);
    return () => clearInterval(id);
  }, [active, ms]);
}

function formatClock(ms: number): string {
  const total = Math.max(0, ms);
  const minutes = Math.floor(total / 60_000);
  const seconds = Math.floor((total % 60_000) / 1000);
  if (total < 10_000) {
    const tenths = Math.floor((total % 1000) / 100);
    return `${minutes}:${String(seconds).padStart(2, "0")}.${tenths}`;
  }
  return `${minutes}:${String(seconds).padStart(2, "0")}`;
}

function Game({
  view,
  receivedAt,
  accept,
}: {
  view: GameView;
  receivedAt: number;
  accept: (next: GameView) => void;
}) {
  const navigate = useNavigate();
  const lobby = useLobby();
  const size = useBoardSize();
  const [flipped, setFlipped] = useState(false);
  const [boardTheme, setBoardTheme] = useBoardTheme();
  const [error, setError] = useState<unknown>(null);
  const movesEnd = useRef<HTMLDivElement>(null);

  const you = view.you;
  const playing = view.status === "playing";
  const running = playing ? view.clock?.running ?? null : null;
  useTicker(running !== null);

  useEffect(() => {
    movesEnd.current?.scrollIntoView({ block: "nearest" });
  }, [view.moves.length]);

  // Your own colour at the bottom; the host of a game nobody has joined yet
  // sees it from white's side.
  const base: Color = you ?? "white";
  const orientation: Color = flipped ? (base === "white" ? "black" : "white") : base;
  const top: Color = orientation === "white" ? "black" : "white";
  const bottom: Color = orientation;

  const act = async (action: Action) => {
    try {
      accept(await chessApi.act(view.id, action));
      setError(null);
    } catch (e) {
      setError(e);
    }
  };

  const onMove = (from: string, to: string, promotion?: Kind) =>
    void act({ type: "move", from, to, promotion });

  const nameOf = (color: Color) => {
    if (!view.host_color) return color === base ? view.host_name : "Waiting for an opponent…";
    const hostSide = view.host_color === color;
    return hostSide ? view.host_name : (view.guest_name ?? "Guest");
  };

  const clockOf = (color: Color) => {
    if (!view.clock) return null;
    let ms = color === "white" ? view.clock.white_ms : view.clock.black_ms;
    if (running === color) ms -= performance.now() - receivedAt;
    return Math.max(0, ms);
  };

  const opponent: Color | null = you ? (you === "white" ? "black" : "white") : null;
  const offered = (key: "draw" | "takeback" | "rematch") => view.offers[key];
  const canMove = playing && !!you && view.turn === you && !view.closed;
  const myAddress = lobby.data?.address;

  const leave = async () => {
    await chessApi.remove(view.id).catch(() => undefined);
    await navigate({ to: "/chess" });
  };

  return (
    <Stack spacing={2}>
      <GameOverDialog
        view={view}
        nameOf={nameOf}
        act={act}
        toLobby={() => void navigate({ to: "/chess" })}
      />
      <Stack direction="row" alignItems="center" spacing={2} flexWrap="wrap" useFlexGap>
        <Button component={Link} to="/chess" size="small" startIcon={<ArrowBackIcon />}>
          Lobby
        </Button>
        <Typography variant="h2">
          {view.host_name} vs {view.guest_name ?? "…"}
        </Typography>
        <Chip size="small" label={rulesSummary(view.rules)} />
        {view.role === "guest" && !view.connected && !view.closed && (
          <Chip size="small" color="warning" label="Reconnecting to the host…" />
        )}
        {view.role === "host" && view.guest_name && !view.connected && (
          <Chip size="small" color="warning" label={`${view.guest_name} is disconnected`} />
        )}
      </Stack>

      {view.status === "waiting" && (
        <Alert severity="info">
          Waiting for someone to join. This game is listed on the chess page of everyone on your
          network running ciabatta
          {myAddress ? (
            <>
              {" "}
              — or they can join by address at <strong>{myAddress}</strong>
            </>
          ) : null}
          .
        </Alert>
      )}
      {view.closed && <Alert severity="warning">{view.closed}</Alert>}

      <Box
        sx={{
          display: "flex",
          gap: 3,
          flexDirection: { xs: "column", lg: "row" },
          alignItems: { xs: "center", lg: "flex-start" },
        }}
      >
        <Stack spacing={1} sx={{ width: size }}>
          <PlayerBar
            name={nameOf(top)}
            color={top}
            clock={clockOf(top)}
            active={running === top || (playing && view.turn === top && !view.clock)}
            checks={view.rules.variant === "three_check" ? view.checks[top] : null}
          />
          <ChessBoard
            view={view}
            orientation={orientation}
            canMove={canMove}
            onMove={onMove}
            size={size}
            colors={boardTheme}
          />
          <PlayerBar
            name={nameOf(bottom)}
            color={bottom}
            clock={clockOf(bottom)}
            active={running === bottom || (playing && view.turn === bottom && !view.clock)}
            checks={view.rules.variant === "three_check" ? view.checks[bottom] : null}
            you={bottom === you}
          />
        </Stack>

        <Stack spacing={2} sx={{ width: { xs: size, lg: 360 }, flexShrink: 0 }}>
          <StatusPanel view={view} />

          {error !== null && <ErrorNote error={error} />}

          {/* What the opponent is asking for. */}
          {playing && opponent && offered("draw") === opponent && (
            <Offer
              text={`${nameOf(opponent)} offers a draw.`}
              onAccept={() => void act({ type: "accept_draw" })}
              onDecline={() => void act({ type: "decline_draw" })}
            />
          )}
          {playing && opponent && offered("takeback") === opponent && (
            <Offer
              text={`${nameOf(opponent)} asks to take back their last move.`}
              onAccept={() => void act({ type: "accept_takeback" })}
              onDecline={() => void act({ type: "decline_takeback" })}
            />
          )}
          {view.status === "finished" && opponent && offered("rematch") === opponent && (
            <Offer
              text={`${nameOf(opponent)} wants a rematch.`}
              onAccept={() => void act({ type: "accept_rematch" })}
              onDecline={() => void act({ type: "decline_rematch" })}
            />
          )}
          {you && offered("draw") === you && playing && (
            <Typography variant="body2" color="text.secondary">
              You offered a draw.
            </Typography>
          )}
          {you && offered("takeback") === you && playing && (
            <Typography variant="body2" color="text.secondary">
              You asked for a takeback.
            </Typography>
          )}
          {you && offered("rematch") === you && (
            <Typography variant="body2" color="text.secondary">
              You offered a rematch.
            </Typography>
          )}

          <ThemePicker current={boardTheme} onChoose={setBoardTheme} />

          <Paper variant="outlined" sx={{ p: 1.5 }}>
            <Typography variant="overline" color="text.secondary">
              Moves
            </Typography>
            <MoveList moves={view.moves} endRef={movesEnd} />
          </Paper>

          <Stack direction="row" flexWrap="wrap" gap={1}>
            {playing && you && view.moves.length < 2 && (
              <Button variant="outlined" onClick={() => void act({ type: "abort" })}>
                Abort
              </Button>
            )}
            {playing && you && (
              <Button
                variant="outlined"
                color="error"
                startIcon={<FlagIcon />}
                onClick={() => void act({ type: "resign" })}
              >
                Resign
              </Button>
            )}
            {playing && you && view.rules.draw_offers && offered("draw") !== you && (
              <Button
                variant="outlined"
                startIcon={<HandshakeIcon />}
                onClick={() => void act({ type: "offer_draw" })}
              >
                Offer draw
              </Button>
            )}
            {playing && you && view.rules.takebacks && view.moves.length > 0 && offered("takeback") !== you && (
              <Button
                variant="outlined"
                startIcon={<UndoIcon />}
                onClick={() => void act({ type: "request_takeback" })}
              >
                Takeback
              </Button>
            )}
            {view.status === "finished" && you && !view.closed && offered("rematch") !== you && (
              <Button
                variant="contained"
                startIcon={<ReplayIcon />}
                onClick={() => void act({ type: "offer_rematch" })}
              >
                Rematch
              </Button>
            )}
            <Tooltip title="Flip the board">
              <Button variant="text" startIcon={<SwapVertIcon />} onClick={() => setFlipped(!flipped)}>
                Flip
              </Button>
            </Tooltip>
            <Button color="inherit" onClick={() => void leave()}>
              {playing ? "Resign & leave" : view.status === "waiting" ? "Cancel game" : "Leave"}
            </Button>
          </Stack>
        </Stack>
      </Box>
    </Stack>
  );
}

function PlayerBar({
  name,
  color,
  clock,
  active,
  checks,
  you = false,
}: {
  name: string;
  color: Color;
  clock: number | null;
  active: boolean;
  checks: number | null;
  you?: boolean;
}) {
  const low = clock !== null && clock < 20_000;
  return (
    <Stack direction="row" alignItems="center" justifyContent="space-between" spacing={1}>
      <Stack direction="row" alignItems="center" spacing={1} sx={{ minWidth: 0 }}>
        <Box
          sx={{
            width: 14,
            height: 14,
            borderRadius: "50%",
            bgcolor: color === "white" ? "#FAFAFA" : "#161616",
            border: "1px solid",
            borderColor: "divider",
            flexShrink: 0,
          }}
        />
        <Typography variant="body1" sx={{ fontWeight: 600 }} noWrap>
          {name}
        </Typography>
        {you && <Chip size="small" label="You" />}
        {checks !== null && <Chip size="small" variant="outlined" label={`${checks}/3 checks`} />}
      </Stack>
      {clock !== null && (
        <Paper
          variant="outlined"
          sx={{
            px: 1.5,
            py: 0.25,
            fontFamily: "monospace",
            fontSize: 22,
            fontWeight: 600,
            minWidth: 96,
            textAlign: "right",
            bgcolor: active ? (low ? "error.main" : "primary.main") : undefined,
            color: active ? "common.white" : "text.secondary",
            transition: "background-color 120ms",
          }}
        >
          {formatClock(clock)}
        </Paper>
      )}
    </Stack>
  );
}

/**
 * The end of a game, impossible to miss: a large dialog naming the result and
 * how it came about, with a rematch one click away. Dismissing it leaves the
 * final position on the board to look at; it comes back for the next ending,
 * including the end of a rematch.
 */
function GameOverDialog({
  view,
  nameOf,
  act,
  toLobby,
}: {
  view: GameView;
  nameOf: (color: Color) => string;
  act: (action: Action) => Promise<void>;
  toLobby: () => void;
}) {
  const finished = view.status === "finished";
  // Which ending this is, so dismissing one doesn't hide the next.
  const ending = finished
    ? `${view.id}:${view.host_color}:${view.moves.length}:${view.reason}:${view.result}`
    : null;
  const [dismissed, setDismissed] = useState<string | null>(null);
  // A new game (a rematch) forgets the last dismissal, so an ending that
  // happens to look identical to a dismissed one still gets its dialog.
  useEffect(() => {
    if (!finished) setDismissed(null);
  }, [finished]);
  if (!finished || ending === dismissed) return null;

  const you = view.you;
  const opponent: Color | null = you ? (you === "white" ? "black" : "white") : null;
  const winner = view.winner;
  const loser: Color | null = winner ? (winner === "white" ? "black" : "white") : null;
  const won = !!you && winner === you;
  const lost = !!you && !!winner && winner !== you;
  const aborted = view.reason === "aborted";

  const title = aborted
    ? "Game aborted"
    : winner === null
      ? "Draw"
      : won
        ? "You won!"
        : lost
          ? "You lost"
          : `${nameOf(winner)} wins`;

  let how = view.reason ? REASON_LABEL[view.reason] : "";
  if (loser && view.reason === "resignation") how = `${nameOf(loser)} resigned`;
  if (loser && view.reason === "timeout") how = `${nameOf(loser)} ran out of time`;
  if (view.reason === "checkmate" && winner) how = `Checkmate by ${nameOf(winner)}`;
  if (view.reason === "king_of_the_hill" && winner) how = `${nameOf(winner)}'s king reached the hill`;
  if (view.reason === "three_checks" && winner) how = `${nameOf(winner)} gave three checks`;

  const accent = won ? "success.main" : lost ? "error.main" : aborted ? "text.secondary" : "info.main";
  const Icon = won ? EmojiEventsIcon : lost ? SentimentDissatisfiedIcon : aborted ? BlockIcon : HandshakeIcon;

  const theyOffered = !!opponent && view.offers.rematch === opponent;
  const youOffered = !!you && view.offers.rematch === you;
  const canRematch = !!you && !view.closed;

  return (
    <Dialog
      open
      onClose={() => setDismissed(ending)}
      maxWidth="sm"
      fullWidth
      PaperProps={{ sx: { borderTop: 8, borderColor: accent, overflow: "hidden" } }}
    >
      <DialogContent sx={{ textAlign: "center", pt: 5, pb: 3 }}>
        <Icon sx={{ fontSize: 96, color: accent }} />
        <Typography
          variant="h1"
          sx={{ fontSize: { xs: 44, sm: 64 }, lineHeight: 1.05, mt: 1, color: accent }}
        >
          {title}
        </Typography>
        <Typography variant="h3" sx={{ mt: 2, fontWeight: 500 }}>
          {how}
        </Typography>
        {view.result && !aborted && (
          <Typography
            sx={{ mt: 2, fontFamily: "monospace", fontSize: 28, fontWeight: 600 }}
            color="text.secondary"
          >
            {view.result}
          </Typography>
        )}
        <Typography variant="body2" color="text.secondary" sx={{ mt: 1 }}>
          {nameOf("white")} (white) vs {nameOf("black")} (black) · {view.moves.length} plies
        </Typography>

        {theyOffered && (
          <Alert severity="info" sx={{ mt: 3, textAlign: "left" }}>
            {opponent && nameOf(opponent)} wants a rematch.
          </Alert>
        )}
        {youOffered && (
          <Typography variant="body2" color="text.secondary" sx={{ mt: 3 }}>
            Rematch offered — waiting for {opponent ? nameOf(opponent) : "your opponent"}…
          </Typography>
        )}
        {view.closed && (
          <Alert severity="warning" sx={{ mt: 3, textAlign: "left" }}>
            {view.closed}
          </Alert>
        )}
      </DialogContent>
      <DialogActions sx={{ justifyContent: "center", gap: 1, pb: 3, flexWrap: "wrap" }}>
        {canRematch && theyOffered && (
          <>
            <Button
              size="large"
              variant="contained"
              startIcon={<ReplayIcon />}
              onClick={() => void act({ type: "accept_rematch" })}
            >
              Accept rematch
            </Button>
            <Button size="large" onClick={() => void act({ type: "decline_rematch" })}>
              Decline
            </Button>
          </>
        )}
        {canRematch && !theyOffered && (
          <Button
            size="large"
            variant="contained"
            startIcon={<ReplayIcon />}
            disabled={youOffered}
            onClick={() => void act({ type: "offer_rematch" })}
          >
            {youOffered ? "Rematch offered" : "Rematch"}
          </Button>
        )}
        <Button size="large" variant="outlined" onClick={() => setDismissed(ending)}>
          View board
        </Button>
        <Button size="large" color="inherit" onClick={toLobby}>
          Back to lobby
        </Button>
      </DialogActions>
    </Dialog>
  );
}

/** Swatches for the board colours; the choice is remembered in this browser. */
function ThemePicker({
  current,
  onChoose,
}: {
  current: BoardTheme;
  onChoose: (id: string) => void;
}) {
  return (
    <Stack direction="row" alignItems="center" spacing={1} flexWrap="wrap" useFlexGap>
      <Typography variant="overline" color="text.secondary" sx={{ mr: 0.5 }}>
        Board
      </Typography>
      {BOARD_THEMES.map((theme) => (
        <Tooltip key={theme.id} title={theme.name}>
          <Box
            component="button"
            type="button"
            aria-label={`${theme.name} board`}
            aria-pressed={theme.id === current.id}
            onClick={() => onChoose(theme.id)}
            sx={{
              width: 30,
              height: 30,
              p: 0,
              borderRadius: 1,
              cursor: "pointer",
              overflow: "hidden",
              display: "grid",
              gridTemplateColumns: "1fr 1fr",
              gridTemplateRows: "1fr 1fr",
              border: "2px solid",
              borderColor: theme.id === current.id ? "primary.main" : "divider",
              outline: theme.id === current.id ? "2px solid" : "none",
              outlineColor: "primary.main",
              outlineOffset: 1,
            }}
          >
            {[theme.light, theme.dark, theme.dark, theme.light].map((c, i) => (
              <Box key={i} sx={{ bgcolor: c }} />
            ))}
          </Box>
        </Tooltip>
      ))}
    </Stack>
  );
}

function StatusPanel({ view }: { view: GameView }) {
  let text: string;
  let severity: "info" | "success" | "warning" | "error" = "info";
  if (view.status === "waiting") {
    text = "Waiting for an opponent.";
  } else if (view.status === "finished") {
    const reason = view.reason ? REASON_LABEL[view.reason] : "";
    if (view.winner === null) {
      text = `${view.reason === "aborted" ? "Aborted" : "Draw"} · ${reason}`;
    } else {
      const won = view.winner === view.you;
      text = `${won ? "You won" : "You lost"} · ${reason} · ${view.result}`;
      severity = won ? "success" : "error";
    }
    if (view.winner === null && view.result) text += ` · ${view.result}`;
  } else {
    const yours = view.turn === view.you;
    text = `${yours ? "Your move" : "Their move"}${view.check ? " — check!" : ""}`;
    severity = yours ? "warning" : "info";
  }
  return (
    <Alert severity={severity} icon={false} sx={{ fontWeight: 600 }}>
      {text}
    </Alert>
  );
}

function MoveList({ moves, endRef }: { moves: string[]; endRef: React.RefObject<HTMLDivElement> }) {
  if (moves.length === 0) {
    return (
      <Typography variant="body2" color="text.secondary">
        No moves yet.
      </Typography>
    );
  }
  const rows: [string, string | undefined][] = [];
  for (let i = 0; i < moves.length; i += 2) rows.push([moves[i], moves[i + 1]]);
  return (
    <Box
      sx={{
        maxHeight: 280,
        overflowY: "auto",
        display: "grid",
        gridTemplateColumns: "3ch 1fr 1fr",
        columnGap: 1.5,
        rowGap: 0.25,
        fontFamily: "monospace",
        fontSize: 14,
      }}
    >
      {rows.map(([white, black], i) => (
        <Box key={i} sx={{ display: "contents" }}>
          <Box sx={{ color: "text.secondary" }}>{i + 1}.</Box>
          <Box>{white}</Box>
          <Box>{black ?? ""}</Box>
        </Box>
      ))}
      <div ref={endRef} />
    </Box>
  );
}

function Offer({
  text,
  onAccept,
  onDecline,
}: {
  text: string;
  onAccept: () => void;
  onDecline: () => void;
}) {
  return (
    <Alert
      severity="info"
      action={
        <Stack direction="row" spacing={1}>
          <Button color="inherit" size="small" onClick={onAccept}>
            Accept
          </Button>
          <Button color="inherit" size="small" onClick={onDecline}>
            Decline
          </Button>
        </Stack>
      }
    >
      {text}
    </Alert>
  );
}
