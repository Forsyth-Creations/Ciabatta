//! Whether a remote cache server can be reached, asked once and remembered.
//!
//! A run talks to its remote cache from several places: the version check, the
//! cache session's registration, every step's lookup and upload, and the build
//! and workflow reports at the end. Each used to find out on its own that the
//! server was down, by waiting out its own ten-second timeout — so a configured
//! cache that wasn't there cost the run half a minute and buried the prompts
//! and output in between.
//!
//! Now the first caller probes the server's health endpoint, at most
//! [`ATTEMPTS`] times and for no more than [`BUDGET`] in all, and the answer is
//! kept for [`MEMORY`]. Everything after asks here first, and a server known to
//! be down is skipped without a request. A server that drops out mid-run is
//! marked down by whichever request found out ([`mark_down`]), so later steps
//! skip it too.
//!
//! The client that got the answer is kept with it, and [`connect`] hands it
//! on, so the requests after a probe reuse its connection rather than looking
//! the server's name up again. A resolver that answers some lookups and fails
//! others would otherwise let the probe through and fail the registration a
//! moment later — "reachable" and "isn't reachable" in the same run.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use super::client::{Client, Health, normalize};

/// The longest a probe may delay a run, all attempts together.
pub const BUDGET: Duration = Duration::from_secs(2);

/// How many times a probe asks before calling the server unreachable.
pub const ATTEMPTS: u32 = 2;

/// How long an answer is trusted. Long enough to cover a run's start and its
/// end-of-run reports; short enough that a long-lived daemon notices a server
/// that has come back.
const MEMORY: Duration = Duration::from_secs(5 * 60);

#[derive(Clone)]
enum Known {
    Up(Box<(Health, Client)>),
    Down(String),
}

static KNOWN: Mutex<Option<HashMap<String, (Instant, Known)>>> = Mutex::new(None);

fn remembered(url: &str) -> Option<Known> {
    let guard = KNOWN.lock().ok()?;
    let (at, known) = guard.as_ref()?.get(&normalize(url))?;
    (at.elapsed() < MEMORY).then(|| known.clone())
}

fn remember(url: &str, known: Known) {
    if let Ok(mut guard) = KNOWN.lock() {
        guard
            .get_or_insert_with(HashMap::new)
            .insert(normalize(url), (Instant::now(), known));
    }
}

/// The server's health, or why it couldn't be had — from memory when it was
/// asked recently, otherwise from at most [`ATTEMPTS`] requests inside
/// [`BUDGET`].
pub async fn probe(url: &str, tls_verify: bool) -> Result<Health, String> {
    reached(url, tls_verify).await.map(|(health, _)| health)
}

/// A client for a server that answers, carrying the session saved for it now
/// and the connection the probe opened — or why the server couldn't be had.
pub async fn connect(url: &str, tls_verify: bool) -> Result<Client, String> {
    let (_, client) = reached(url, tls_verify).await?;
    client.renewed(tls_verify).map_err(|e| format!("{e:#}"))
}

/// [`connect`] to several servers at once. Results come back in the order
/// given.
pub async fn connect_all(servers: &[(String, bool)]) -> Vec<Result<Client, String>> {
    futures::future::join_all(servers.iter().map(|(url, tls)| connect(url, *tls))).await
}

async fn reached(url: &str, tls_verify: bool) -> Result<(Health, Client), String> {
    match remembered(url) {
        Some(Known::Up(up)) => return Ok(*up),
        Some(Known::Down(why)) => return Err(why),
        None => {}
    }
    let result = ask(url, tls_verify).await;
    remember(
        url,
        match &result {
            Ok(up) => Known::Up(Box::new(up.clone())),
            Err(why) => Known::Down(why.clone()),
        },
    );
    result
}

async fn ask(url: &str, tls_verify: bool) -> Result<(Health, Client), String> {
    let client = Client::new(url, tls_verify).map_err(|e| format!("{e:#}"))?;
    let deadline = Instant::now() + BUDGET;
    let mut why = String::from("no answer");
    for attempt in 0..ATTEMPTS {
        // Each attempt gets an even share of what's left, so a first attempt
        // that hangs can't leave the second no time at all.
        let left = deadline.saturating_duration_since(Instant::now());
        let share = left / (ATTEMPTS - attempt);
        if share.is_zero() {
            break;
        }
        match tokio::time::timeout(share, client.health()).await {
            Ok(Ok(health)) => return Ok((health, client)),
            Ok(Err(e)) => why = format!("{e:#}"),
            Err(_) => why = format!("no answer within {}ms", share.as_millis()),
        }
        // A refusal comes back at once; give a server that is restarting a
        // moment before asking again, without spending the budget on it.
        if attempt + 1 < ATTEMPTS {
            let pause = Duration::from_millis(100)
                .min(deadline.saturating_duration_since(Instant::now()) / 2);
            tokio::time::sleep(pause).await;
        }
    }
    Err(format!("no answer after {ATTEMPTS} tries: {why}"))
}

