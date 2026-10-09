//! Serving the remote cache over HTTPS.
//!
//! The cache moves build artifacts and session tokens, and on anything but a
//! trusted network both deserve encryption. It used to say "put it behind a
//! reverse proxy" and leave it there — which is right for a company that runs
//! one, and a second service to stand up for everybody else. So the server
//! terminates TLS itself when given a certificate:
//!
//! ```yaml
//! server:
//!   tls:
//!     cert: tls/cert.pem
//!     key:  tls/key.pem
//! ```
//!
//! and `ciabatta remote-cache init --tls` writes a self-signed pair to point
//! that at, so HTTPS works on a network with no CA to hand. Clients then trust
//! it with `ciabatta remote-cache login <URL> --ca-cert tls/cert.pem`, which
//! pins that one certificate rather than switching verification off.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use axum::Router;
use axum::extract::ConnectInfo;
use serde::{Deserialize, Serialize};

/// Where the server's certificate and key are.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct TlsConfig {
    /// PEM certificate chain, leaf first. Relative to the config file.
    pub cert: PathBuf,
    /// PEM private key (PKCS#8, PKCS#1, or SEC1). Relative to the config file.
    pub key: PathBuf,
}

impl TlsConfig {
    /// Resolve relative paths against the config file's directory, so the
    /// server can be started from anywhere.
    pub fn resolve_relative(&mut self, base: &Path) {
        if self.cert.is_relative() {
            self.cert = base.join(&self.cert);
        }
        if self.key.is_relative() {
            self.key = base.join(&self.key);
        }
    }

    /// Read the pair and build what accepts TLS connections with it.
    ///
    /// Done at startup, so a missing file or a key that doesn't match the
    /// certificate stops the server with a message rather than failing every
    /// handshake afterwards.
    pub fn acceptor(&self) -> Result<tokio_rustls::TlsAcceptor> {
        use rustls::pki_types::pem::PemObject;
        use rustls::pki_types::{CertificateDer, PrivateKeyDer};

        let certs: Vec<CertificateDer<'static>> = CertificateDer::pem_file_iter(&self.cert)
            .with_context(|| format!("Failed to read the certificate {}", self.cert.display()))?
            .collect::<Result<_, _>>()
            .with_context(|| format!("{} is not a PEM certificate", self.cert.display()))?;
        anyhow::ensure!(
            !certs.is_empty(),
            "{} holds no certificate",
            self.cert.display()
        );
        let key = PrivateKeyDer::from_pem_file(&self.key)
            .with_context(|| format!("Failed to read the private key {}", self.key.display()))?;

        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let mut config = rustls::ServerConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .context("No TLS protocol versions are available")?
            .with_no_client_auth()
            .with_single_cert(certs, key)
            .with_context(|| {
                format!(
                    "{} and {} don't make a usable pair — is the key the certificate's own?",
                    self.cert.display(),
                    self.key.display()
                )
            })?;
        // HTTP/2 where the client offers it, as reqwest does.
        config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
        Ok(tokio_rustls::TlsAcceptor::from(Arc::new(config)))
    }

    /// The SHA-256 fingerprint of the leaf certificate, for printing at
    /// startup — what somebody compares before trusting a self-signed cert.
    pub fn fingerprint(&self) -> Option<String> {
        use rustls::pki_types::CertificateDer;
        use rustls::pki_types::pem::PemObject;
        let first = CertificateDer::pem_file_iter(&self.cert)
            .ok()?
            .next()?
            .ok()?;
        Some(colon_hex(&crate::cache::hash_bytes(first.as_ref())))
    }
}

/// `ab12cd…` → `AB:12:CD:…`, the way certificate fingerprints are shown.
fn colon_hex(hex: &str) -> String {
    hex.as_bytes()
        .chunks(2)
        .map(|pair| String::from_utf8_lossy(pair).to_uppercase())
        .collect::<Vec<_>>()
        .join(":")
}

