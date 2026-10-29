//! The rules of chess: positions, legal moves, notation, and the material and
//! board tests a game needs to decide how it ends.
//!
//! A plain 8x8 mailbox. Nothing here searches, so speed is irrelevant next to
//! being obviously right: legality is "generate everything the pieces can do,
//! then keep the moves that don't leave your own king attacked".
//!
//! Castling is written for Chess960 from the start, because the standard rule
//! is just the special case where the rooks begin on a and h. A side's rights
//! are the *files* of the rooks it may still castle with, and a castling move
//! names that rook's square as its destination (the UCI 960 convention), so a
//! king that already stands on g1 can still castle without the move being
//! ambiguous.

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Color {
    White,
    Black,
}

impl Color {
    pub fn other(self) -> Self {
        match self {
            Color::White => Color::Black,
            Color::Black => Color::White,
        }
    }

    pub fn index(self) -> usize {
        match self {
            Color::White => 0,
            Color::Black => 1,
        }
    }

    /// Which way this side's pawns walk, in ranks.
    fn forward(self) -> i8 {
        match self {
            Color::White => 1,
            Color::Black => -1,
        }
    }

    fn back_rank(self) -> u8 {
        match self {
            Color::White => 0,
            Color::Black => 7,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Pawn,
    Knight,
    Bishop,
    Rook,
    Queen,
    King,
}

impl Kind {
    fn letter(self) -> char {
        match self {
            Kind::Pawn => 'P',
            Kind::Knight => 'N',
            Kind::Bishop => 'B',
            Kind::Rook => 'R',
            Kind::Queen => 'Q',
            Kind::King => 'K',
        }
    }

    fn from_letter(c: char) -> Option<Self> {
        Some(match c.to_ascii_uppercase() {
            'P' => Kind::Pawn,
            'N' => Kind::Knight,
            'B' => Kind::Bishop,
            'R' => Kind::Rook,
            'Q' => Kind::Queen,
            'K' => Kind::King,
            _ => return None,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Piece {
    pub color: Color,
    pub kind: Kind,
}

impl Piece {
    /// `wK`, `bp`… — the form the web app draws from.
    pub fn code(self) -> String {
        let side = match self.color {
            Color::White => 'w',
            Color::Black => 'b',
        };
        format!("{side}{}", self.kind.letter())
    }

    fn fen_char(self) -> char {
        match self.color {
            Color::White => self.kind.letter(),
            Color::Black => self.kind.letter().to_ascii_lowercase(),
        }
    }
}

/// 0..64, a1 = 0, b1 = 1, … h8 = 63.
pub type Square = u8;

pub fn file_of(sq: Square) -> u8 {
    sq % 8
}

pub fn rank_of(sq: Square) -> u8 {
    sq / 8
}

pub fn square_name(sq: Square) -> String {
    format!("{}{}", (b'a' + file_of(sq)) as char, rank_of(sq) + 1)
}

pub fn parse_square(name: &str) -> Option<Square> {
    let bytes = name.as_bytes();
    if bytes.len() != 2 {
        return None;
    }
    let file = bytes[0].checked_sub(b'a').filter(|f| *f < 8)?;
    let rank = bytes[1].checked_sub(b'1').filter(|r| *r < 8)?;
    Some(rank * 8 + file)
}

/// The square `df` files and `dr` ranks away, if it's on the board.
fn offset(sq: Square, df: i8, dr: i8) -> Option<Square> {
    let file = file_of(sq) as i8 + df;
    let rank = rank_of(sq) as i8 + dr;
    ((0..8).contains(&file) && (0..8).contains(&rank)).then(|| (rank * 8 + file) as Square)
}

/// Whether a square is light, for the same-coloured-bishops draw.
fn is_light(sq: Square) -> bool {
    (file_of(sq) + rank_of(sq)) % 2 == 1
}

const KNIGHT_STEPS: [(i8, i8); 8] = [
    (1, 2),
    (2, 1),
    (2, -1),
    (1, -2),
    (-1, -2),
    (-2, -1),
    (-2, 1),
    (-1, 2),
];
const KING_STEPS: [(i8, i8); 8] = [
    (1, 0),
    (1, 1),
    (0, 1),
    (-1, 1),
    (-1, 0),
    (-1, -1),
    (0, -1),
    (1, -1),
];
const ROOK_DIRS: [(i8, i8); 4] = [(1, 0), (-1, 0), (0, 1), (0, -1)];
const BISHOP_DIRS: [(i8, i8); 4] = [(1, 1), (1, -1), (-1, 1), (-1, -1)];
const PROMOTIONS: [Kind; 4] = [Kind::Queen, Kind::Rook, Kind::Bishop, Kind::Knight];

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CastleSide {
    King,
    Queen,
}

impl CastleSide {
    fn index(self) -> usize {
        match self {
            CastleSide::King => 0,
            CastleSide::Queen => 1,
        }
    }

    /// Where the king and rook end up, as files.
    fn king_file(self) -> u8 {
        match self {
            CastleSide::King => 6,
            CastleSide::Queen => 2,
        }
    }

    fn rook_file(self) -> u8 {
        match self {
            CastleSide::King => 5,
            CastleSide::Queen => 3,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Move {
    pub from: Square,
    /// For a castling move, the castling rook's square — see the module docs.
    pub to: Square,
    pub promotion: Option<Kind>,
    pub castle: Option<CastleSide>,
}

impl Move {
    /// Where the moving piece lands. For castling that's the king's square,
    /// not the rook's.
    pub fn landing(&self) -> Square {
        match self.castle {
            Some(side) => rank_of(self.from) * 8 + side.king_file(),
            None => self.to,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Position {
    board: [Option<Piece>; 64],
    pub turn: Color,
    /// The files of the rooks each side may still castle with, indexed
    /// `[color][CastleSide]`.
    castling: [[Option<u8>; 2]; 2],
    /// The square a pawn may capture onto en passant. Only set when an enemy
    /// pawn stands ready to take it, so it never makes otherwise identical
    /// positions count as different for repetition.
    ep: Option<Square>,
    pub halfmove: u32,
    pub fullmove: u32,
}

pub const STANDARD_FEN: &str = "rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq - 0 1";

impl Position {
    pub fn standard() -> Self {
        Self::from_fen(STANDARD_FEN).expect("the standard position parses")
    }

    /// Chess960 start position `n` (0..960) in Scharnagl's numbering, where
    /// 518 is the standard setup.
    pub fn chess960(n: u16) -> Self {
        let mut n = (n % 960) as usize;
        let mut back: [Option<Kind>; 8] = [None; 8];

        back[[1, 3, 5, 7][n % 4]] = Some(Kind::Bishop);
        n /= 4;
        back[[0, 2, 4, 6][n % 4]] = Some(Kind::Bishop);
        n /= 4;

        let place_nth_empty = |back: &mut [Option<Kind>; 8], index: usize, kind: Kind| {
            let file = (0..8)
                .filter(|f| back[*f].is_none())
                .nth(index)
                .expect("there are enough empty files");
            back[file] = Some(kind);
        };

        place_nth_empty(&mut back, n % 6, Kind::Queen);
        n /= 6;

        const KNIGHTS: [(usize, usize); 10] = [
            (0, 1),
            (0, 2),
            (0, 3),
            (0, 4),
            (1, 2),
            (1, 3),
            (1, 4),
            (2, 3),
            (2, 4),
            (3, 4),
        ];
        let (a, b) = KNIGHTS[n];
        // The second knight first: placing the first shifts the indices.
        place_nth_empty(&mut back, b, Kind::Knight);
        place_nth_empty(&mut back, a, Kind::Knight);

        // Rook, king, rook in the three files left.
        for kind in [Kind::Rook, Kind::King, Kind::Rook] {
            place_nth_empty(&mut back, 0, kind);
        }

        let mut position = Position {
            board: [None; 64],
            turn: Color::White,
            castling: [[None; 2]; 2],
            ep: None,
            halfmove: 0,
            fullmove: 1,
        };
        let rooks: Vec<u8> = (0..8u8)
            .filter(|f| back[*f as usize] == Some(Kind::Rook))
            .collect();
        for color in [Color::White, Color::Black] {
            let rank = color.back_rank();
            let pawn_rank = (rank as i8 + color.forward()) as u8;
            for file in 0..8u8 {
                position.board[(rank * 8 + file) as usize] =
                    back[file as usize].map(|kind| Piece { color, kind });
                position.board[(pawn_rank * 8 + file) as usize] = Some(Piece {
                    color,
                    kind: Kind::Pawn,
                });
            }
            position.castling[color.index()] = [Some(rooks[1]), Some(rooks[0])];
        }
        position
    }

    pub fn piece_at(&self, sq: Square) -> Option<Piece> {
        self.board[sq as usize]
    }

    /// Parse FEN, accepting `KQkq` as well as Shredder-style rook files
    /// (`HAha`) in the castling field.
    pub fn from_fen(fen: &str) -> Result<Self, String> {
        let fields: Vec<&str> = fen.split_whitespace().collect();
        if fields.len() < 4 {
            return Err(format!("'{fen}' is not a FEN position"));
        }

        let mut board = [None; 64];
        let ranks: Vec<&str> = fields[0].split('/').collect();
        if ranks.len() != 8 {
            return Err("a FEN board has eight ranks".into());
        }
        for (i, row) in ranks.iter().enumerate() {
            let rank = 7 - i as u8;
            let mut file = 0u8;
            for c in row.chars() {
                if let Some(skip) = c.to_digit(10) {
                    file += skip as u8;
                } else {
                    let kind =
                        Kind::from_letter(c).ok_or_else(|| format!("unknown piece '{c}'"))?;
                    let color = if c.is_ascii_uppercase() {
                        Color::White
                    } else {
                        Color::Black
                    };
                    if file >= 8 {
                        return Err(format!("rank {} is too long", rank + 1));
                    }
                    board[(rank * 8 + file) as usize] = Some(Piece { color, kind });
                    file += 1;
                }
            }
            if file != 8 {
                return Err(format!("rank {} doesn't have eight files", rank + 1));
            }
        }

        let turn = match fields[1] {
            "w" => Color::White,
            "b" => Color::Black,
            other => return Err(format!("'{other}' is not a side to move")),
        };

        let mut position = Position {
            board,
            turn,
            castling: [[None; 2]; 2],
            ep: None,
            halfmove: fields.get(4).and_then(|v| v.parse().ok()).unwrap_or(0),
            fullmove: fields.get(5).and_then(|v| v.parse().ok()).unwrap_or(1),
        };

        if fields[2] != "-" {
            for c in fields[2].chars() {
                let color = if c.is_ascii_uppercase() {
                    Color::White
                } else {
                    Color::Black
                };
                let king = position
                    .king(color)
                    .filter(|k| rank_of(*k) == color.back_rank())
                    .ok_or("castling rights without a king on its back rank")?;
                let rook_files = (0..8u8).filter(|f| {
                    position.piece_at(color.back_rank() * 8 + f)
                        == Some(Piece {
                            color,
                            kind: Kind::Rook,
                        })
                });
                let (side, file) = match c.to_ascii_lowercase() {
                    'k' => (
                        CastleSide::King,
                        rook_files.filter(|f| *f > file_of(king)).max(),
                    ),
                    'q' => (
                        CastleSide::Queen,
                        rook_files.filter(|f| *f < file_of(king)).min(),
                    ),
                    f @ 'a'..='h' => {
                        let file = f as u8 - b'a';
                        let side = if file > file_of(king) {
                            CastleSide::King
                        } else {
                            CastleSide::Queen
                        };
                        (side, Some(file))
                    }
                    _ => return Err(format!("'{c}' is not a castling right")),
                };
                let file = file.ok_or_else(|| format!("castling right '{c}' has no rook"))?;
                position.castling[color.index()][side.index()] = Some(file);
            }
        }

        if fields[3] != "-" {
            let sq = parse_square(fields[3]).ok_or("bad en passant square")?;
            position.ep = position.ep_if_capturable(sq);
        }

        if position.king(Color::White).is_none() || position.king(Color::Black).is_none() {
            return Err("both sides need a king".into());
        }
        Ok(position)
    }

    pub fn fen(&self) -> String {
        let mut out = String::new();
        for rank in (0..8u8).rev() {
            let mut empty = 0;
            for file in 0..8u8 {
                match self.board[(rank * 8 + file) as usize] {
                    Some(piece) => {
                        if empty > 0 {
                            out.push_str(&empty.to_string());
                            empty = 0;
                        }
                        out.push(piece.fen_char());
                    }
                    None => empty += 1,
                }
            }
            if empty > 0 {
                out.push_str(&empty.to_string());
            }
            if rank > 0 {
                out.push('/');
            }
        }
        out.push(' ');
        out.push(match self.turn {
            Color::White => 'w',
            Color::Black => 'b',
        });
        out.push(' ');
        out.push_str(&self.castling_field());
        out.push(' ');
        out.push_str(&self.ep.map(square_name).unwrap_or_else(|| "-".into()));
        out.push_str(&format!(" {} {}", self.halfmove, self.fullmove));
        out
    }

    /// `KQkq` when the rooks are on a and h, rook files otherwise.
    fn castling_field(&self) -> String {
        let mut out = String::new();
        for color in [Color::White, Color::Black] {
            for side in [CastleSide::King, CastleSide::Queen] {
                let Some(file) = self.castling[color.index()][side.index()] else {
                    continue;
                };
                let standard = match side {
                    CastleSide::King => file == 7,
                    CastleSide::Queen => file == 0,
                };
                let c = if standard {
                    match side {
                        CastleSide::King => 'k',
                        CastleSide::Queen => 'q',
                    }
                } else {
                    (b'a' + file) as char
                };
                out.push(match color {
                    Color::White => c.to_ascii_uppercase(),
                    Color::Black => c,
                });
            }
        }
        if out.is_empty() { "-".into() } else { out }
    }

    /// What identifies a position for the repetition rule: everything but the
    /// move counters.
    pub fn repetition_key(&self) -> String {
        let fen = self.fen();
        fen.split(' ').take(4).collect::<Vec<_>>().join(" ")
    }

    pub fn king(&self, color: Color) -> Option<Square> {
        (0..64u8).find(|sq| {
            self.board[*sq as usize]
                == Some(Piece {
                    color,
                    kind: Kind::King,
                })
        })
    }

    /// Whether `by` attacks `sq` on `board`.
    fn attacked_on(board: &[Option<Piece>; 64], sq: Square, by: Color) -> bool {
        let is = |s: Option<Square>, kinds: &[Kind]| {
            s.and_then(|s| board[s as usize])
                .is_some_and(|p| p.color == by && kinds.contains(&p.kind))
        };

        // A pawn of `by` attacks diagonally forward, so look one rank back.
        let back = -by.forward();
        if is(offset(sq, -1, back), &[Kind::Pawn]) || is(offset(sq, 1, back), &[Kind::Pawn]) {
            return true;
        }
        if KNIGHT_STEPS
            .iter()
            .any(|(df, dr)| is(offset(sq, *df, *dr), &[Kind::Knight]))
        {
            return true;
        }
        if KING_STEPS
            .iter()
            .any(|(df, dr)| is(offset(sq, *df, *dr), &[Kind::King]))
        {
            return true;
        }
        let ray_hits = |dirs: &[(i8, i8)], kinds: &[Kind]| {
            dirs.iter().any(|(df, dr)| {
                let mut at = sq;
                while let Some(next) = offset(at, *df, *dr) {
                    if let Some(piece) = board[next as usize] {
                        return piece.color == by && kinds.contains(&piece.kind);
                    }
                    at = next;
                }
                false
            })
        };
        ray_hits(&ROOK_DIRS, &[Kind::Rook, Kind::Queen])
            || ray_hits(&BISHOP_DIRS, &[Kind::Bishop, Kind::Queen])
    }

    pub fn attacked(&self, sq: Square, by: Color) -> bool {
        Self::attacked_on(&self.board, sq, by)
    }

    /// Whether the side to move is in check.
    pub fn in_check(&self) -> bool {
        self.king(self.turn)
            .is_some_and(|k| self.attacked(k, self.turn.other()))
    }

    /// `sq` as an en passant target, if a pawn of the side to move could
    /// actually take there.
    fn ep_if_capturable(&self, sq: Square) -> Option<Square> {
        let us = self.turn;
        let behind = -us.forward();
        [-1, 1]
            .iter()
            .any(|df| {
                offset(sq, *df, behind).and_then(|s| self.piece_at(s))
                    == Some(Piece {
                        color: us,
                        kind: Kind::Pawn,
                    })
            })
            .then_some(sq)
    }

    fn pseudo_moves(&self, out: &mut Vec<Move>) {
        let us = self.turn;
        for from in 0..64u8 {
            let Some(piece) = self.piece_at(from) else {
                continue;
            };
            if piece.color != us {
                continue;
            }
            let mut push = |to: Square| {
                out.push(Move {
                    from,
                    to,
                    promotion: None,
                    castle: None,
                })
            };
            let free_or_enemy = |to: Square| self.piece_at(to).is_none_or(|p| p.color != us);

            match piece.kind {
                Kind::Pawn => {
                    let fwd = us.forward();
                    let last_rank = us.other().back_rank();
                    let pawn_to = |to: Square, out: &mut Vec<Move>| {
                        if rank_of(to) == last_rank {
                            for kind in PROMOTIONS {
                                out.push(Move {
                                    from,
                                    to,
                                    promotion: Some(kind),
                                    castle: None,
                                });
                            }
                        } else {
                            out.push(Move {
                                from,
                                to,
                                promotion: None,
                                castle: None,
                            });
                        }
                    };
                    if let Some(one) = offset(from, 0, fwd).filter(|s| self.piece_at(*s).is_none())
                    {
                        pawn_to(one, out);
                        let start_rank = (us.back_rank() as i8 + fwd) as u8;
                        if rank_of(from) == start_rank
                            && let Some(two) =
                                offset(one, 0, fwd).filter(|s| self.piece_at(*s).is_none())
                        {
                            pawn_to(two, out);
                        }
                    }
                    for df in [-1, 1] {
                        let Some(to) = offset(from, df, fwd) else {
                            continue;
                        };
                        let enemy = self.piece_at(to).is_some_and(|p| p.color != us);
                        if enemy || self.ep == Some(to) {
                            pawn_to(to, out);
                        }
                    }
                }
                Kind::Knight | Kind::King => {
                    let steps = if piece.kind == Kind::Knight {
                        &KNIGHT_STEPS
                    } else {
                        &KING_STEPS
                    };
                    for (df, dr) in steps {
                        if let Some(to) = offset(from, *df, *dr).filter(|s| free_or_enemy(*s)) {
                            push(to);
                        }
                    }
                }
                Kind::Bishop | Kind::Rook | Kind::Queen => {
                    let dirs: Vec<(i8, i8)> = match piece.kind {
                        Kind::Bishop => BISHOP_DIRS.to_vec(),
                        Kind::Rook => ROOK_DIRS.to_vec(),
                        _ => ROOK_DIRS.iter().chain(&BISHOP_DIRS).copied().collect(),
                    };
                    for (df, dr) in dirs {
                        let mut at = from;
                        while let Some(to) = offset(at, df, dr) {
                            match self.piece_at(to) {
                                None => push(to),
                                Some(p) => {
                                    if p.color != us {
                                        push(to);
                                    }
                                    break;
                                }
                            }
                            at = to;
                        }
                    }
                }
            }
        }
    }

    /// Castling moves that are legal right now.
    fn castling_moves(&self, out: &mut Vec<Move>) {
        let us = self.turn;
        let them = us.other();
        let Some(king) = self.king(us) else { return };
        if rank_of(king) != us.back_rank() || self.in_check() {
            return;
        }
        let back = us.back_rank();
        for side in [CastleSide::King, CastleSide::Queen] {
            let Some(rook_file) = self.castling[us.index()][side.index()] else {
                continue;
            };
            let rook = back * 8 + rook_file;
            if self.piece_at(rook)
                != Some(Piece {
                    color: us,
                    kind: Kind::Rook,
                })
            {
                continue;
            }
            let king_to = back * 8 + side.king_file();
            let rook_to = back * 8 + side.rook_file();

            // Every square either piece crosses or lands on must be empty of
            // everything but the two of them. The two paths always overlap, so
            // their span is one run of squares.
            let lo = king.min(king_to).min(rook).min(rook_to);
            let hi = king.max(king_to).max(rook).max(rook_to);
            if (lo..=hi).any(|s| s != king && s != rook && self.piece_at(s).is_some()) {
                continue;
            }

            // The king may not pass through or land on an attacked square.
            // Judged with both castling pieces lifted, so neither shields a
            // square it is about to leave.
            let mut lifted = self.board;
            lifted[king as usize] = None;
            lifted[rook as usize] = None;
            let (a, b) = (king.min(king_to), king.max(king_to));
            if (a..=b).any(|s| Self::attacked_on(&lifted, s, them)) {
                continue;
            }

            out.push(Move {
                from: king,
                to: rook,
                promotion: None,
                castle: Some(side),
            });
        }
    }

    pub fn legal_moves(&self) -> Vec<Move> {
        let mut pseudo = Vec::with_capacity(64);
        self.pseudo_moves(&mut pseudo);
        let us = self.turn;
        let mut legal: Vec<Move> = pseudo
            .into_iter()
            .filter(|m| {
                let after = self.play(m);
                after
                    .king(us)
                    .is_some_and(|k| !after.attacked(k, us.other()))
            })
            .collect();
        let mut castles = Vec::new();
        self.castling_moves(&mut castles);
        legal.extend(castles.into_iter().filter(|m| {
            let after = self.play(m);
            after
                .king(us)
                .is_some_and(|k| !after.attacked(k, us.other()))
        }));
        legal
    }

    /// The position after `m`, which is assumed to be at least pseudo-legal.
    pub fn play(&self, m: &Move) -> Position {
        let mut next = self.clone();
        let us = self.turn;
        let them = us.other();
        let Some(piece) = self.piece_at(m.from) else {
            return next;
        };
        next.ep = None;

        if let Some(side) = m.castle {
            let back = us.back_rank();
            next.board[m.from as usize] = None;
            next.board[m.to as usize] = None;
            next.board[(back * 8 + side.king_file()) as usize] = Some(piece);
            next.board[(back * 8 + side.rook_file()) as usize] = Some(Piece {
                color: us,
                kind: Kind::Rook,
            });
            next.castling[us.index()] = [None, None];
            next.halfmove += 1;
        } else {
            let mut captured = self.piece_at(m.to);
            if piece.kind == Kind::Pawn && Some(m.to) == self.ep && captured.is_none() {
                let taken = rank_of(m.from) * 8 + file_of(m.to);
                captured = next.board[taken as usize].take();
            }
            next.board[m.from as usize] = None;
            next.board[m.to as usize] = Some(Piece {
                color: us,
                kind: m.promotion.unwrap_or(piece.kind),
            });

            if piece.kind == Kind::Pawn || captured.is_some() {
                next.halfmove = 0;
            } else {
                next.halfmove += 1;
            }

            if piece.kind == Kind::King {
                next.castling[us.index()] = [None, None];
            }
            if piece.kind == Kind::Rook && rank_of(m.from) == us.back_rank() {
                for right in &mut next.castling[us.index()] {
                    if *right == Some(file_of(m.from)) {
                        *right = None;
                    }
                }
            }
            if captured.is_some_and(|p| p.kind == Kind::Rook) && rank_of(m.to) == them.back_rank() {
                for right in &mut next.castling[them.index()] {
                    if *right == Some(file_of(m.to)) {
                        *right = None;
                    }
                }
            }

            next.turn = them;
            if piece.kind == Kind::Pawn && (rank_of(m.to) as i8 - rank_of(m.from) as i8).abs() == 2
            {
                let skipped = (rank_of(m.from) as i8 + us.forward()) as u8 * 8 + file_of(m.from);
                next.ep = next.ep_if_capturable(skipped);
            }
        }

        next.turn = them;
        if us == Color::Black {
            next.fullmove += 1;
        }
        next
    }

    /// Standard algebraic notation for a legal move, with `+` or `#`.
    pub fn san(&self, m: &Move) -> String {
        let mut out = match m.castle {
            Some(CastleSide::King) => "O-O".to_string(),
            Some(CastleSide::Queen) => "O-O-O".to_string(),
            None => {
                let piece = self.piece_at(m.from).expect("a move starts at a piece");
                let capture = self.piece_at(m.to).is_some()
                    || (piece.kind == Kind::Pawn && file_of(m.from) != file_of(m.to));
                let mut s = String::new();
                if piece.kind == Kind::Pawn {
                    if capture {
                        s.push((b'a' + file_of(m.from)) as char);
                    }
                } else {
                    s.push(piece.kind.letter());
                    let rivals: Vec<Move> = self
                        .legal_moves()
                        .into_iter()
                        .filter(|o| {
                            o.castle.is_none()
                                && o.to == m.to
                                && o.from != m.from
                                && self.piece_at(o.from) == Some(piece)
                        })
                        .collect();
                    if !rivals.is_empty() {
                        let same_file = rivals.iter().any(|o| file_of(o.from) == file_of(m.from));
                        let same_rank = rivals.iter().any(|o| rank_of(o.from) == rank_of(m.from));
                        if !same_file {
                            s.push((b'a' + file_of(m.from)) as char);
                        } else if !same_rank {
                            s.push((b'1' + rank_of(m.from)) as char);
                        } else {
                            s.push_str(&square_name(m.from));
                        }
                    }
                }
                if capture {
                    s.push('x');
                }
                s.push_str(&square_name(m.to));
                if let Some(kind) = m.promotion {
                    s.push('=');
                    s.push(kind.letter());
                }
                s
            }
        };

        let after = self.play(m);
        if after.in_check() {
            out.push(if after.legal_moves().is_empty() {
                '#'
            } else {
                '+'
            });
        }
        out
    }

    /// Neither side can possibly mate: kings alone, a single minor piece, or
    /// only bishops that all share a square colour.
    pub fn insufficient_material(&self) -> bool {
        let mut minors = 0;
        let mut bishops_light = false;
        let mut bishops_dark = false;
        let mut knights = 0;
        for sq in 0..64u8 {
            let Some(piece) = self.piece_at(sq) else {
                continue;
            };
            match piece.kind {
                Kind::King => {}
                Kind::Pawn | Kind::Rook | Kind::Queen => return false,
                Kind::Knight => {
                    knights += 1;
                    minors += 1;
                }
                Kind::Bishop => {
                    minors += 1;
                    if is_light(sq) {
                        bishops_light = true;
                    } else {
                        bishops_dark = true;
                    }
                }
            }
        }
        minors <= 1 || (knights == 0 && !(bishops_light && bishops_dark))
    }

    /// Whether `color` has nothing but its king — the case where running out
    /// of time against it is a draw rather than a loss.
    pub fn bare_king(&self, color: Color) -> bool {
        (0..64u8).all(|sq| {
            self.piece_at(sq)
                .is_none_or(|p| p.color != color || p.kind == Kind::King)
        })
    }

    /// Whether `color`'s king stands on one of the four centre squares.
    pub fn king_on_hill(&self, color: Color) -> bool {
        self.king(color)
            .is_some_and(|k| (3..=4).contains(&file_of(k)) && (3..=4).contains(&rank_of(k)))
    }

    /// Every square, a1 first, as the web app's piece codes (`""` for empty).
    pub fn board_codes(&self) -> Vec<String> {
        self.board
            .iter()
            .map(|p| p.map(Piece::code).unwrap_or_default())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Count leaf nodes `depth` plies down — the standard way to prove a move
    /// generator right against published numbers.
    fn perft(position: &Position, depth: u32) -> u64 {
        if depth == 0 {
            return 1;
        }
        position
            .legal_moves()
            .iter()
            .map(|m| perft(&position.play(m), depth - 1))
            .sum()
    }

    #[test]
    fn perft_from_the_start() {
        let start = Position::standard();
        assert_eq!(perft(&start, 1), 20);
        assert_eq!(perft(&start, 2), 400);
        assert_eq!(perft(&start, 3), 8_902);
    }

    #[test]
    fn perft_kiwipete_covers_castling_ep_and_promotion() {
        let p = Position::from_fen(
            "r3k2r/p1ppqpb1/bn2pnp1/3PN3/1p2P3/2N2Q1p/PPPBBPPP/R3K2R w KQkq - 0 1",
        )
        .unwrap();
        assert_eq!(perft(&p, 1), 48);
        assert_eq!(perft(&p, 2), 2_039);
        assert_eq!(perft(&p, 3), 97_862);
    }

    #[test]
    fn perft_position_three_catches_ep_pins() {
        let p = Position::from_fen("8/2p5/3p4/KP5r/1R3p1k/8/4P1P1/8 w - - 0 1").unwrap();
        assert_eq!(perft(&p, 1), 14);
        assert_eq!(perft(&p, 2), 191);
        assert_eq!(perft(&p, 3), 2_812);
        assert_eq!(perft(&p, 4), 43_238);
    }

    #[test]
    fn perft_position_four_covers_promotions_into_check() {
        let p =
            Position::from_fen("r3k2r/Pppp1ppp/1b3nbN/nP6/BBP1P3/q4N2/Pp1P2PP/R2Q1RK1 w kq - 0 1")
                .unwrap();
        assert_eq!(perft(&p, 1), 6);
        assert_eq!(perft(&p, 2), 264);
        assert_eq!(perft(&p, 3), 9_467);
    }

    #[test]
    fn chess960_numbering_matches_the_standard_setup_at_518() {
        assert_eq!(Position::chess960(518).fen(), STANDARD_FEN);
        // Position 0 is BBQNNRKR.
        assert!(Position::chess960(0).fen().starts_with("bbqnnrkr/"));
    }

    #[test]
    fn every_chess960_start_is_well_formed() {
        for n in 0..960 {
            let p = Position::chess960(n);
            assert!(p.legal_moves().len() >= 18, "position {n}");
            let back: Vec<Kind> = (0..8).map(|f| p.piece_at(f).unwrap().kind).collect();
            let king = back.iter().position(|k| *k == Kind::King).unwrap();
            let rooks: Vec<usize> = (0..8).filter(|f| back[*f] == Kind::Rook).collect();
            assert!(
                rooks[0] < king && king < rooks[1],
                "king between rooks in {n}"
            );
            let bishops: Vec<usize> = (0..8).filter(|f| back[*f] == Kind::Bishop).collect();
            assert_ne!(bishops[0] % 2, bishops[1] % 2, "opposite bishops in {n}");
        }
    }

    #[test]
    fn chess960_castling_with_the_king_already_on_g1() {
        // King on g1, rook on h1: castling moves only the rook.
        let p = Position::from_fen("4k3/8/8/8/8/8/8/6KR w H - 0 1").unwrap();
        let castle = p
            .legal_moves()
            .into_iter()
            .find(|m| m.castle == Some(CastleSide::King))
            .expect("O-O is legal");
        let after = p.play(&castle);
        assert_eq!(
            after.piece_at(parse_square("g1").unwrap()).unwrap().kind,
            Kind::King
        );
        assert_eq!(
            after.piece_at(parse_square("f1").unwrap()).unwrap().kind,
            Kind::Rook
        );
        assert!(after.piece_at(parse_square("h1").unwrap()).is_none());
    }

    #[test]
    fn san_disambiguates_and_marks_mate() {
        // Two knights can reach d2.
        let p = Position::from_fen("4k3/8/8/8/8/8/8/1N2KN2 w - - 0 1").unwrap();
        let m = p
            .legal_moves()
            .into_iter()
            .find(|m| m.from == parse_square("b1").unwrap() && m.to == parse_square("d2").unwrap())
            .unwrap();
        assert_eq!(p.san(&m), "Nbd2");

        // Back-rank mate.
        let p = Position::from_fen("6k1/5ppp/8/8/8/8/8/R5K1 w - - 0 1").unwrap();
        let m = p
            .legal_moves()
            .into_iter()
            .find(|m| m.to == parse_square("a8").unwrap())
            .unwrap();
        assert_eq!(p.san(&m), "Ra8#");
    }

    #[test]
    fn insufficient_material_cases() {
        let draw = |fen: &str| Position::from_fen(fen).unwrap().insufficient_material();
        assert!(draw("4k3/8/8/8/8/8/8/4K3 w - - 0 1"));
        assert!(draw("4k3/8/8/8/8/8/8/4KN2 w - - 0 1"));
        // Bishops on the same colour (c1 and f8 are both dark).
        assert!(draw("4kb2/8/8/8/8/8/8/2B1K3 w - - 0 1"));
        assert!(!draw("4k3/8/8/8/8/8/8/2B1KB2 w - - 0 1"));
        assert!(!draw("4k3/8/8/8/8/8/4P3/4K3 w - - 0 1"));
    }

    #[test]
    fn fen_round_trips() {
        for fen in [
            STANDARD_FEN,
            "r3k2r/p1ppqpb1/bn2pnp1/3PN3/1p2P3/2N2Q1p/PPPBBPPP/R3K2R w KQkq - 0 1",
            "4k3/8/8/8/8/8/8/6KR w K - 0 1",
            // A Chess960 rook that isn't on a or h is named by its file.
            "4k3/8/8/8/8/8/8/1R2K3 w B - 0 1",
        ] {
            assert_eq!(Position::from_fen(fen).unwrap().fen(), fen);
        }
    }
}
