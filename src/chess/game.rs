//! One game: its rules, its history, the clocks, and the offers on the table.
//!
//! Only the hosting daemon owns a [`Game`]. The guest's daemon holds the
//! [`Snapshot`]s the host sends it and forwards its player's [`Action`]s, so
//! there is exactly one copy of the truth and nothing to reconcile.

use std::time::Instant;

use rand::Rng;
use serde::{Deserialize, Serialize};

use super::rules::{Color, Kind, Move, Position, parse_square, square_name};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Variant {
    #[default]
    Standard,
    /// Fischer random: the back rank is shuffled, the same for both sides.
    Chess960,
    /// Also win by walking your king onto d4, e4, d5 or e5.
    KingOfTheHill,
    /// Also win by giving check three times.
    ThreeCheck,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ColorChoice {
    White,
    Black,
    #[default]
    Random,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimeControl {
    pub initial_secs: u32,
    pub increment_secs: u32,
}

/// Everything the host chooses when opening a game.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Rules {
    pub variant: Variant,
    /// `None` is an untimed game.
    pub time: Option<TimeControl>,
    /// Which side the host plays.
    pub host_color: ColorChoice,
    /// Whether a player may ask to take a move back.
    pub takebacks: bool,
    /// Whether a player may offer a draw.
    pub draw_offers: bool,
}

impl Default for Rules {
    fn default() -> Self {
        Self {
            variant: Variant::Standard,
            time: Some(TimeControl {
                initial_secs: 600,
                increment_secs: 5,
            }),
            host_color: ColorChoice::Random,
            takebacks: true,
            draw_offers: true,
        }
    }
}

/// Three hours a side and three minutes a move are far past any LAN game.
const MAX_INITIAL_SECS: u32 = 3 * 3600;
const MAX_INCREMENT_SECS: u32 = 180;

impl Rules {
    pub fn validate(&self) -> Result<(), String> {
        if let Some(time) = self.time {
            if time.initial_secs == 0 || time.initial_secs > MAX_INITIAL_SECS {
                return Err("Starting time must be between 1 second and 3 hours.".into());
            }
            if time.increment_secs > MAX_INCREMENT_SECS {
                return Err("The increment can be at most 3 minutes.".into());
            }
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Reason {
    Checkmate,
    Resignation,
    Timeout,
    /// Out of time, but the opponent has only a king and can't win.
    TimeoutVsInsufficient,
    Stalemate,
    Agreement,
    Repetition,
    FiftyMoves,
    InsufficientMaterial,
    KingOfTheHill,
    ThreeChecks,
    /// Called off before both sides had moved.
    Aborted,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    /// Open, waiting for someone to join.
    Waiting,
    Playing,
    Finished {
        winner: Option<Color>,
        reason: Reason,
    },
}

/// Something a player does.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Action {
    Move {
        from: String,
        to: String,
        #[serde(default)]
        promotion: Option<Kind>,
    },
    Resign,
    OfferDraw,
    AcceptDraw,
    DeclineDraw,
    RequestTakeback,
    AcceptTakeback,
    DeclineTakeback,
    OfferRematch,
    AcceptRematch,
    DeclineRematch,
    Abort,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Offers {
    pub draw: Option<Color>,
    pub takeback: Option<Color>,
    pub rematch: Option<Color>,
}

#[derive(Clone, Debug)]
struct Clock {
    /// Milliseconds left, indexed by [`Color::index`].
    remaining: [i64; 2],
    increment: i64,
    /// When the side to move's time started running. `None` until both
    /// players have made their first move, so nobody loses time while the
    /// other is still sitting down.
    since: Option<Instant>,
}

pub struct Game {
    pub id: String,
    pub rules: Rules,
    pub created_at: String,
    /// Resolved from [`Rules::host_color`] when the guest joins.
    pub host_color: Option<Color>,
    /// `positions[0]` is the start; one more per move played.
    positions: Vec<Position>,
    moves: Vec<Move>,
    sans: Vec<String>,
    pub status: Status,
    clock: Option<Clock>,
    pub offers: Offers,
    /// Bumped on every change, so watchers can tell a new state from an old one.
    pub version: u64,
}

/// A legal move as the web app needs it: where to click from and to.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LegalMove {
    pub from: String,
    pub to: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub promotion: Option<Kind>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub castle: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClockView {
    pub white_ms: i64,
    pub black_ms: i64,
    /// Whose time is running, if anyone's.
    pub running: Option<Color>,
    pub increment_ms: i64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Checks {
    pub white: u8,
    pub black: u8,
}

/// The whole visible state of a game, as the host sends it to the guest and
/// as either daemon hands it to its web app.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    pub id: String,
    pub rules: Rules,
    pub created_at: String,
    pub host_name: String,
    pub guest_name: Option<String>,
    pub host_color: Option<Color>,
    /// `waiting`, `playing` or `finished`.
    pub status: String,
    pub winner: Option<Color>,
    pub reason: Option<Reason>,
    /// `1-0`, `0-1` or `½-½` once finished.
    pub result: Option<String>,
    /// 64 piece codes, a1 first.
    pub board: Vec<String>,
    pub fen: String,
    pub turn: Color,
    pub check: bool,
    pub last_move: Option<[String; 2]>,
    pub moves: Vec<String>,
    pub legal: Vec<LegalMove>,
    pub clock: Option<ClockView>,
    pub offers: Offers,
    /// Checks each side has given, for three-check.
    pub checks: Checks,
    pub version: u64,
}

fn start_position(variant: Variant) -> Position {
    match variant {
        Variant::Chess960 => Position::chess960(rand::thread_rng().gen_range(0..960)),
        _ => Position::standard(),
    }
}

impl Game {
    pub fn new(id: String, rules: Rules) -> Self {
        let positions = vec![start_position(rules.variant)];
        Self {
            id,
            created_at: chrono::Local::now().to_rfc3339(),
            host_color: None,
            positions,
            moves: Vec::new(),
            sans: Vec::new(),
            status: Status::Waiting,
            clock: None,
            offers: Offers::default(),
            version: 1,
            rules,
        }
    }

    fn position(&self) -> &Position {
        self.positions.last().expect("a game always has a position")
    }

    /// Seat the guest and start the game.
    pub fn start(&mut self) {
        let host = match self.rules.host_color {
            ColorChoice::White => Color::White,
            ColorChoice::Black => Color::Black,
            ColorChoice::Random => {
                if rand::thread_rng().gen_bool(0.5) {
                    Color::White
                } else {
                    Color::Black
                }
            }
        };
        self.begin(host);
    }

    /// (Re)start from a fresh position with the host on `host`.
    fn begin(&mut self, host: Color) {
        self.host_color = Some(host);
        self.positions = vec![start_position(self.rules.variant)];
        self.moves.clear();
        self.sans.clear();
        self.status = Status::Playing;
        self.offers = Offers::default();
        self.clock = self.rules.time.map(|t| Clock {
            remaining: [t.initial_secs as i64 * 1000; 2],
            increment: t.increment_secs as i64 * 1000,
            since: None,
        });
        self.version += 1;
    }

    /// The colour the host or the guest plays.
    pub fn color_of(&self, host: bool) -> Option<Color> {
        self.host_color.map(|c| if host { c } else { c.other() })
    }

    fn finish(&mut self, winner: Option<Color>, reason: Reason) {
        self.status = Status::Finished { winner, reason };
        if let Some(clock) = &mut self.clock
            && let Some(since) = clock.since.take()
        {
            let turn = self.positions.last().expect("a position").turn;
            clock.remaining[turn.index()] -= since.elapsed().as_millis() as i64;
        }
        self.offers = Offers::default();
        self.version += 1;
    }

    fn remaining(&self, now: Instant) -> Option<[i64; 2]> {
        let clock = self.clock.as_ref()?;
        let mut left = clock.remaining;
        if let Some(since) = clock.since {
            let turn = self.position().turn;
            left[turn.index()] -= now.saturating_duration_since(since).as_millis() as i64;
        }
        Some(left)
    }

    /// End the game if the side to move has run out of time. Returns whether
    /// anything changed.
    pub fn tick(&mut self, now: Instant) -> bool {
        if self.status != Status::Playing {
            return false;
        }
        let Some(left) = self.remaining(now) else {
            return false;
        };
        let turn = self.position().turn;
        if left[turn.index()] > 0 {
            return false;
        }
        if let Some(clock) = &mut self.clock {
            clock.remaining[turn.index()] = 0;
            clock.since = None;
        }
        if self.position().bare_king(turn.other()) {
            self.finish(None, Reason::TimeoutVsInsufficient);
        } else {
            self.finish(Some(turn.other()), Reason::Timeout);
        }
        true
    }

    /// Apply what player `by` did.
    pub fn apply(&mut self, by: Color, action: Action, now: Instant) -> Result<(), String> {
        if let Action::OfferRematch | Action::AcceptRematch | Action::DeclineRematch = action {
            return self.rematch(by, action);
        }
        if self.tick(now) {
            return Err("Your clock ran out.".into());
        }
        if self.status != Status::Playing {
            return Err("This game isn't in progress.".into());
        }

        match action {
            Action::Move {
                from,
                to,
                promotion,
            } => self.play(by, &from, &to, promotion, now)?,
            Action::Resign => self.finish(Some(by.other()), Reason::Resignation),
            Action::Abort => {
                if self.moves.len() >= 2 {
                    return Err("A game can only be aborted before both sides have moved.".into());
                }
                self.finish(None, Reason::Aborted);
            }
            Action::OfferDraw | Action::AcceptDraw => {
                if !self.rules.draw_offers {
                    return Err("Draw offers are turned off for this game.".into());
                }
                if self.offers.draw == Some(by.other()) {
                    self.finish(None, Reason::Agreement);
                } else if action == Action::AcceptDraw {
                    return Err("There's no draw offer to accept.".into());
                } else {
                    self.offers.draw = Some(by);
                    self.version += 1;
                }
            }
            Action::DeclineDraw => {
                if self.offers.draw == Some(by.other()) {
                    self.offers.draw = None;
                    self.version += 1;
                }
            }
            Action::RequestTakeback => {
                if !self.rules.takebacks {
                    return Err("Takebacks are turned off for this game.".into());
                }
                if self.plies_to_take_back(by) == 0 {
                    return Err("You haven't made a move to take back.".into());
                }
                if self.offers.takeback == Some(by.other()) {
                    // Both want one: treat as the other's request accepted.
                    self.take_back(by.other(), now);
                } else {
                    self.offers.takeback = Some(by);
                    self.version += 1;
                }
            }
            Action::AcceptTakeback => {
                if self.offers.takeback != Some(by.other()) {
                    return Err("There's no takeback request to accept.".into());
                }
                self.take_back(by.other(), now);
            }
            Action::DeclineTakeback => {
                if self.offers.takeback == Some(by.other()) {
                    self.offers.takeback = None;
                    self.version += 1;
                }
            }
            Action::OfferRematch | Action::AcceptRematch | Action::DeclineRematch => unreachable!(),
        }
        Ok(())
    }

    fn rematch(&mut self, by: Color, action: Action) -> Result<(), String> {
        if !matches!(self.status, Status::Finished { .. }) {
            return Err("A rematch can only be offered once the game is over.".into());
        }
        match action {
            Action::OfferRematch | Action::AcceptRematch => {
                if self.offers.rematch == Some(by.other()) {
                    // Swap sides, as players expect from a rematch.
                    let host = self
                        .host_color
                        .expect("a finished game had players")
                        .other();
                    self.begin(host);
                } else if action == Action::AcceptRematch {
                    return Err("There's no rematch offer to accept.".into());
                } else {
                    self.offers.rematch = Some(by);
                    self.version += 1;
                }
            }
            _ => {
                if self.offers.rematch == Some(by.other()) {
                    self.offers.rematch = None;
                    self.version += 1;
                }
            }
        }
        Ok(())
    }

    /// How many plies undoing `by`'s last move means: one if they moved last,
    /// two if their opponent has replied since.
    fn plies_to_take_back(&self, by: Color) -> usize {
        let Some(last) = self.positions.len().checked_sub(2) else {
            return 0;
        };
        let last_mover = self.positions[last].turn;
        if last_mover == by {
            1
        } else if self.moves.len() >= 2 {
            2
        } else {
            0
        }
    }

    fn take_back(&mut self, by: Color, now: Instant) {
        let plies = self.plies_to_take_back(by);
        let thinking = self.position().turn;
        for _ in 0..plies {
            self.positions.pop();
            self.moves.pop();
            self.sans.pop();
        }
        if let Some(clock) = &mut self.clock {
            // Time already spent stays spent; the clock simply restarts for
            // whoever is now to move.
            if let Some(since) = clock.since.take() {
                clock.remaining[thinking.index()] -=
                    now.saturating_duration_since(since).as_millis() as i64;
            }
            clock.since = (self.moves.len() >= 2).then_some(now);
        }
        self.offers = Offers::default();
        self.version += 1;
    }

    /// The legal move a click from `from` to `to` means. A plain move wins
    /// over castling, so in Chess960 a king step and a castle onto the same
    /// square stay distinct: that castle is made by dropping on the rook.
    fn find_move(&self, from: &str, to: &str, promotion: Option<Kind>) -> Option<Move> {
        let from = parse_square(from)?;
        let to = parse_square(to)?;
        let legal = self.position().legal_moves();
        legal
            .iter()
            .find(|m| {
                m.castle.is_none() && m.from == from && m.to == to && m.promotion == promotion
            })
            .or_else(|| {
                legal
                    .iter()
                    .find(|m| m.castle.is_some() && m.from == from && m.to == to)
            })
            .or_else(|| {
                legal
                    .iter()
                    .find(|m| m.castle.is_some() && m.from == from && m.landing() == to)
            })
            .copied()
    }

    fn play(
        &mut self,
        by: Color,
        from: &str,
        to: &str,
        promotion: Option<Kind>,
        now: Instant,
    ) -> Result<(), String> {
        if self.position().turn != by {
            return Err("It isn't your move.".into());
        }
        let m = self
            .find_move(from, to, promotion)
            .ok_or_else(|| format!("{from}–{to} isn't a legal move."))?;

        if let Some(clock) = &mut self.clock
            && let Some(since) = clock.since
        {
            clock.remaining[by.index()] -= now.saturating_duration_since(since).as_millis() as i64;
            clock.remaining[by.index()] += clock.increment;
        }

        let san = self.position().san(&m);
        let next = self.position().play(&m);
        self.positions.push(next);
        self.moves.push(m);
        self.sans.push(san);

        if let Some(clock) = &mut self.clock {
            clock.since = (self.moves.len() >= 2).then_some(now);
        }
        // Moving on is declining whatever the opponent had offered, and any
        // takeback request is moot once the board has changed.
        if self.offers.draw == Some(by.other()) {
            self.offers.draw = None;
        }
        self.offers.takeback = None;
        self.version += 1;

        self.check_end(by);
        Ok(())
    }

    /// Decide whether the move `mover` just made ended the game.
    fn check_end(&mut self, mover: Color) {
        let position = self.position().clone();
        let variant = self.rules.variant;

        if variant == Variant::KingOfTheHill && position.king_on_hill(mover) {
            return self.finish(Some(mover), Reason::KingOfTheHill);
        }
        if variant == Variant::ThreeCheck && self.checks_given(mover) >= 3 {
            return self.finish(Some(mover), Reason::ThreeChecks);
        }
        if position.legal_moves().is_empty() {
            return if position.in_check() {
                self.finish(Some(mover), Reason::Checkmate)
            } else {
                self.finish(None, Reason::Stalemate)
            };
        }
        // In King of the Hill a lone king can still win by walking to the
        // centre, so there is no such thing as insufficient material there.
        if variant != Variant::KingOfTheHill && position.insufficient_material() {
            return self.finish(None, Reason::InsufficientMaterial);
        }
        if position.halfmove >= 100 {
            return self.finish(None, Reason::FiftyMoves);
        }
        let key = position.repetition_key();
        if self
            .positions
            .iter()
            .filter(|p| p.repetition_key() == key)
            .count()
            >= 3
        {
            self.finish(None, Reason::Repetition);
        }
    }

    /// How many times `color` has given check.
    fn checks_given(&self, color: Color) -> u8 {
        self.positions[1..]
            .iter()
            .filter(|p| p.turn == color.other() && p.in_check())
            .count() as u8
    }

    pub fn snapshot(&self, host_name: &str, guest_name: Option<&str>, now: Instant) -> Snapshot {
        let position = self.position();
        let (status, winner, reason) = match self.status {
            Status::Waiting => ("waiting", None, None),
            Status::Playing => ("playing", None, None),
            Status::Finished { winner, reason } => ("finished", winner, Some(reason)),
        };
        let result = matches!(self.status, Status::Finished { .. }).then(|| {
            match winner {
                Some(Color::White) => "1-0",
                Some(Color::Black) => "0-1",
                None => "½-½",
            }
            .to_string()
        });

        let mut legal = Vec::new();
        if self.status == Status::Playing {
            let moves = position.legal_moves();
            for m in &moves {
                let from = square_name(m.from);
                if m.castle.is_some() {
                    // Offer both the rook's square and the king's landing
                    // square as targets, unless a plain king move already
                    // goes there — see `find_move`.
                    legal.push(LegalMove {
                        from: from.clone(),
                        to: square_name(m.to),
                        promotion: None,
                        castle: true,
                    });
                    let landing = m.landing();
                    let clashes = landing == m.from
                        || landing == m.to
                        || moves
                            .iter()
                            .any(|o| o.castle.is_none() && o.from == m.from && o.to == landing);
                    if !clashes {
                        legal.push(LegalMove {
                            from,
                            to: square_name(landing),
                            promotion: None,
                            castle: true,
                        });
                    }
                } else {
                    legal.push(LegalMove {
                        from,
                        to: square_name(m.to),
                        promotion: m.promotion,
                        castle: false,
                    });
                }
            }
        }

        let clock = self.remaining(now).map(|left| ClockView {
            white_ms: left[0].max(0),
            black_ms: left[1].max(0),
            running: self
                .clock
                .as_ref()
                .and_then(|c| c.since)
                .map(|_| position.turn),
            increment_ms: self.clock.as_ref().map(|c| c.increment).unwrap_or(0),
        });

        Snapshot {
            id: self.id.clone(),
            rules: self.rules.clone(),
            created_at: self.created_at.clone(),
            host_name: host_name.to_string(),
            guest_name: guest_name.map(str::to_string),
            host_color: self.host_color,
            status: status.to_string(),
            winner,
            reason,
            result,
            board: position.board_codes(),
            fen: position.fen(),
            turn: position.turn,
            check: position.in_check(),
            last_move: self
                .moves
                .last()
                .map(|m| [square_name(m.from), square_name(m.landing())]),
            moves: self.sans.clone(),
            legal,
            clock,
            offers: self.offers,
            checks: Checks {
                white: self.checks_given(Color::White),
                black: self.checks_given(Color::Black),
            },
            version: self.version,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn untimed(variant: Variant) -> Game {
        let mut game = Game::new(
            "g".into(),
            Rules {
                variant,
                time: None,
                host_color: ColorChoice::White,
                ..Rules::default()
            },
        );
        game.start();
        game
    }

    fn mv(game: &mut Game, by: Color, from: &str, to: &str) -> Result<(), String> {
        game.apply(
            by,
            Action::Move {
                from: from.into(),
                to: to.into(),
                promotion: None,
            },
            Instant::now(),
        )
    }

    #[test]
    fn fools_mate_ends_the_game() {
        let mut g = untimed(Variant::Standard);
        mv(&mut g, Color::White, "f2", "f3").unwrap();
        mv(&mut g, Color::Black, "e7", "e5").unwrap();
        mv(&mut g, Color::White, "g2", "g4").unwrap();
        mv(&mut g, Color::Black, "d8", "h4").unwrap();
        assert_eq!(
            g.status,
            Status::Finished {
                winner: Some(Color::Black),
                reason: Reason::Checkmate
            }
        );
        let snap = g.snapshot("host", Some("guest"), Instant::now());
        assert_eq!(snap.moves.last().unwrap(), "Qh4#");
        assert_eq!(snap.result.as_deref(), Some("0-1"));
        assert!(snap.legal.is_empty());
    }

    #[test]
    fn players_can_only_move_on_their_turn() {
        let mut g = untimed(Variant::Standard);
        assert!(mv(&mut g, Color::Black, "e7", "e5").is_err());
        assert!(mv(&mut g, Color::White, "e2", "e5").is_err());
        mv(&mut g, Color::White, "e2", "e4").unwrap();
    }

    #[test]
    fn castling_by_king_square_or_rook_square() {
        let mut g = untimed(Variant::Standard);
        for (by, from, to) in [
            (Color::White, "e2", "e4"),
            (Color::Black, "e7", "e5"),
            (Color::White, "g1", "f3"),
            (Color::Black, "g8", "f6"),
            (Color::White, "f1", "c4"),
            (Color::Black, "f8", "c5"),
        ] {
            mv(&mut g, by, from, to).unwrap();
        }
        mv(&mut g, Color::White, "e1", "g1").unwrap();
        mv(&mut g, Color::Black, "e8", "h8").unwrap();
        let snap = g.snapshot("h", None, Instant::now());
        assert_eq!(&snap.moves[6..], ["O-O", "O-O"]);
        assert_eq!(snap.board[6], "wK");
        assert_eq!(snap.board[5], "wR");
    }

    #[test]
    fn takeback_undoes_the_requesters_last_move() {
        let mut g = untimed(Variant::Standard);
        mv(&mut g, Color::White, "e2", "e4").unwrap();
        mv(&mut g, Color::Black, "e7", "e5").unwrap();
        let now = Instant::now();
        g.apply(Color::White, Action::RequestTakeback, now).unwrap();
        assert_eq!(g.offers.takeback, Some(Color::White));
        g.apply(Color::Black, Action::AcceptTakeback, now).unwrap();
        // White's e4 and black's reply both go.
        assert!(g.moves.is_empty());
        assert_eq!(g.position().turn, Color::White);
    }

    #[test]
    fn takebacks_and_draws_respect_the_rules() {
        let mut g = Game::new(
            "g".into(),
            Rules {
                time: None,
                takebacks: false,
                draw_offers: false,
                host_color: ColorChoice::White,
                ..Rules::default()
            },
        );
        g.start();
        mv(&mut g, Color::White, "e2", "e4").unwrap();
        let now = Instant::now();
        assert!(g.apply(Color::White, Action::RequestTakeback, now).is_err());
        assert!(g.apply(Color::White, Action::OfferDraw, now).is_err());
    }

    #[test]
    fn a_draw_needs_both_sides() {
        let mut g = untimed(Variant::Standard);
        let now = Instant::now();
        g.apply(Color::White, Action::OfferDraw, now).unwrap();
        assert_eq!(g.status, Status::Playing);
        g.apply(Color::Black, Action::AcceptDraw, now).unwrap();
        assert_eq!(
            g.status,
            Status::Finished {
                winner: None,
                reason: Reason::Agreement
            }
        );
    }

    #[test]
    fn rematch_swaps_colours() {
        let mut g = untimed(Variant::Standard);
        let now = Instant::now();
        g.apply(Color::White, Action::Resign, now).unwrap();
        g.apply(Color::Black, Action::OfferRematch, now).unwrap();
        g.apply(Color::White, Action::AcceptRematch, now).unwrap();
        assert_eq!(g.status, Status::Playing);
        assert_eq!(g.host_color, Some(Color::Black));
    }

    #[test]
    fn repetition_draws() {
        let mut g = untimed(Variant::Standard);
        for _ in 0..2 {
            mv(&mut g, Color::White, "g1", "f3").unwrap();
            mv(&mut g, Color::Black, "g8", "f6").unwrap();
            mv(&mut g, Color::White, "f3", "g1").unwrap();
            mv(&mut g, Color::Black, "f6", "g8").unwrap();
        }
        assert_eq!(
            g.status,
            Status::Finished {
                winner: None,
                reason: Reason::Repetition
            }
        );
    }

    #[test]
    fn three_checks_win() {
        let mut g = untimed(Variant::ThreeCheck);
        for (by, from, to) in [
            (Color::White, "e2", "e4"),
            (Color::Black, "f7", "f6"),
            (Color::White, "d1", "h5"), // check 1
            (Color::Black, "g7", "g6"),
            (Color::White, "h5", "g6"), // check 2
            (Color::Black, "h7", "g6"),
            (Color::White, "f1", "c4"),
            (Color::Black, "a7", "a6"),
            (Color::White, "c4", "g8"),
            (Color::Black, "h8", "g8"),
            (Color::White, "b1", "c3"),
            (Color::Black, "a6", "a5"),
            (Color::White, "c3", "d5"),
            (Color::Black, "a5", "a4"),
            (Color::White, "d5", "c7"), // check 3
        ] {
            mv(&mut g, by, from, to).unwrap();
        }
        assert_eq!(
            g.status,
            Status::Finished {
                winner: Some(Color::White),
                reason: Reason::ThreeChecks
            }
        );
    }

    #[test]
    fn king_of_the_hill_wins_in_the_centre() {
        let mut g = untimed(Variant::KingOfTheHill);
        for (by, from, to) in [
            (Color::White, "e2", "e4"),
            (Color::Black, "a7", "a6"),
            (Color::White, "e1", "e2"),
            (Color::Black, "a6", "a5"),
            (Color::White, "e2", "e3"),
            (Color::Black, "a5", "a4"),
            (Color::White, "e3", "d4"),
        ] {
            mv(&mut g, by, from, to).unwrap();
        }
        assert_eq!(
            g.status,
            Status::Finished {
                winner: Some(Color::White),
                reason: Reason::KingOfTheHill
            }
        );
    }

    #[test]
    fn the_clock_waits_for_both_first_moves_then_flags() {
        let mut g = Game::new(
            "g".into(),
            Rules {
                time: Some(TimeControl {
                    initial_secs: 1,
                    increment_secs: 0,
                }),
                host_color: ColorChoice::White,
                ..Rules::default()
            },
        );
        g.start();
        let t0 = Instant::now();
        let later = t0 + std::time::Duration::from_secs(5);
        // Nobody's clock runs before both sides have moved.
        assert!(!g.tick(later));
        mv(&mut g, Color::White, "e2", "e4").unwrap();
        mv(&mut g, Color::Black, "e7", "e5").unwrap();
        assert!(g.tick(Instant::now() + std::time::Duration::from_secs(2)));
        assert_eq!(
            g.status,
            Status::Finished {
                winner: Some(Color::Black),
                reason: Reason::Timeout
            }
        );
    }

    #[test]
    fn abort_only_before_both_have_moved() {
        let mut g = untimed(Variant::Standard);
        mv(&mut g, Color::White, "e2", "e4").unwrap();
        mv(&mut g, Color::Black, "e7", "e5").unwrap();
        assert!(
            g.apply(Color::White, Action::Abort, Instant::now())
                .is_err()
        );

        let mut g = untimed(Variant::Standard);
        mv(&mut g, Color::White, "e2", "e4").unwrap();
        g.apply(Color::Black, Action::Abort, Instant::now())
            .unwrap();
        assert_eq!(
            g.status,
            Status::Finished {
                winner: None,
                reason: Reason::Aborted
            }
        );
    }

    #[test]
    fn rules_validate_time_limits() {
        let mut rules = Rules::default();
        assert!(rules.validate().is_ok());
        rules.time = Some(TimeControl {
            initial_secs: 0,
            increment_secs: 0,
        });
        assert!(rules.validate().is_err());
    }
}