/// Write a self-signed certificate and key into `dir`, valid for `hosts`.
///
/// `hosts` should include every name clients will use — a certificate is only
/// accepted for the names it lists. `localhost` and the loopback addresses are
/// always added, so the server can be checked from its own machine.
pub fn generate_self_signed(dir: &Path, hosts: &[String]) -> Result<TlsConfig> {
    let mut names: Vec<String> = vec![
        "localhost".to_string(),
        "127.0.0.1".to_string(),
        "::1".to_string(),
    ];
    for host in hosts {
        let host = host.trim();
        if !host.is_empty() && !names.iter().any(|n| n == host) {
            names.push(host.to_string());
        }
    }

    let generated = rcgen::generate_simple_self_signed(names)
        .context("Failed to generate a self-signed certificate")?;

    std::fs::create_dir_all(dir).with_context(|| format!("Failed to create {}", dir.display()))?;
    let cert = dir.join("cert.pem");
    let key = dir.join("key.pem");
    std::fs::write(&cert, generated.cert.pem())
        .with_context(|| format!("Failed to write {}", cert.display()))?;
    std::fs::write(&key, generated.key_pair.serialize_pem())
        .with_context(|| format!("Failed to write {}", key.display()))?;
    restrict(&key);

    Ok(TlsConfig { cert, key })
}

/// Keep a private key readable by its owner only.
fn restrict(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    #[cfg(not(unix))]
    let _ = path;
}

/// Serve `app` over TLS until `shutdown` resolves.
///
/// The same router as the plain-HTTP path, with the peer address attached the
/// same way, so the request log and every handler can't tell the difference.
/// A handshake that fails — a client speaking plain HTTP to the HTTPS port,
/// usually — is logged and dropped without disturbing anything else.
pub async fn serve(
    listener: tokio::net::TcpListener,
    app: Router,
    acceptor: tokio_rustls::TlsAcceptor,
    shutdown: impl std::future::Future<Output = ()>,
) -> Result<()> {
    use hyper_util::rt::{TokioExecutor, TokioIo};
    use tower::ServiceExt;

    tokio::pin!(shutdown);
    loop {
        let (stream, peer) = tokio::select! {
            accepted = listener.accept() => match accepted {
                Ok(pair) => pair,
                Err(e) => {
                    tracing::warn!("couldn't accept a connection: {e}");
                    continue;
                }
            },
            () = &mut shutdown => return Ok(()),
        };

        let acceptor = acceptor.clone();
        let app = app.clone();
        tokio::spawn(async move {
            let tls = match acceptor.accept(stream).await {
                Ok(tls) => tls,
                Err(e) => {
                    tracing::debug!(%peer, "TLS handshake failed: {e}");
                    return;
                }
            };
            let service = hyper::service::service_fn(
                move |mut request: hyper::Request<hyper::body::Incoming>| {
                    request
                        .extensions_mut()
                        .insert(ConnectInfo::<SocketAddr>(peer));
                    app.clone().oneshot(request)
                },
            );
            if let Err(e) = hyper_util::server::conn::auto::Builder::new(TokioExecutor::new())
                .serve_connection_with_upgrades(TokioIo::new(tls), service)
                .await
            {
                tracing::debug!(%peer, "connection ended with an error: {e}");
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_generated_pair_loads_and_has_a_fingerprint() {
        let dir = std::env::temp_dir().join(format!("ciab_tls_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);

        let config = generate_self_signed(&dir, &["cache.internal".to_string()]).unwrap();
        config
            .acceptor()
            .expect("a generated pair must be usable as-is");
        let fingerprint = config.fingerprint().unwrap();
        assert_eq!(fingerprint.split(':').count(), 32, "{fingerprint}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_missing_certificate_is_reported_by_name() {
        let config = TlsConfig {
            cert: PathBuf::from("/nonexistent/cert.pem"),
            key: PathBuf::from("/nonexistent/key.pem"),
        };
        let Err(error) = config.acceptor() else {
            panic!("a missing certificate must be refused");
        };
        let message = format!("{error:#}");
        assert!(message.contains("/nonexistent/cert.pem"), "{message}");
    }
}
