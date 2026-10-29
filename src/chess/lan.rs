//! Playing over the LAN: finding other ciabattas, and the connection two of
//! them share for one game.
//!
//! # Discovery
//!
//! A daemon hosting an open game broadcasts a small JSON beacon over UDP every
//! couple of seconds — its display name, the TCP port it listens on, and the
//! games still looking for an opponent. Every daemon that has had the chess
//! page opened listens for those beacons and lists what it hears. Nothing is
//! sent or listened for until someone visits `/chess`.
//!
//! # The game connection
//!
//! Joining is a TCP connection to the host carrying newline-delimited JSON.
//! The first line is the handshake: `join` names a game, and the host seats
//! the first caller and answers with a fresh 256-bit secret. From then on the
//! game is closed — it drops out of the beacon, and any other `join` is
//! refused. Only a caller presenting the secret (`resume`, after a dropped
//! connection) gets back in, so the two players are the only ones who can
//! ever move.
//!
//! The host's [`Game`] is the only copy of the truth. The guest sends
//! [`Action`]s; the host applies them as the guest's colour, answers each with
//! an `ack`, and pushes a fresh [`Snapshot`] to the guest after every change.
//!
//! The listener is reachable from the network on purpose, but it speaks only
//! this protocol: lines are capped, a connection must handshake promptly, and
//! the number of connections is bounded. It cannot reach any other daemon
//! route.

use std::collections::HashMap;
use std::io;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, SocketAddrV4};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::{TcpListener, TcpStream, UdpSocket};
use tokio::sync::{OnceCell, Semaphore, mpsc, oneshot, watch};

use super::game::{Action, Game, Rules, Snapshot, Status};
use super::rules::Color;

/// Bumped whenever the wire format changes incompatibly.
pub const PROTOCOL: u32 = 1;

/// UDP port beacons are sent to and heard on.
pub const DISCOVERY_PORT_ENV: &str = "CIABATTA_CHESS_DISCOVERY_PORT";
pub const DEFAULT_DISCOVERY_PORT: u16 = 47_651;
/// TCP port the host listens on. Fixed by default so a firewall rule can let
/// it in; if it's taken (a second daemon on the same machine) and wasn't asked
/// for explicitly, any free port is used instead — the beacon carries it.
pub const PORT_ENV: &str = "CIABATTA_CHESS_PORT";
pub const DEFAULT_PORT: u16 = 47_652;
/// Comma-separated addresses to send beacons to, for networks where the
/// limited broadcast address doesn't reach everyone (e.g. `192.168.1.255`).
pub const BROADCAST_ENV: &str = "CIABATTA_CHESS_BROADCAST";

