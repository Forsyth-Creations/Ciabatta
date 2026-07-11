//! Read-only browsing of an HTTP registry's contents.
//!
//! Nexus raw repositories, Artifactory, and most static file servers expose an
//! HTML directory index at each folder URL. We fetch that page and scrape the
//! `<a href>` links to present a navigable listing of the repository's folders
//! and artifacts — handy when configuring recipes and you need to know which
//! paths already exist.

use anyhow::{Context, Result};

/// A single entry in a repository listing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    pub is_dir: bool,
}

/// List the entries at `url` by fetching and parsing its HTML directory index.
///
/// `creds` supplies HTTP basic-auth credentials when the repository requires
/// authentication; `tls_verify` mirrors the registry's TLS setting.
pub async fn list_http(
    url: &str,
    tls_verify: bool,
    creds: Option<(String, String)>,
) -> Result<Vec<Entry>> {
    tracing::debug!(%url, tls_verify, "browsing registry path");
    let client = reqwest::Client::builder()
        .danger_accept_invalid_certs(!tls_verify)
        .build()?;

    let mut req = client.get(url);
    if let Some((user, pass)) = creds {
        req = req.basic_auth(user, Some(pass));
    }

    let resp = req
        .send()
        .await
        .with_context(|| format!("HTTP GET {url} failed"))?;
    let status = resp.status();
    if !status.is_success() {
        anyhow::bail!("listing {} failed: HTTP {}", url, status);
    }

    let body = resp.text().await?;
    Ok(parse_html_links(&body))
}

/// Extract folder/file entries from an HTML directory index. Only relative
/// links are kept; parent (`../`) and absolute/scheme links are ignored. A
/// trailing slash marks a folder. Folders sort before files, then by name.
fn parse_html_links(html: &str) -> Vec<Entry> {
    let re = regex::Regex::new(r#"(?is)<a\s+[^>]*href\s*=\s*["']([^"'#?]+)["']"#).unwrap();
    let mut entries: Vec<Entry> = Vec::new();
    let mut seen = std::collections::HashSet::new();

    for cap in re.captures_iter(html) {
        let href = cap[1].trim();
        if href.is_empty()
            || href == "/"
            || href == "./"
            || href == "../"
            || href.starts_with("http://")
            || href.starts_with("https://")
            || href.starts_with("//")
            || href.starts_with("mailto:")
        {
            continue;
        }

        let is_dir = href.ends_with('/');
        // Nexus lists relative hrefs; keep only the final path segment as the
        // display name (some servers emit full paths).
        let trimmed = href.trim_end_matches('/');
        let name = trimmed.rsplit('/').next().unwrap_or(trimmed);
        if name.is_empty() || name.starts_with("..") {
            continue;
        }

        if seen.insert(name.to_string()) {
            entries.push(Entry {
                name: name.to_string(),
                is_dir,
            });
        }
    }

    entries.sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then_with(|| a.name.cmp(&b.name)));
    entries
}

/// Build the listing URL for a `path` relative to a registry's base `url`.
/// Nexus needs a trailing slash to return a directory index.
pub fn listing_url(base_url: &str, path: &str) -> String {
    let base = base_url.trim_end_matches('/');
    let path = path.trim_matches('/');
    if path.is_empty() {
        format!("{base}/")
    } else {
        format!("{base}/{path}/")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_nexus_style_index() {
        let html = r#"
            <html><body>
            <a href="../">Parent Directory</a>
            <a href="releases/">releases/</a>
            <a href="snapshots/">snapshots/</a>
            <a href="app-1.0.0.jar">app-1.0.0.jar</a>
            <a href="https://external.example.com/x">external</a>
            </body></html>
        "#;
        let entries = parse_html_links(html);
        assert_eq!(
            entries,
            vec![
                Entry {
                    name: "releases".into(),
                    is_dir: true
                },
                Entry {
                    name: "snapshots".into(),
                    is_dir: true
                },
                Entry {
                    name: "app-1.0.0.jar".into(),
                    is_dir: false
                },
            ]
        );
    }

    #[test]
    fn listing_url_appends_trailing_slash() {
        assert_eq!(
            listing_url("https://nexus.example.com/repository/raw/", ""),
            "https://nexus.example.com/repository/raw/"
        );
        assert_eq!(
            listing_url("https://nexus.example.com/repository/raw", "team/app"),
            "https://nexus.example.com/repository/raw/team/app/"
        );
    }
}
