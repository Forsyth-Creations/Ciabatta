//! Keeping a remote cache's primary and its backups holding the same entries:
//! `ciabatta cache sync`.
//!
//! Runs keep the servers close on their own. Every build is uploaded to each
//! server that answers, and an entry restored from one server is copied up to
//! the servers that missed it. What that can't cover is a server that was down
//! while others were written to: when it comes back it is missing everything
//! built in the meantime. This catches it up. Each server lists the keys it
//! holds, and every key missing from a writable server is copied there from
//! one that has it: downloaded once, uploaded to each server that lacks it.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use super::client::{Client, ListedEntry, normalize};
use crate::config::CiabattaConfig;

/// One server as the sync found it.
struct Server {
    url: String,
    read_only: bool,
    client: Client,
    project: String,
    keys: BTreeMap<String, ListedEntry>,
}

/// What a sync found and did.
#[derive(Default)]
pub struct Report {
    /// `(url, why)` for each configured server that couldn't be used.
    pub skipped: Vec<(String, String)>,
    /// How many entries each reachable server holds, before the sync.
    pub held: Vec<(String, usize)>,
    /// `(url, entries, bytes)` copied to each server.
    pub copied: Vec<(String, usize, u64)>,
    /// `(url, key, why)` for each copy that failed.
    pub failed: Vec<(String, String, String)>,
}

/// The copies needed: for each key some server lacks, the server to copy it
/// from (the first, in priority order, that has it) and the writable servers
/// it is missing on.
pub fn plan(servers: &[(BTreeSet<String>, bool)]) -> Vec<(String, usize, Vec<usize>)> {
    let every: BTreeSet<&String> = servers.iter().flat_map(|(keys, _)| keys).collect();
    every
        .into_iter()
        .filter_map(|key| {
            let from = servers.iter().position(|(keys, _)| keys.contains(key))?;
            let to: Vec<usize> = servers
                .iter()
                .enumerate()
                .filter(|(_, (keys, writable))| *writable && !keys.contains(key))
                .map(|(i, _)| i)
                .collect();
            (!to.is_empty()).then(|| (key.clone(), from, to))
        })
        .collect()
}