const BEACON_EVERY: Duration = Duration::from_secs(2);
/// A peer that hasn't been heard from in this long has gone away.
const PEER_TTL: Duration = Duration::from_secs(7);
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
/// Both ends ping every [`PING_EVERY`]; silence for this long is a dead link.
const IDLE_TIMEOUT: Duration = Duration::from_secs(30);
const PING_EVERY: Duration = Duration::from_secs(10);
const RECONNECT_EVERY: Duration = Duration::from_secs(2);
const ACK_TIMEOUT: Duration = Duration::from_secs(5);
/// A full snapshot is a few kilobytes; nothing legitimate comes near this.
const MAX_LINE: u64 = 256 * 1024;
const MAX_CONNECTIONS: usize = 64;
const MAX_HOSTED: usize = 16;
const MAX_PEERS: usize = 256;
pub const MAX_NAME: usize = 40;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpenGame {
    pub id: String,
    pub rules: Rules,
    pub created_at: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct Beacon {
    ciabatta_chess: u32,
    instance: String,
    name: String,
    version: String,
    port: u16,
    games: Vec<OpenGame>,
}

/// Lines a guest sends a host.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
enum ToHost {
    /// List the open games, then hang up — for joining by address.
    Hello {
        proto: u32,
    },
    Join {
        proto: u32,
        game: String,
        name: String,
    },
    Resume {
        proto: u32,
        game: String,
        secret: String,
    },
    Act {
        seq: u64,
        action: Action,
    },
    Ping,
    Bye,
}

/// Lines a host sends a guest.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
enum ToGuest {
    Games {
        name: String,
        games: Vec<OpenGame>,
    },
    Joined {
        secret: String,
        snapshot: Box<Snapshot>,
    },
    Rejected {
        reason: String,
    },
    State {
        snapshot: Box<Snapshot>,
    },
    Ack {
        seq: u64,
        error: Option<String>,
    },
    Closed {
        reason: String,
    },
    Ping,
}

/// What the web app sees of one game, from this daemon's side of the board.
#[derive(Clone, Debug, Serialize)]
pub struct GameView {
    #[serde(flatten)]
    pub snapshot: Snapshot,
    /// `host` or `guest`.
    pub role: &'static str,
    pub you: Option<Color>,
    pub opponent: Option<String>,
    /// Whether the other daemon is connected right now.
    pub connected: bool,
    /// The host's address, for a game this daemon joined.
    pub address: Option<String>,
    /// Set when the host closed the game for good.
    pub closed: Option<String>,
    /// This daemon's own change counter for the game, for long-polling.
    pub rev: u64,
}

#[derive(Clone, Debug, Serialize)]
pub struct PeerView {
    pub instance: String,
    pub name: String,
    pub address: String,
    pub version: String,
    pub games: Vec<OpenGame>,
}

#[derive(Clone, Debug, Serialize)]
pub struct Lobby {
    pub name: String,
    pub hostname: String,
    pub version: &'static str,
    /// The TCP port games are hosted on, once something has been hosted.
    pub port: Option<u16>,
    /// `ip:port` others can join by, once something has been hosted.
    pub address: Option<String>,
    pub discovery_port: u16,
    pub discovery_error: Option<String>,
    pub peers: Vec<PeerView>,
    pub games: Vec<GameView>,
}

struct Guest {
    name: String,
    secret: String,
    address: String,
    /// The live connection: its id, and where to send it lines.
    link: Option<(u64, mpsc::UnboundedSender<ToGuest>)>,
}

/// A guest's place at a game, as the handshake hands it to the connection.
struct Seat {
    secret: String,
    snapshot: Snapshot,
    /// This connection's id, so a newer connection can supersede it.
    conn: u64,
    tx: mpsc::UnboundedSender<ToGuest>,
    rx: mpsc::UnboundedReceiver<ToGuest>,
}

struct Hosted {
    game: Game,
    guest: Option<Guest>,
    rev: u64,
}

struct Joined {
    address: String,
    secret: String,
    snapshot: Snapshot,
    /// When `snapshot` arrived, so a running clock can be advanced locally.
    received: Instant,
    link: Option<mpsc::UnboundedSender<ToHost>>,
    pending: HashMap<u64, oneshot::Sender<Option<String>>>,
    closed: Option<String>,
    rev: u64,
}

struct Peer {
    name: String,
    address: IpAddr,
    port: u16,
    version: String,
    games: Vec<OpenGame>,
    seen: Instant,
}

struct State {
    name: String,
    hosted: HashMap<String, Hosted>,
    joined: HashMap<String, Joined>,
    peers: HashMap<String, Peer>,
}

/// The daemon's chess side: every game it hosts or has joined, and the peers
/// it has heard from.
pub struct Lan {
    instance: String,
    hostname: String,
    discovery_port: u16,
    state: Mutex<State>,
    changed: watch::Sender<u64>,
    discovery: OnceCell<Result<(), String>>,
    listener: OnceCell<Result<u16, String>>,
    next_id: AtomicU64,
    connections: Arc<Semaphore>,
    closing: AtomicBool,
}

/// Why connecting to a host failed: worth retrying, or refused for good.
enum LinkError {
    Transient(String),
    Refused(String),
}

impl Lan {
    /// Cheap: nothing touches the network until the chess page is used.
    pub fn new() -> Arc<Self> {
        let hostname = hostname();
        let name = super::load_name().unwrap_or_else(|| hostname.clone());
        Arc::new(Self {
            instance: uuid::Uuid::new_v4().to_string(),
            hostname,
            discovery_port: env_port(DISCOVERY_PORT_ENV).unwrap_or(DEFAULT_DISCOVERY_PORT),
            state: Mutex::new(State {
                name,
                hosted: HashMap::new(),
                joined: HashMap::new(),
                peers: HashMap::new(),
            }),
            changed: watch::channel(0).0,
            discovery: OnceCell::new(),
            listener: OnceCell::new(),
            next_id: AtomicU64::new(1),
            connections: Arc::new(Semaphore::new(MAX_CONNECTIONS)),
            closing: AtomicBool::new(false),
        })
    }

    fn next_id(&self) -> u64 {
        self.next_id.fetch_add(1, Ordering::Relaxed)
    }

    /// Wake every long-poll so it can check whether its game changed.
    fn touch(&self) {
        self.changed.send_modify(|v| *v += 1);
    }

    pub fn subscribe(&self) -> watch::Receiver<u64> {
        self.changed.subscribe()
    }

    /// The daemon is stopping: answer every long-poll now, so an open chess
    /// tab doesn't hold the shutdown up for a whole poll.
    pub fn close(&self) {
        self.closing.store(true, Ordering::Relaxed);
        self.touch();
    }

    pub fn is_closing(&self) -> bool {
        self.closing.load(Ordering::Relaxed)
    }

    pub fn set_name(&self, name: &str) -> Result<String, String> {
        let name = clean_name(name);
        if name.is_empty() {
            return Err("A name can't be empty.".into());
        }
        super::save_name(&name).map_err(|e| format!("{e:#}"))?;
        let mut state = self.state.lock().unwrap();
        state.name = name.clone();
        // The guests' snapshots carry the host's name, so they need the new one.
        let State {
            hosted, name: n, ..
        } = &mut *state;
        for game in hosted.values_mut() {
            push(game, n);
        }
        drop(state);
        self.touch();
        Ok(name)
    }

    // --- Discovery -----------------------------------------------------------

    /// Start listening for beacons (and sending ours while we host), once.
    pub async fn ensure_discovery(self: &Arc<Self>) -> Result<(), String> {
        self.discovery
            .get_or_init(|| async {
                let socket = bind_discovery(self.discovery_port)
                    .and_then(UdpSocket::from_std)
                    .map_err(|e| {
                        format!(
                            "Couldn't listen for other players on UDP port {}: {e}. \
                             Set {DISCOVERY_PORT_ENV} to use another port, or join by address.",
                            self.discovery_port
                        )
                    })?;
                let socket = Arc::new(socket);
                tokio::spawn(hear_beacons(self.clone(), socket.clone()));
                tokio::spawn(send_beacons(self.clone(), socket));
                Ok(())
            })
            .await
            .clone()
    }

    fn beacon(&self, port: u16) -> Beacon {
        let state = self.state.lock().unwrap();
        Beacon {
            ciabatta_chess: PROTOCOL,
            instance: self.instance.clone(),
            name: state.name.clone(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            port,
            games: open_games(&state),
        }
    }

    fn heard(&self, beacon: Beacon, from: SocketAddr) {
        if beacon.instance == self.instance || beacon.ciabatta_chess != PROTOCOL {
            return;
        }
        let mut state = self.state.lock().unwrap();
        if beacon.games.is_empty() {
            // The host's last open game was taken or closed.
            if state.peers.remove(&beacon.instance).is_some() {
                drop(state);
                self.touch();
            }
            return;
        }
        if !state.peers.contains_key(&beacon.instance) && state.peers.len() >= MAX_PEERS {
            return;
        }
        let changed = state.peers.get(&beacon.instance).is_none_or(|p| {
            p.games != beacon.games || p.name != beacon.name || p.address != from.ip()
        });
        state.peers.insert(
            beacon.instance,
            Peer {
                name: clean_name(&beacon.name),
                address: from.ip(),
                port: beacon.port,
                version: beacon.version.chars().take(32).collect(),
                games: beacon.games.into_iter().take(MAX_HOSTED).collect(),
                seen: Instant::now(),
            },
        );
        drop(state);
        if changed {
            self.touch();
        }
    }

    // --- Hosting -------------------------------------------------------------

    /// Bind the game listener and start the clock ticker, once.
    async fn ensure_listener(self: &Arc<Self>) -> Result<u16, String> {
        self.listener
            .get_or_init(|| async {
                let chosen = env_port(PORT_ENV);
                let port = chosen.unwrap_or(DEFAULT_PORT);
                let bind = |port| TcpListener::bind(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, port));
                let listener = match bind(port).await {
                    Ok(listener) => Ok(listener),
                    Err(_) if chosen.is_none() => bind(0).await,
                    Err(e) => Err(e),
                }
                .map_err(|e| format!("Couldn't open TCP port {port} for chess games: {e}"))?;
                let port = listener.local_addr().map_err(|e| e.to_string())?.port();
                tracing::info!("hosting chess games on TCP port {port}");
                tokio::spawn(accept(self.clone(), listener));
                tokio::spawn(tick_clocks(self.clone()));
                Ok(port)
            })
            .await
            .clone()
    }

    fn port(&self) -> Option<u16> {
        self.listener.get().and_then(|r| r.as_ref().ok().copied())
    }

    pub async fn create(self: &Arc<Self>, rules: Rules) -> Result<GameView, String> {
        rules.validate()?;
        // Discovery failing (say, the UDP port is taken) shouldn't stop anyone
        // hosting: a game can still be joined by address.
        let _ = self.ensure_discovery().await;
        self.ensure_listener().await?;

        let id = super::new_id();
        {
            let mut state = self.state.lock().unwrap();
            if state.hosted.len() >= MAX_HOSTED {
                return Err(format!("You can host at most {MAX_HOSTED} games at once."));
            }
            state.hosted.insert(
                id.clone(),
                Hosted {
                    game: Game::new(id.clone(), rules),
                    guest: None,
                    rev: 1,
                },
            );
        }
        self.touch();
        self.view(&id).ok_or_else(|| "The game vanished.".into())
    }

    /// Seat the first caller in an open game.
    fn seat(&self, id: &str, name: &str, from: SocketAddr) -> Result<Seat, String> {
        let mut state = self.state.lock().unwrap();
        let host_name = state.name.clone();
        let hosted = state
            .hosted
            .get_mut(id)
            .ok_or("There's no such game here any more.")?;
        if hosted.guest.is_some() || hosted.game.status != Status::Waiting {
            return Err("This game already has two players.".into());
        }
        hosted.game.start();
        let secret = crate::daemon::auth::generate_token();
        let (tx, rx) = mpsc::unbounded_channel();
        let conn = self.next_id();
        let name = clean_name(name);
        hosted.guest = Some(Guest {
            name: if name.is_empty() {
                "Guest".into()
            } else {
                name
            },
            secret: secret.clone(),
            address: from.ip().to_string(),
            link: Some((conn, tx.clone())),
        });
        hosted.rev += 1;
        let snapshot = snapshot_of(hosted, &host_name);
        drop(state);
        self.touch();
        tracing::info!("chess game {id}: {from} joined");
        Ok(Seat {
            secret,
            snapshot,
            conn,
            tx,
            rx,
        })
    }

    /// Let a seated guest back in on a fresh connection.
    fn resume(&self, id: &str, secret: &str) -> Result<Seat, String> {
        let mut state = self.state.lock().unwrap();
        let host_name = state.name.clone();
        let hosted = state
            .hosted
            .get_mut(id)
            .ok_or("There's no such game here any more.")?;
        let guest = hosted
            .guest
            .as_mut()
            .filter(|g| crate::daemon::auth::constant_time_eq(&g.secret, secret))
            .ok_or("That isn't your seat.")?;
        let (tx, rx) = mpsc::unbounded_channel();
        let conn = self.next_id();
        // Replacing the old sender ends the old connection's loop.
        guest.link = Some((conn, tx.clone()));
        hosted.rev += 1;
        let snapshot = snapshot_of(hosted, &host_name);
        drop(state);
        self.touch();
        Ok(Seat {
            secret: secret.to_string(),
            snapshot,
            conn,
            tx,
            rx,
        })
    }

    /// Apply an action the guest sent over connection `conn`.
    fn guest_act(&self, id: &str, conn: u64, action: Action) -> Result<(), String> {
        let mut state = self.state.lock().unwrap();
        let State { hosted, name, .. } = &mut *state;
        let hosted = hosted.get_mut(id).ok_or("This game is gone.")?;
        let current = hosted
            .guest
            .as_ref()
            .and_then(|g| g.link.as_ref())
            .is_some_and(|(c, _)| *c == conn);
        if !current {
            return Err("This connection was replaced.".into());
        }
        let color = hosted
            .game
            .color_of(false)
            .ok_or("The game hasn't started.")?;
        let result = hosted.game.apply(color, action, Instant::now());
        push(hosted, name);
        drop(state);
        self.touch();
        result
    }

    fn guest_gone(&self, id: &str, conn: u64) {
        let mut state = self.state.lock().unwrap();
        let Some(hosted) = state.hosted.get_mut(id) else {
            return;
        };
        if let Some(guest) = &mut hosted.guest
            && guest.link.as_ref().is_some_and(|(c, _)| *c == conn)
        {
            guest.link = None;
            hosted.rev += 1;
            drop(state);
            self.touch();
        }
    }

    fn open_games(&self) -> (String, Vec<OpenGame>) {
        let state = self.state.lock().unwrap();
        (state.name.clone(), open_games(&state))
    }

    // --- Joining -------------------------------------------------------------

    /// Ask a host which games it has open.
    pub async fn browse(&self, address: &str) -> Result<(String, Vec<OpenGame>), String> {
        let (mut reader, mut write) = connect(address).await.map_err(|e| e.message())?;
        write_line(&mut write, &ToHost::Hello { proto: PROTOCOL })
            .await
            .map_err(|e| e.to_string())?;
        match read_reply(&mut reader).await.map_err(|e| e.message())? {
            ToGuest::Games { name, games } => Ok((clean_name(&name), games)),
            ToGuest::Rejected { reason } => Err(reason),
            _ => Err("The host answered with something unexpected.".into()),
        }
    }

    pub async fn join(self: &Arc<Self>, address: &str, id: &str) -> Result<GameView, String> {
        {
            let state = self.state.lock().unwrap();
            if state.hosted.contains_key(id) {
                return Err("That's your own game.".into());
            }
            if state.joined.contains_key(id) {
                return self
                    .view_locked(&state, id)
                    .ok_or_else(|| "The game vanished.".into());
            }
        }
        let address = normalize_address(address)?;
        let name = self.state.lock().unwrap().name.clone();
        let (mut reader, mut write) = connect(&address).await.map_err(|e| e.message())?;
        write_line(
            &mut write,
            &ToHost::Join {
                proto: PROTOCOL,
                game: id.to_string(),
                name,
            },
        )
        .await
        .map_err(|e| e.to_string())?;

        let (secret, snapshot) = match read_reply(&mut reader).await.map_err(|e| e.message())? {
            ToGuest::Joined { secret, snapshot } => (secret, *snapshot),
            ToGuest::Rejected { reason } => return Err(reason),
            _ => return Err("The host answered with something unexpected.".into()),
        };
        if snapshot.id != id {
            return Err("The host answered for a different game.".into());
        }

        let (tx, rx) = mpsc::unbounded_channel();
        self.state.lock().unwrap().joined.insert(
            id.to_string(),
            Joined {
                address,
                secret,
                snapshot,
                received: Instant::now(),
                link: Some(tx),
                pending: HashMap::new(),
                closed: None,
                rev: 1,
            },
        );
        self.touch();
        tokio::spawn(guest_task(self.clone(), id.to_string(), reader, write, rx));
        self.view(id).ok_or_else(|| "The game vanished.".into())
    }

    /// Connect again with our secret after the link dropped.
    async fn reconnect(
        &self,
        id: &str,
    ) -> Result<
        (
            BufReader<OwnedReadHalf>,
            OwnedWriteHalf,
            mpsc::UnboundedReceiver<ToHost>,
        ),
        LinkError,
    > {
        let (address, secret) = {
            let state = self.state.lock().unwrap();
            let joined = state
                .joined
                .get(id)
                .ok_or_else(|| LinkError::Refused("left".into()))?;
            (joined.address.clone(), joined.secret.clone())
        };
        let (mut reader, mut write) = connect(&address).await?;
        write_line(
            &mut write,
            &ToHost::Resume {
                proto: PROTOCOL,
                game: id.to_string(),
                secret,
            },
        )
        .await
        .map_err(|e| LinkError::Transient(e.to_string()))?;
        let snapshot = match read_reply(&mut reader).await? {
            ToGuest::Joined { snapshot, .. } => *snapshot,
            ToGuest::Rejected { reason } => return Err(LinkError::Refused(reason)),
            _ => return Err(LinkError::Transient("unexpected reply".into())),
        };
        let (tx, rx) = mpsc::unbounded_channel();
        let mut state = self.state.lock().unwrap();
        let joined = state
            .joined
            .get_mut(id)
            .ok_or_else(|| LinkError::Refused("left".into()))?;
        joined.snapshot = snapshot;
        joined.received = Instant::now();
        joined.link = Some(tx);
        joined.rev += 1;
        drop(state);
        self.touch();
        Ok((reader, write, rx))
    }

    fn guest_update(&self, id: &str, snapshot: Snapshot) {
        let mut state = self.state.lock().unwrap();
        if let Some(joined) = state.joined.get_mut(id)
            && snapshot.id == id
        {
            joined.snapshot = snapshot;
            joined.received = Instant::now();
            joined.rev += 1;
            drop(state);
            self.touch();
        }
    }

    fn guest_ack(&self, id: &str, seq: u64, error: Option<String>) {
        let mut state = self.state.lock().unwrap();
        if let Some(reply) = state
            .joined
            .get_mut(id)
            .and_then(|j| j.pending.remove(&seq))
        {
            let _ = reply.send(error);
        }
    }

    /// Record why the link ended. `closed` is the host's reason when it shut
    /// the game for good.
    fn guest_disconnected(&self, id: &str, closed: Option<String>) {
        let mut state = self.state.lock().unwrap();
        if let Some(joined) = state.joined.get_mut(id) {
            joined.link = None;
            joined.pending.clear();
            if closed.is_some() {
                joined.closed = closed;
            }
            joined.rev += 1;
            drop(state);
            self.touch();
        }
    }

    fn still_joined(&self, id: &str) -> bool {
        self.state
            .lock()
            .unwrap()
            .joined
            .get(id)
            .is_some_and(|j| j.closed.is_none())
    }

    // --- Shared --------------------------------------------------------------

    /// Do something in a game, as this daemon's player.
    pub async fn act(self: &Arc<Self>, id: &str, action: Action) -> Result<GameView, String> {
        let reply = {
            let mut state = self.state.lock().unwrap();
            let State {
                hosted,
                joined,
                name,
                ..
            } = &mut *state;
            if let Some(hosted) = hosted.get_mut(id) {
                let color = hosted
                    .game
                    .color_of(true)
                    .ok_or("Nobody has joined this game yet.")?;
                let result = hosted.game.apply(color, action, Instant::now());
                push(hosted, name);
                drop(state);
                self.touch();
                result?;
                return self.view(id).ok_or_else(|| "The game vanished.".into());
            }
            let joined = joined.get_mut(id).ok_or("There's no such game.")?;
            if let Some(reason) = &joined.closed {
                return Err(reason.clone());
            }
            let link = joined
                .link
                .as_ref()
                .ok_or("Not connected to the host right now — reconnecting…")?;
            let seq = self.next_id();
            let (reply, wait) = oneshot::channel();
            joined.pending.insert(seq, reply);
            link.send(ToHost::Act { seq, action })
                .map_err(|_| "The connection to the host just closed.")?;
            wait
        };
        match tokio::time::timeout(ACK_TIMEOUT, reply).await {
            Ok(Ok(None)) => self.view(id).ok_or_else(|| "The game vanished.".into()),
            Ok(Ok(Some(error))) => Err(error),
            Ok(Err(_)) => Err("Lost the connection to the host.".into()),
            Err(_) => Err("The host didn't answer in time.".into()),
        }
    }

    /// Leave a game: resign it if it's still going, then forget it.
    pub async fn remove(self: &Arc<Self>, id: &str) -> Result<(), String> {
        let playing_joined = {
            let mut state = self.state.lock().unwrap();
            let State {
                hosted,
                joined,
                name,
                ..
            } = &mut *state;
            if let Some(mut game) = hosted.remove(id) {
                if game.game.status == Status::Playing
                    && let Some(color) = game.game.color_of(true)
                {
                    let _ = game.game.apply(color, Action::Resign, Instant::now());
                    push(&mut game, name);
                }
                if let Some((_, link)) = game.guest.as_ref().and_then(|g| g.link.as_ref()) {
                    let _ = link.send(ToGuest::Closed {
                        reason: format!("{name} closed this game."),
                    });
                }
                drop(state);
                self.touch();
                return Ok(());
            }
            let joined = joined.get(id).ok_or("There's no such game.")?;
            joined.snapshot.status == "playing" && joined.link.is_some() && joined.closed.is_none()
        };
        if playing_joined {
            // Best effort: if the host is unreachable it will see us leave.
            let _ = self.act(id, Action::Resign).await;
        }
        // Dropping the link's sender ends its task, which says goodbye.
        self.state.lock().unwrap().joined.remove(id);
        self.touch();
        Ok(())
    }

    pub fn view(&self, id: &str) -> Option<GameView> {
        let state = self.state.lock().unwrap();
        self.view_locked(&state, id)
    }

    fn view_locked(&self, state: &State, id: &str) -> Option<GameView> {
        if let Some(hosted) = state.hosted.get(id) {
            let snapshot = snapshot_of(hosted, &state.name);
            let guest = hosted.guest.as_ref();
            return Some(GameView {
                you: hosted.game.color_of(true),
                opponent: guest.map(|g| g.name.clone()),
                connected: guest.is_some_and(|g| g.link.is_some()),
                address: guest.map(|g| g.address.clone()),
                role: "host",
                closed: None,
                rev: hosted.rev,
                snapshot,
            });
        }
        let joined = state.joined.get(id)?;
        let mut snapshot = joined.snapshot.clone();
        // The host's clock reading is as of when it arrived; run it forward.
        if let Some(clock) = &mut snapshot.clock {
            let elapsed = joined.received.elapsed().as_millis() as i64;
            match clock.running {
                Some(Color::White) => clock.white_ms = (clock.white_ms - elapsed).max(0),
                Some(Color::Black) => clock.black_ms = (clock.black_ms - elapsed).max(0),
                None => {}
            }
        }
        Some(GameView {
            you: snapshot.host_color.map(Color::other),
            opponent: Some(snapshot.host_name.clone()),
            connected: joined.link.is_some(),
            address: Some(joined.address.clone()),
            role: "guest",
            closed: joined.closed.clone(),
            rev: joined.rev,
            snapshot,
        })
    }

    pub async fn lobby(self: &Arc<Self>) -> Lobby {
        let discovery_error = self.ensure_discovery().await.err();
        let mut state = self.state.lock().unwrap();
        state.peers.retain(|_, p| p.seen.elapsed() < PEER_TTL);
        let mut peers: Vec<PeerView> = state
            .peers
            .iter()
            .map(|(instance, p)| PeerView {
                instance: instance.clone(),
                name: p.name.clone(),
                address: SocketAddr::new(p.address, p.port).to_string(),
                version: p.version.clone(),
                games: p.games.clone(),
            })
            .collect();
        peers.sort_by(|a, b| a.name.cmp(&b.name).then(a.address.cmp(&b.address)));

        let ids: Vec<String> = state
            .hosted
            .keys()
            .chain(state.joined.keys())
            .cloned()
            .collect();
        let mut games: Vec<GameView> = ids
            .iter()
            .filter_map(|id| self.view_locked(&state, id))
            .collect();
        games.sort_by(|a, b| b.snapshot.created_at.cmp(&a.snapshot.created_at));

        Lobby {
            name: state.name.clone(),
            hostname: self.hostname.clone(),
            version: env!("CARGO_PKG_VERSION"),
            port: self.port(),
            address: self
                .port()
                .and_then(|port| local_ip().map(|ip| SocketAddr::new(ip, port).to_string())),
            discovery_port: self.discovery_port,
            discovery_error,
            peers,
            games,
        }
    }
}

fn snapshot_of(hosted: &Hosted, host_name: &str) -> Snapshot {
    let mut snapshot = hosted.game.snapshot(
        host_name,
        hosted.guest.as_ref().map(|g| g.name.as_str()),
        Instant::now(),
    );
    // The game's own version only moves with the board; the guest wants to
    // know about name changes too.
    snapshot.version = hosted.rev.max(snapshot.version);
    snapshot
}

/// Record a change to a hosted game and send the guest the new state.
fn push(hosted: &mut Hosted, host_name: &str) {
    hosted.rev += 1;
    if let Some((_, link)) = hosted.guest.as_ref().and_then(|g| g.link.as_ref()) {
        let _ = link.send(ToGuest::State {
            snapshot: Box::new(snapshot_of(hosted, host_name)),
        });
    }
}

fn open_games(state: &State) -> Vec<OpenGame> {
    let mut games: Vec<OpenGame> = state
        .hosted
        .values()
        .filter(|h| h.game.status == Status::Waiting)
        .map(|h| OpenGame {
            id: h.game.id.clone(),
            rules: h.game.rules.clone(),
            created_at: h.game.created_at.clone(),
        })
        .collect();
    games.sort_by(|a, b| a.created_at.cmp(&b.created_at));
    games
}

// --- Tasks -------------------------------------------------------------------

async fn hear_beacons(lan: Arc<Lan>, socket: Arc<UdpSocket>) {
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let Ok((len, from)) = socket.recv_from(&mut buf).await else {
            tokio::time::sleep(Duration::from_millis(250)).await;
            continue;
        };
        if let Ok(beacon) = serde_json::from_slice::<Beacon>(&buf[..len]) {
            lan.heard(beacon, from);
        }
    }
}

