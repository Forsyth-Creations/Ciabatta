/**
 * The chess page's view of the daemon: the lobby, and one game at a time.
 *
 * A game is long-polled rather than streamed: `GET /api/chess/games/:id?since=`
 * answers as soon as the game's `rev` moves on, which is all a turn-based game
 * needs, and the clocks tick locally between answers.
 */

import { useQuery } from "@tanstack/react-query";
import { useEffect, useRef, useState } from "react";

import { api } from "./client";

export type Color = "white" | "black";
export type Kind = "pawn" | "knight" | "bishop" | "rook" | "queen" | "king";
export type Variant = "standard" | "chess960" | "king_of_the_hill" | "three_check";
export type ColorChoice = Color | "random";

export interface TimeControl {
  initial_secs: number;
  increment_secs: number;
}

export interface Rules {
  variant: Variant;
  time: TimeControl | null;
  host_color: ColorChoice;
  takebacks: boolean;
  draw_offers: boolean;
}

export interface LegalMove {
  from: string;
  to: string;
  promotion?: Kind;
  castle?: boolean;
}

export type Reason =
  | "checkmate"
  | "resignation"
  | "timeout"
  | "timeout_vs_insufficient"
  | "stalemate"
  | "agreement"
  | "repetition"
  | "fifty_moves"
  | "insufficient_material"
  | "king_of_the_hill"
  | "three_checks"
  | "aborted";

export interface GameView {
  id: string;
  rules: Rules;
  created_at: string;
  host_name: string;
  guest_name: string | null;
  host_color: Color | null;
  status: "waiting" | "playing" | "finished";
  winner: Color | null;
  reason: Reason | null;
  result: string | null;
  board: string[];
  fen: string;
  turn: Color;
  check: boolean;
  last_move: [string, string] | null;
  moves: string[];
  legal: LegalMove[];
  clock: { white_ms: number; black_ms: number; running: Color | null; increment_ms: number } | null;
  offers: { draw: Color | null; takeback: Color | null; rematch: Color | null };
  checks: { white: number; black: number };
  role: "host" | "guest";
  you: Color | null;
  opponent: string | null;
  connected: boolean;
  address: string | null;
  closed: string | null;
  rev: number;
}

export interface OpenGame {
  id: string;
  rules: Rules;
  created_at: string;
}

export interface Peer {
  instance: string;
  name: string;
  address: string;
  version: string;
  games: OpenGame[];
}

export interface Lobby {
  name: string;
  hostname: string;
  version: string;
  port: number | null;
  address: string | null;
  discovery_port: number;
  discovery_error: string | null;
  peers: Peer[];
  games: GameView[];
}

export type Action =
  | { type: "move"; from: string; to: string; promotion?: Kind }
  | { type: "resign" }
  | { type: "offer_draw" }
  | { type: "accept_draw" }
  | { type: "decline_draw" }
  | { type: "request_takeback" }
  | { type: "accept_takeback" }
  | { type: "decline_takeback" }
  | { type: "offer_rematch" }
  | { type: "accept_rematch" }
  | { type: "decline_rematch" }
  | { type: "abort" };

export const chessKeys = { lobby: ["chess", "lobby"] as const };

export function useLobby() {
  return useQuery({
    queryKey: chessKeys.lobby,
    queryFn: () => api.get<Lobby>("/api/chess"),
    refetchInterval: 2_000,
  });
}

export const chessApi = {
  create: (rules: Rules) => api.post<GameView>("/api/chess/games", rules),
  act: (id: string, action: Action) => api.post<GameView>(`/api/chess/games/${id}/action`, action),
  remove: (id: string) => api.delete<{ ok: boolean }>(`/api/chess/games/${id}`),
  join: (address: string, game: string) => api.post<GameView>("/api/chess/join", { address, game }),
  browse: (address: string) =>
    api.post<{ name: string; address: string; games: OpenGame[] }>("/api/chess/browse", { address }),
  setName: (name: string) => api.post<{ name: string }>("/api/chess/name", { name }),
};

/**
 * Follow one game: the latest view, when it arrived, and a setter for the
 * views that come back from actions so the board updates without waiting for
 * the next poll.
 */
export function useGame(id: string) {
  const [view, setView] = useState<GameView | null>(null);
  const [receivedAt, setReceivedAt] = useState(() => performance.now());
  const [error, setError] = useState<unknown>(null);
  const rev = useRef<number | null>(null);

  const accept = (next: GameView) => {
    rev.current = next.rev;
    setView(next);
    setReceivedAt(performance.now());
    setError(null);
  };

  useEffect(() => {
    let cancelled = false;
    rev.current = null;
    setView(null);
    (async () => {
      while (!cancelled) {
        try {
          const since = rev.current === null ? "" : `?since=${rev.current}`;
          const next = await api.get<GameView>(`/api/chess/games/${id}${since}`);
          if (cancelled) return;
          if (next.rev !== rev.current) accept(next);
        } catch (e) {
          if (cancelled) return;
          setError(e);
          await new Promise((r) => setTimeout(r, 1_500));
        }
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [id]);

  return { view, receivedAt, error, accept };
}

export const VARIANT_LABEL: Record<Variant, string> = {
  standard: "Standard",
  chess960: "Chess960",
  king_of_the_hill: "King of the Hill",
  three_check: "Three-check",
};

export function timeLabel(time: TimeControl | null): string {
  if (!time) return "Unlimited";
  const minutes = time.initial_secs / 60;
  const base = Number.isInteger(minutes) ? `${minutes}` : `${time.initial_secs}s`;
  return `${base}+${time.increment_secs}`;
}

export function rulesSummary(rules: Rules): string {
  const parts = [VARIANT_LABEL[rules.variant], timeLabel(rules.time)];
  if (!rules.takebacks) parts.push("no takebacks");
  if (!rules.draw_offers) parts.push("no draw offers");
  return parts.join(" · ");
}

export const REASON_LABEL: Record<Reason, string> = {
  checkmate: "Checkmate",
  resignation: "Resignation",
  timeout: "Out of time",
  timeout_vs_insufficient: "Out of time, but the opponent can't mate",
  stalemate: "Stalemate",
  agreement: "Draw agreed",
  repetition: "Threefold repetition",
  fifty_moves: "Fifty-move rule",
  insufficient_material: "Insufficient material",
  king_of_the_hill: "King reached the hill",
  three_checks: "Three checks",
  aborted: "Aborted",
};
