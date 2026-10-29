/**
 * The chess board: draws a game view and turns clicks and drags into moves.
 *
 * Both ways of moving work on the same squares: click a piece then a target,
 * or press on a piece and drop it on a target. A drag uses pointer events
 * rather than HTML5 drag-and-drop, so the piece follows the pointer exactly
 * and it works with a touch screen too.
 *
 * The board only knows what the daemon says is legal (`view.legal`) — it never
 * works out chess for itself.
 */

import { Box, ButtonBase, Paper, Stack } from "@mui/material";
import { useTheme } from "@mui/material/styles";
import { useRef, useState } from "react";
import type { PointerEvent as ReactPointerEvent } from "react";

import type { Color, GameView, Kind } from "../api/chess";
import { brand } from "../theme";

/** Filled glyphs for both sides, coloured in CSS; `︎` asks for text, not emoji. */
const GLYPH: Record<string, string> = {
  K: "♚︎",
  Q: "♛︎",
  R: "♜︎",
  B: "♝︎",
  N: "♞︎",
  P: "♟︎",
};

const PROMOTION_CHOICES: { kind: Kind; letter: string }[] = [
  { kind: "queen", letter: "Q" },
  { kind: "rook", letter: "R" },
  { kind: "bishop", letter: "B" },
  { kind: "knight", letter: "N" },
];

const FILES = "abcdefgh";

function squareName(index: number): string {
  return `${FILES[index % 8]}${Math.floor(index / 8) + 1}`;
}

function squareIndex(name: string): number {
  return (Number(name[1]) - 1) * 8 + FILES.indexOf(name[0]);
}

interface Props {
  view: GameView;
  orientation: Color;
  /** Whether this player may move pieces right now. */
  canMove: boolean;
  onMove: (from: string, to: string, promotion?: Kind) => void;
  size: number;
}

interface Drag {
  from: string;
  x: number;
  y: number;
  /** Whether the pointer has travelled far enough to count as a drag. */
  moved: boolean;
  startX: number;
  startY: number;
  /** The square was already selected when the press began. */
  wasSelected: boolean;
}