async fn send_beacons(lan: Arc<Lan>, socket: Arc<UdpSocket>) {
    let targets = broadcast_targets(lan.discovery_port);
    let mut every = tokio::time::interval(BEACON_EVERY);
    // Whether the last beacon advertised anything: one empty beacon follows
    // the last open game away, so peers drop it at once instead of on expiry.
    let mut was_open = false;
    loop {
        every.tick().await;
        let Some(port) = lan.port() else { continue };
        let beacon = lan.beacon(port);
        let open = !beacon.games.is_empty();
        if !open && !was_open {
            continue;
        }
        was_open = open;
        let Ok(bytes) = serde_json::to_vec(&beacon) else {
            continue;
        };
        for target in &targets {
            if let Err(e) = socket.send_to(&bytes, target).await {
                tracing::debug!("chess beacon to {target} failed: {e}");
            }
        }
    }
}

async fn tick_clocks(lan: Arc<Lan>) {
    let mut every = tokio::time::interval(Duration::from_millis(200));
    loop {
        every.tick().await;
        let now = Instant::now();
        let mut state = lan.state.lock().unwrap();
        let State { hosted, name, .. } = &mut *state;
        let mut changed = false;
        for game in hosted.values_mut() {
            if game.game.tick(now) {
                push(game, name);
                changed = true;
            }
        }
        drop(state);
        if changed {
            lan.touch();
        }
    }
}