/// Bring every reachable server of `config`'s remote cache up to the union of
/// what they hold. `dry_run` works out and reports the copies without making
/// them.
pub async fn sync(config: &CiabattaConfig, root: &Path, dry_run: bool) -> Result<Report> {
    let Some(remote) = config.cache.as_ref().and_then(|c| c.remote()).cloned() else {
        bail!(
            "This project has no remote cache. Set `cache.remote.url` (and \
             `cache.remote.backups`) in the root ciabatta.yaml."
        );
    };
    let configured = remote.servers();
    if configured.len() < 2 {
        bail!(
            "There is nothing to sync with only one remote cache server. List more under \
             `cache.remote.backups` in the root ciabatta.yaml."
        );
    }

    let name = remote
        .name
        .clone()
        .or_else(|| config.workspace.as_ref().and_then(|w| w.name.clone()))
        .or_else(|| root.file_name().map(|n| n.to_string_lossy().to_string()))
        .unwrap_or_else(|| "project".to_string());

    let mut report = Report::default();
    let probes: Vec<(String, bool)> = configured
        .iter()
        .map(|s| (s.url.clone(), s.tls_verify))
        .collect();
    let answers = super::reach::probe_all(&probes).await;

    let mut project = remote.project.clone();
    let mut servers = Vec::new();
    for (server, answer) in configured.into_iter().zip(answers) {
        let url = normalize(&server.url);
        let found = async {
            answer.map_err(anyhow::Error::msg)?;
            let client = Client::new(&server.url, server.tls_verify)?;
            let registered = client.register(&name, project.as_deref()).await?;
            let keys = client
                .list_entries(&registered.id)
                .await
                .context("couldn't list its entries (a server older than this ciabatta can't)")?;
            anyhow::Ok((client, registered.id, keys))
        }
        .await;
        match found {
            Ok((client, id, keys)) => {
                project.get_or_insert_with(|| id.clone());
                report.held.push((url.clone(), keys.len()));
                servers.push(Server {
                    url,
                    read_only: server.read_only,
                    client,
                    project: id,
                    keys: keys.into_iter().map(|e| (e.key.clone(), e)).collect(),
                });
            }
            Err(e) => report.skipped.push((url, format!("{e:#}"))),
        }
    }
    if servers.len() < 2 {
        bail!(
            "Only {} of the remote cache servers could be reached, so there is nothing to sync{}",
            servers.len(),
            report
                .skipped
                .iter()
                .map(|(url, why)| format!("\n  {url}: {why}"))
                .collect::<String>()
        );
    }

    let shape: Vec<(BTreeSet<String>, bool)> = servers
        .iter()
        .map(|s| (s.keys.keys().cloned().collect(), !s.read_only))
        .collect();
    let copies = plan(&shape);
    let mut totals: Vec<(usize, u64)> = vec![(0, 0); servers.len()];

    let scratch = scratch_dir();
    for (n, (key, from, to)) in copies.iter().enumerate() {
        let size = servers[*from].keys.get(key).map(|e| e.size).unwrap_or(0);
        if dry_run {
            for &i in to {
                totals[i].0 += 1;
                totals[i].1 += size;
            }
            continue;
        }
        let source = &servers[*from];
        let dir = scratch.join(n.to_string());
        let fetched = async {
            let entry = source
                .client
                .fetch_for_sync(&source.project, key)
                .await?
                .context("it disappeared from the server that listed it")?;
            source
                .client
                .download(&source.project, key, &entry, &dir)
                .await?;
            anyhow::Ok(entry)
        }
        .await;
        let entry = match fetched {
            Ok(entry) => entry,
            Err(e) => {
                report
                    .failed
                    .push((source.url.clone(), key.clone(), format!("{e:#}")));
                let _ = std::fs::remove_dir_all(&dir);
                continue;
            }
        };
        for &i in to {
            let target = &servers[i];
            match target
                .client
                .upload(&target.project, key, &entry, &dir)
                .await
            {
                Ok(()) => {
                    totals[i].0 += 1;
                    totals[i].1 += entry.size;
                }
                Err(e) => report
                    .failed
                    .push((target.url.clone(), key.clone(), format!("{e:#}"))),
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
    let _ = std::fs::remove_dir_all(&scratch);

    report.copied = servers
        .iter()
        .zip(totals)
        .map(|(s, (count, bytes))| (s.url.clone(), count, bytes))
        .collect();
    Ok(report)
}

fn scratch_dir() -> PathBuf {
    std::env::temp_dir().join(format!(
        "ciabatta-cache-sync-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4().simple()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(list: &[&str]) -> BTreeSet<String> {
        list.iter().map(|k| k.to_string()).collect()
    }

    #[test]
    fn each_missing_key_is_copied_from_the_first_server_that_has_it() {
        let servers = vec![
            (keys(&["a", "b"]), true),
            (keys(&["b", "c"]), true),
            (keys(&["c"]), true),
        ];
        assert_eq!(
            plan(&servers),
            vec![
                ("a".to_string(), 0, vec![1, 2]),
                ("b".to_string(), 0, vec![2]),
                ("c".to_string(), 1, vec![0]),
            ]
        );
    }

    #[test]
    fn a_read_only_server_is_read_from_but_never_written_to() {
        let servers = vec![(keys(&["a"]), true), (keys(&["b"]), false)];
        assert_eq!(plan(&servers), vec![("b".to_string(), 1, vec![0])]);
    }

    #[test]
    fn servers_already_in_step_need_nothing() {
        let servers = vec![(keys(&["a", "b"]), true), (keys(&["a", "b"]), true)];
        assert!(plan(&servers).is_empty());
    }
}