export function ChessBoard({ view, orientation, canMove, onMove, size }: Props) {
  const theme = useTheme();
  const dark = theme.palette.mode === "dark";
  const boardRef = useRef<HTMLDivElement>(null);
  const [selected, setSelected] = useState<string | null>(null);
  const [drag, setDragState] = useState<Drag | null>(null);
  // Mirrored in a ref: a quick flick can deliver pointerup before React has
  // re-rendered with the drag that pointerdown started.
  const dragRef = useRef<Drag | null>(null);
  const setDrag = (next: Drag | null) => {
    dragRef.current = next;
    setDragState(next);
  };
  const [promotion, setPromotion] = useState<{ from: string; to: string } | null>(null);

  const cell = size / 8;
  const you = view.you;
  const lightSquare = dark ? "#B9C7D6" : "#E4ECF4";
  const darkSquare = dark ? brand.steel : brand.sky;

  const targetsFrom = (from: string | null) =>
    from ? view.legal.filter((m) => m.from === from) : [];
  const targets = new Set(targetsFrom(selected).map((m) => m.to));

  const pieceAt = (sq: string) => view.board[squareIndex(sq)] ?? "";
  const ownPiece = (sq: string) => {
    const code = pieceAt(sq);
    return !!code && !!you && code[0] === you[0];
  };

  /** Screen position (row, column from the top-left) of a square. */
  const place = (index: number) => {
    const file = index % 8;
    const rank = Math.floor(index / 8);
    return orientation === "white" ? { col: file, row: 7 - rank } : { col: 7 - file, row: rank };
  };

  const squareFromPoint = (clientX: number, clientY: number): string | null => {
    const rect = boardRef.current?.getBoundingClientRect();
    if (!rect) return null;
    const col = Math.floor(((clientX - rect.left) / rect.width) * 8);
    const row = Math.floor(((clientY - rect.top) / rect.height) * 8);
    if (col < 0 || col > 7 || row < 0 || row > 7) return null;
    const file = orientation === "white" ? col : 7 - col;
    const rank = orientation === "white" ? 7 - row : row;
    return squareName(rank * 8 + file);
  };

  const tryMove = (from: string, to: string): boolean => {
    const moves = view.legal.filter((m) => m.from === from && m.to === to);
    if (moves.length === 0) return false;
    if (moves.some((m) => m.promotion)) {
      setPromotion({ from, to });
    } else {
      onMove(from, to);
    }
    setSelected(null);
    return true;
  };

  const onPointerDown = (event: ReactPointerEvent<HTMLDivElement>) => {
    if (event.button !== 0 || promotion) return;
    const sq = squareFromPoint(event.clientX, event.clientY);
    if (!sq) return;

    // A second click on a highlighted target completes a click-move.
    if (selected && targets.has(sq) && tryMove(selected, sq)) return;

    if (canMove && ownPiece(sq)) {
      event.currentTarget.setPointerCapture(event.pointerId);
      setDrag({
        from: sq,
        x: event.clientX,
        y: event.clientY,
        startX: event.clientX,
        startY: event.clientY,
        moved: false,
        wasSelected: selected === sq,
      });
      setSelected(sq);
    } else {
      setSelected(null);
    }
  };

  const onPointerMove = (event: ReactPointerEvent<HTMLDivElement>) => {
    const drag = dragRef.current;
    if (!drag) return;
    const moved =
      drag.moved || Math.hypot(event.clientX - drag.startX, event.clientY - drag.startY) > 4;
    setDrag({ ...drag, x: event.clientX, y: event.clientY, moved });
  };

  const onPointerUp = (event: ReactPointerEvent<HTMLDivElement>) => {
    const drag = dragRef.current;
    if (!drag) return;
    const sq = squareFromPoint(event.clientX, event.clientY);
    setDrag(null);
    if (sq && sq !== drag.from) {
      // Released over another square: a drop. If it isn't a legal target the
      // piece goes home but stays selected, so a click can still finish.
      tryMove(drag.from, sq);
    } else if (drag.wasSelected && !drag.moved) {
      // Clicking the selected piece again puts it down.
      setSelected(null);
    }
  };

  const lastMove = new Set(view.last_move ?? []);
  const checkedKing =
    view.check && view.status !== "waiting"
      ? view.board.findIndex((code) => code === `${view.turn[0]}K`)
      : -1;
  const draggingFrom = drag?.moved ? drag.from : null;

  const glyph = (code: string, px: number) => (
    <Box
      component="span"
      sx={{
        fontSize: px,
        lineHeight: 1,
        fontFamily: '"DejaVu Sans", "Segoe UI Symbol", "Noto Sans Symbols 2", "Apple Symbols", serif',
        color: code[0] === "w" ? "#FAFAFA" : "#161616",
        WebkitTextStroke: code[0] === "w" ? "1.2px #1E1E1E" : "0.6px #000",
        textShadow:
          code[0] === "w" ? "0 1px 2px rgba(0,0,0,0.45)" : "0 1px 1px rgba(255,255,255,0.25)",
        userSelect: "none",
        pointerEvents: "none",
      }}
    >
      {GLYPH[code[1]]}
    </Box>
  );

  return (
    <Box sx={{ position: "relative", width: size, height: size, flexShrink: 0 }}>
      <Box
        ref={boardRef}
        onPointerDown={onPointerDown}
        onPointerMove={onPointerMove}
        onPointerUp={onPointerUp}
        onPointerCancel={() => setDrag(null)}
        sx={{
          position: "relative",
          width: size,
          height: size,
          borderRadius: 1,
          overflow: "hidden",
          boxShadow: 4,
          touchAction: "none",
          cursor: drag?.moved ? "grabbing" : "default",
        }}
      >
        {view.board.map((code, index) => {
          const sq = squareName(index);
          const { col, row } = place(index);
          const light = (index % 8) + Math.floor(index / 8);
          const isLight = light % 2 === 1;
          const isTarget = targets.has(sq);
          const occupied = !!code;
          const grabbable = canMove && occupied && !!you && code[0] === you[0];
          return (
            <Box
              key={sq}
              data-square={sq}
              sx={{
                position: "absolute",
                left: col * cell,
                top: row * cell,
                width: cell,
                height: cell,
                bgcolor: isLight ? lightSquare : darkSquare,
                display: "flex",
                alignItems: "center",
                justifyContent: "center",
                cursor: grabbable ? "grab" : isTarget ? "pointer" : "default",
              }}
            >
              {lastMove.has(sq) && (
                <Box sx={{ position: "absolute", inset: 0, bgcolor: "rgba(240,195,65,0.42)" }} />
              )}
              {selected === sq && (
                <Box sx={{ position: "absolute", inset: 0, bgcolor: "rgba(32,77,113,0.45)" }} />
              )}
              {index === checkedKing && (
                <Box
                  sx={{
                    position: "absolute",
                    inset: 0,
                    background:
                      "radial-gradient(circle, rgba(241,60,40,0.95) 0%, rgba(241,60,40,0.5) 45%, transparent 75%)",
                  }}
                />
              )}
              {/* File and rank labels along the bottom and left edges. */}
              {row === 7 && (
                <Box
                  component="span"
                  sx={{
                    position: "absolute",
                    right: 3,
                    bottom: 1,
                    fontSize: Math.max(10, cell * 0.17),
                    fontWeight: 600,
                    color: isLight ? darkSquare : lightSquare,
                    userSelect: "none",
                  }}
                >
                  {sq[0]}
                </Box>
              )}
              {col === 0 && (
                <Box
                  component="span"
                  sx={{
                    position: "absolute",
                    left: 3,
                    top: 1,
                    fontSize: Math.max(10, cell * 0.17),
                    fontWeight: 600,
                    color: isLight ? darkSquare : lightSquare,
                    userSelect: "none",
                  }}
                >
                  {sq[1]}
                </Box>
              )}
              {occupied && draggingFrom !== sq && (
                <Box sx={{ position: "relative", display: "flex" }}>{glyph(code, cell * 0.8)}</Box>
              )}
              {occupied && draggingFrom === sq && (
                <Box sx={{ position: "relative", display: "flex", opacity: 0.3 }}>
                  {glyph(code, cell * 0.8)}
                </Box>
              )}
              {isTarget &&
                (occupied ? (
                  <Box
                    sx={{
                      position: "absolute",
                      inset: 0,
                      borderRadius: "50%",
                      boxShadow: `inset 0 0 0 ${cell * 0.08}px rgba(12,33,49,0.45)`,
                    }}
                  />
                ) : (
                  <Box
                    sx={{
                      position: "absolute",
                      width: cell * 0.3,
                      height: cell * 0.3,
                      borderRadius: "50%",
                      bgcolor: "rgba(12,33,49,0.35)",
                    }}
                  />
                ))}
            </Box>
          );
        })}
      </Box>

      {drag?.moved && boardRef.current && (
        <Box
          sx={{
            position: "fixed",
            left: drag.x - cell / 2,
            top: drag.y - cell / 2,
            width: cell,
            height: cell,
            display: "flex",
            alignItems: "center",
            justifyContent: "center",
            pointerEvents: "none",
            zIndex: 1500,
            transform: "scale(1.12)",
          }}
        >
          {glyph(pieceAt(drag.from), cell * 0.8)}
        </Box>
      )}

      {promotion && (
        <Box
          onClick={() => setPromotion(null)}
          sx={{
            position: "absolute",
            inset: 0,
            bgcolor: "rgba(0,0,0,0.45)",
            display: "flex",
            alignItems: "center",
            justifyContent: "center",
            zIndex: 2,
          }}
        >
          <Paper elevation={8} sx={{ p: 1 }} onClick={(e) => e.stopPropagation()}>
            <Stack direction="row" spacing={1}>
              {PROMOTION_CHOICES.map(({ kind, letter }) => (
                <ButtonBase
                  key={kind}
                  aria-label={`Promote to ${kind}`}
                  onClick={() => {
                    onMove(promotion.from, promotion.to, kind);
                    setPromotion(null);
                  }}
                  sx={{
                    width: cell,
                    height: cell,
                    borderRadius: 1,
                    bgcolor: lightSquare,
                    "&:hover": { bgcolor: brand.gold },
                  }}
                >
                  {glyph(`${you?.[0] ?? "w"}${letter}`, cell * 0.8)}
                </ButtonBase>
              ))}
            </Stack>
          </Paper>
        </Box>
      )}
    </Box>
  );
}