async fn accept(lan: Arc<Lan>, listener: TcpListener) {
    loop {
        match listener.accept().await {
            Ok((stream, from)) => {
                let Ok(permit) = lan.connections.clone().try_acquire_owned() else {
                    continue;
                };
                let lan = lan.clone();
                tokio::spawn(async move {
                    serve_guest(lan, stream, from).await;
                    drop(permit);
                });
            }
            Err(e) => {
                tracing::warn!("chess listener: {e}");
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
        }
    }
}

/// One connection to the host, from handshake to hang-up.
async fn serve_guest(lan: Arc<Lan>, stream: TcpStream, from: SocketAddr) {
    let _ = stream.set_nodelay(true);
    let (read, mut write) = stream.into_split();
    let mut reader = BufReader::new(read);

    let first =
        match tokio::time::timeout(HANDSHAKE_TIMEOUT, read_line::<_, ToHost>(&mut reader)).await {
            Ok(Ok(Some(message))) => message,
            _ => return,
        };
    let refuse = |reason: &str| ToGuest::Rejected {
        reason: reason.to_string(),
    };
    let wrong_version = "That player is running a different version of ciabatta's chess.";

    let seated = match first {
        ToHost::Hello { .. } => {
            let (name, games) = lan.open_games();
            let _ = write_line(&mut write, &ToGuest::Games { name, games }).await;
            return;
        }
        ToHost::Join { proto, .. } | ToHost::Resume { proto, .. } if proto != PROTOCOL => {
            let _ = write_line(&mut write, &refuse(wrong_version)).await;
            return;
        }
        ToHost::Join { game, name, .. } => lan.seat(&game, &name, from).map(|seat| (game, seat)),
        ToHost::Resume { game, secret, .. } => lan.resume(&game, &secret).map(|seat| (game, seat)),
        _ => return,
    };
    let (
        game,
        Seat {
            secret,
            snapshot,
            conn,
            tx,
            rx: mut outgoing,
        },
    ) = match seated {
        Ok(seated) => seated,
        Err(reason) => {
            let _ = write_line(&mut write, &refuse(&reason)).await;
            return;
        }
    };
    if write_line(
        &mut write,
        &ToGuest::Joined {
            secret,
            snapshot: Box::new(snapshot),
        },
    )
    .await
    .is_err()
    {
        lan.guest_gone(&game, conn);
        return;
    }

    let mut incoming = spawn_reader::<ToHost>(reader);
    let mut ping = tokio::time::interval(PING_EVERY);
    loop {
        tokio::select! {
            message = incoming.recv() => match message {
                Some(ToHost::Act { seq, action }) => {
                    let error = lan.guest_act(&game, conn, action).err();
                    // Through the channel, so it lands after the state push.
                    let _ = tx.send(ToGuest::Ack { seq, error });
                }
                Some(ToHost::Ping) => {}
                Some(ToHost::Bye) | None => break,
                Some(_) => {}
            },
            line = outgoing.recv() => match line {
                Some(line) => {
                    let closing = matches!(line, ToGuest::Closed { .. });
                    if write_line(&mut write, &line).await.is_err() || closing {
                        break;
                    }
                }
                // Our seat was taken over by a newer connection.
                None => break,
            },
            _ = ping.tick() => {
                if write_line(&mut write, &ToGuest::Ping).await.is_err() {
                    break;
                }
            }
        }
    }
    lan.guest_gone(&game, conn);
}

/// A joined game's link to its host, reconnecting until the game is left or
/// the host closes it.
async fn guest_task(
    lan: Arc<Lan>,
    id: String,
    reader: BufReader<OwnedReadHalf>,
    write: OwnedWriteHalf,
    rx: mpsc::UnboundedReceiver<ToHost>,
) {
    let mut link = Some((reader, write, rx));
    loop {
        if let Some((reader, write, rx)) = link.take() {
            match guest_link(&lan, &id, reader, write, rx).await {
                LinkEnd::Left => return,
                LinkEnd::Closed(reason) => {
                    lan.guest_disconnected(&id, Some(reason));
                    return;
                }
                LinkEnd::Dropped => lan.guest_disconnected(&id, None),
            }
        }
        tokio::time::sleep(RECONNECT_EVERY).await;
        if !lan.still_joined(&id) {
            return;
        }
        match lan.reconnect(&id).await {
            Ok(parts) => link = Some(parts),
            Err(LinkError::Transient(_)) => {}
            Err(LinkError::Refused(reason)) => {
                lan.guest_disconnected(&id, Some(reason));
                return;
            }
        }
    }
}

enum LinkEnd {
    /// We left the game.
    Left,
    /// The host closed it.
    Closed(String),
    /// The connection broke; worth reconnecting.
    Dropped,
}

async fn guest_link(
    lan: &Lan,
    id: &str,
    reader: BufReader<OwnedReadHalf>,
    mut write: OwnedWriteHalf,
    mut outgoing: mpsc::UnboundedReceiver<ToHost>,
) -> LinkEnd {
    let mut incoming = spawn_reader::<ToGuest>(reader);
    let mut ping = tokio::time::interval(PING_EVERY);
    loop {
        tokio::select! {
            message = incoming.recv() => match message {
                Some(ToGuest::State { snapshot }) => lan.guest_update(id, *snapshot),
                Some(ToGuest::Ack { seq, error }) => lan.guest_ack(id, seq, error),
                Some(ToGuest::Closed { reason }) => return LinkEnd::Closed(clean_name(&reason)),
                Some(_) => {}
                None => return LinkEnd::Dropped,
            },
            line = outgoing.recv() => match line {
                Some(line) => {
                    if write_line(&mut write, &line).await.is_err() {
                        return LinkEnd::Dropped;
                    }
                }
                None => {
                    let _ = write_line(&mut write, &ToHost::Bye).await;
                    return LinkEnd::Left;
                }
            },
            _ = ping.tick() => {
                if write_line(&mut write, &ToHost::Ping).await.is_err() {
                    return LinkEnd::Dropped;
                }
            }
        }
    }
}

// --- Plumbing ----------------------------------------------------------------

impl LinkError {
    fn message(self) -> String {
        match self {
            LinkError::Transient(m) | LinkError::Refused(m) => m,
        }
    }
}

async fn connect(address: &str) -> Result<(BufReader<OwnedReadHalf>, OwnedWriteHalf), LinkError> {
    let stream = tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect(address))
        .await
        .map_err(|_| LinkError::Transient(format!("{address} didn't answer.")))?
        .map_err(|e| LinkError::Transient(format!("Couldn't reach {address}: {e}")))?;
    let _ = stream.set_nodelay(true);
    let (read, write) = stream.into_split();
    Ok((BufReader::new(read), write))
}