/// Probe several servers at once: the delay is the slowest one's, never the
/// sum. Results come back in the order given.
pub async fn probe_all(servers: &[(String, bool)]) -> Vec<Result<Health, String>> {
    futures::future::join_all(servers.iter().map(|(url, tls)| probe(url, *tls))).await
}

/// A client for each of `remote`'s servers that answers, primary first — the
/// ones worth sending a run's reports to. Unreachable ones are left out
/// silently: the cache session has already said so.
pub async fn live(remote: &crate::cache::RemoteRef) -> Vec<(crate::cache::RemoteServer, Client)> {
    let servers = remote.servers();
    let pairs: Vec<(String, bool)> = servers
        .iter()
        .map(|s| (s.url.clone(), s.tls_verify))
        .collect();
    let answers = connect_all(&pairs).await;
    servers
        .into_iter()
        .zip(answers)
        .filter_map(|(server, answer)| Some((server, answer.ok()?)))
        .collect()
}

/// Whether `url` is known to be down right now. A server never asked about
/// isn't.
pub fn is_down(url: &str) -> bool {
    matches!(remembered(url), Some(Known::Down(_)))
}

/// Record that a request to `url` couldn't reach it, so the rest of the run
/// stops asking.
pub fn mark_down(url: &str, why: String) {
    remember(url, Known::Down(why));
}

/// Whether `error` says the server couldn't be reached at all, as opposed to
/// answering with a refusal — only the first means it is down.
pub fn unreachable(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        cause
            .downcast_ref::<reqwest::Error>()
            .is_some_and(|e| e.is_connect() || e.is_timeout())
    })
}

#[cfg(test)]
pub fn forget(url: &str) {
    if let Ok(mut guard) = KNOWN.lock()
        && let Some(map) = guard.as_mut()
    {
        map.remove(&normalize(url));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_server_that_never_answers_costs_at_most_the_budget() {
        // Accepts the connection and then says nothing, the worst case: the
        // client can't tell it from a slow server until it gives up.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let accepted = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = accepted.clone();
        std::thread::spawn(move || {
            let mut held = Vec::new();
            for stream in listener.incoming() {
                counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                held.push(stream);
            }
        });

        let started = Instant::now();
        let result = probe(&url, true).await;
        let took = started.elapsed();
        assert!(result.is_err());
        assert!(took < BUDGET + Duration::from_millis(500), "took {took:?}");
        assert!(
            accepted.load(std::sync::atomic::Ordering::SeqCst) <= ATTEMPTS as usize,
            "asked more than {ATTEMPTS} times"
        );

        // And it is remembered: asking again costs nothing.
        let again = Instant::now();
        assert!(probe(&url, true).await.is_err());
        assert!(again.elapsed() < Duration::from_millis(50));
        assert!(is_down(&url));
        forget(&url);
    }

    #[tokio::test]
    async fn a_connected_client_reuses_the_probes_connection() {
        // Answers health checks over keep-alive and counts the connections it
        // is opened. One connection for the probe and the request after it
        // means the second never needed the name looked up again.
        use std::io::{BufRead, BufReader, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let opened = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = opened.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                std::thread::spawn(move || {
                    let mut stream = stream.unwrap();
                    let mut reader = BufReader::new(stream.try_clone().unwrap());
                    loop {
                        let mut line = String::new();
                        // Skip to the end of the request's headers.
                        loop {
                            line.clear();
                            if reader.read_line(&mut line).unwrap_or(0) == 0 {
                                return;
                            }
                            if line == "\r\n" {
                                break;
                            }
                        }
                        let body = r#"{"ok":true,"version":"0.0.1"}"#;
                        let _ = write!(
                            stream,
                            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\n\
                             content-length: {}\r\n\r\n{body}",
                            body.len()
                        );
                    }
                });
            }
        });

        assert_eq!(probe(&url, true).await.unwrap().version, "0.0.1");
        let client = connect(&url, true).await.unwrap();
        assert_eq!(client.health().await.unwrap().version, "0.0.1");
        assert_eq!(opened.load(std::sync::atomic::Ordering::SeqCst), 1);
        forget(&url);
    }

    #[tokio::test]
    async fn a_refused_connection_is_tried_twice_and_reported() {
        // Bind and drop, so nothing listens on the port.
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let url = format!("http://127.0.0.1:{port}");
        let why = probe(&url, true).await.unwrap_err();
        assert!(why.contains("after 2 tries"), "{why}");
        forget(&url);
    }
}