async fn read_reply(reader: &mut BufReader<OwnedReadHalf>) -> Result<ToGuest, LinkError> {
    match tokio::time::timeout(HANDSHAKE_TIMEOUT, read_line::<_, ToGuest>(reader)).await {
        Ok(Ok(Some(reply))) => Ok(reply),
        Ok(Ok(None)) => Err(LinkError::Transient("The host hung up.".into())),
        Ok(Err(e)) => Err(LinkError::Transient(format!(
            "The host sent something unreadable: {e}"
        ))),
        Err(_) => Err(LinkError::Transient(
            "The host didn't answer in time.".into(),
        )),
    }
}

/// Read one capped line and parse it. `None` at end of stream.
async fn read_line<R, T>(reader: &mut R) -> io::Result<Option<T>>
where
    R: AsyncBufRead + Unpin,
    T: DeserializeOwned,
{
    let mut line = String::new();
    let read = (&mut *reader).take(MAX_LINE).read_line(&mut line).await?;
    if read == 0 {
        return Ok(None);
    }
    if !line.ends_with('\n') {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "line too long"));
    }
    serde_json::from_str(&line)
        .map(Some)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

/// Read lines on their own task — `read_line` isn't cancel-safe, so it can't
/// sit in a `select!` — until the stream ends, goes quiet, or misbehaves.
fn spawn_reader<T>(mut reader: BufReader<OwnedReadHalf>) -> mpsc::UnboundedReceiver<T>
where
    T: DeserializeOwned + Send + 'static,
{
    let (tx, rx) = mpsc::unbounded_channel();
    tokio::spawn(async move {
        while let Ok(Ok(Some(message))) =
            tokio::time::timeout(IDLE_TIMEOUT, read_line(&mut reader)).await
        {
            if tx.send(message).is_err() {
                break;
            }
        }
    });
    rx
}

async fn write_line<T: Serialize>(write: &mut OwnedWriteHalf, message: &T) -> io::Result<()> {
    let mut line = serde_json::to_vec(message).map_err(io::Error::other)?;
    line.push(b'\n');
    write.write_all(&line).await
}

fn bind_discovery(port: u16) -> io::Result<std::net::UdpSocket> {
    use socket2::{Domain, Protocol, Socket, Type};
    let socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
    // Several daemons on one machine (one per user, say) all hear beacons.
    socket.set_reuse_address(true)?;
    #[cfg(unix)]
    socket.set_reuse_port(true)?;
    socket.set_broadcast(true)?;
    socket.set_nonblocking(true)?;
    socket.bind(&SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, port).into())?;
    Ok(socket.into())
}

fn broadcast_targets(port: u16) -> Vec<SocketAddr> {
    let configured: Vec<SocketAddr> = std::env::var(BROADCAST_ENV)
        .unwrap_or_default()
        .split(',')
        .filter_map(|entry| {
            let entry = entry.trim();
            entry.parse::<SocketAddr>().ok().or_else(|| {
                entry
                    .parse::<IpAddr>()
                    .ok()
                    .map(|ip| SocketAddr::new(ip, port))
            })
        })
        .collect();
    if configured.is_empty() {
        vec![SocketAddr::new(IpAddr::V4(Ipv4Addr::BROADCAST), port)]
    } else {
        configured
    }
}

/// Accept `host:port`, or a bare `host` on the default chess port.
fn normalize_address(address: &str) -> Result<String, String> {
    let address = address.trim();
    if address.is_empty() {
        return Err("Give the host's address as host:port.".into());
    }
    if address
        .rsplit_once(':')
        .is_some_and(|(_, port)| port.parse::<u16>().is_ok())
    {
        Ok(address.to_string())
    } else {
        Err(format!(
            "'{address}' needs a port, like {address}:{DEFAULT_PORT} — the host's chess page shows it."
        ))
    }
}

/// The address this machine reaches the network from. Connecting a UDP
/// socket sends nothing; it only asks the OS which interface it would use.
fn local_ip() -> Option<IpAddr> {
    let socket = std::net::UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).ok()?;
    socket.connect((Ipv4Addr::new(192, 0, 2, 1), 9)).ok()?;
    Some(socket.local_addr().ok()?.ip()).filter(|ip| !ip.is_unspecified())
}

fn env_port(var: &str) -> Option<u16> {
    std::env::var(var).ok()?.trim().parse().ok()
}

/// Trim a name to something safe to show: no control characters, bounded.
pub fn clean_name(name: &str) -> String {
    name.chars()
        .filter(|c| !c.is_control())
        .collect::<String>()
        .trim()
        .chars()
        .take(MAX_NAME)
        .collect::<String>()
        .trim()
        .to_string()
}

/// This machine's name, the default a player is shown as.
pub fn hostname() -> String {
    #[cfg(unix)]
    {
        let mut buf = [0u8; 256];
        // SAFETY: the buffer is valid for its whole length, and gethostname
        // writes at most that many bytes.
        let ok = unsafe { libc::gethostname(buf.as_mut_ptr().cast(), buf.len()) } == 0;
        if ok {
            let end = buf.iter().position(|b| *b == 0).unwrap_or(buf.len());
            let name = clean_name(&String::from_utf8_lossy(&buf[..end]));
            if !name.is_empty() {
                return name;
            }
        }
    }
    std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .map(|n| clean_name(&n))
        .ok()
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| "ciabatta".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_cleaned_and_capped() {
        assert_eq!(clean_name("  bob\u{7}\n "), "bob");
        assert_eq!(clean_name(&"x".repeat(100)).len(), MAX_NAME);
    }

    #[test]
    fn addresses_need_a_port() {
        assert!(normalize_address("10.0.0.5:4000").is_ok());
        assert!(normalize_address("10.0.0.5").is_err());
        assert!(normalize_address("").is_err());
    }

    /// Two daemons in one process: a host opens a game, a guest joins over
    /// real TCP, a third party is turned away, and moves flow both ways.
    #[tokio::test]
    async fn two_daemons_play_and_a_third_is_refused() {
        let host = Lan::new();
        let guest = Lan::new();
        let intruder = Lan::new();

        let rules = Rules {
            time: None,
            host_color: super::super::game::ColorChoice::White,
            ..Rules::default()
        };
        let port = host.ensure_listener().await.unwrap();
        let id = super::super::new_id();
        host.state.lock().unwrap().hosted.insert(
            id.clone(),
            Hosted {
                game: Game::new(id.clone(), rules),
                guest: None,
                rev: 1,
            },
        );
        let address = format!("127.0.0.1:{port}");

        let (_, games) = guest.browse(&address).await.unwrap();
        assert_eq!(games.len(), 1);

        let view = guest.join(&address, &id).await.unwrap();
        assert_eq!(view.you, Some(Color::Black));
        assert_eq!(view.snapshot.status, "playing");

        // The game is closed to everyone else now.
        let refused = intruder.join(&address, &id).await.unwrap_err();
        assert!(refused.contains("two players"), "{refused}");
        assert!(guest.browse(&address).await.unwrap().1.is_empty());

        // The guest can't move out of turn; the host moves, then the guest.
        assert!(
            guest
                .act(
                    &id,
                    Action::Move {
                        from: "e7".into(),
                        to: "e5".into(),
                        promotion: None
                    }
                )
                .await
                .is_err()
        );
        host.act(
            &id,
            Action::Move {
                from: "e2".into(),
                to: "e4".into(),
                promotion: None,
            },
        )
        .await
        .unwrap();
        let after = guest
            .act(
                &id,
                Action::Move {
                    from: "e7".into(),
                    to: "e5".into(),
                    promotion: None,
                },
            )
            .await
            .unwrap();
        assert_eq!(after.snapshot.moves, ["e4", "e5"]);
        assert_eq!(host.view(&id).unwrap().snapshot.moves, ["e4", "e5"]);

        // A wrong secret can't take the seat over; the right one can, on a
        // fresh connection, and play carries on through it.
        assert!(host.resume(&id, "not-the-secret").is_err());
        let (reader, write, rx) = guest.reconnect(&id).await.ok().expect("resume succeeds");
        tokio::spawn(guest_task(guest.clone(), id.clone(), reader, write, rx));
        host.act(
            &id,
            Action::Move {
                from: "g1".into(),
                to: "f3".into(),
                promotion: None,
            },
        )
        .await
        .unwrap();
        let after = guest
            .act(
                &id,
                Action::Move {
                    from: "b8".into(),
                    to: "c6".into(),
                    promotion: None,
                },
            )
            .await
            .unwrap();
        assert_eq!(after.snapshot.moves, ["e4", "e5", "Nf3", "Nc6"]);
        assert!(host.view(&id).unwrap().connected);

        // Leaving resigns.
        guest.remove(&id).await.unwrap();
        let view = host.view(&id).unwrap();
        assert_eq!(view.snapshot.winner, Some(Color::White));
    }
}
