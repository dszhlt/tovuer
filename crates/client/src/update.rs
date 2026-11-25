//! Updates from the project's GitHub releases. Each release carries a build
//! for every system (`tovuer-linux-amd64`, `tovuer-windows-amd64.exe`) and
//! GitHub tells each file's SHA-256: `/update` takes the newest release,
//! checks the file against it and puts it in place of this program.

use std::sync::Arc;
use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};

use crate::transfer::{Events, TaskEvent, asset_name, looks_like_executable, replace_self};

/// Where releases are published.
pub const REPO: &str = "dszhlt/tovuer";
/// The biggest build taken.
const MAX_BUILD: u64 = 300 << 20;

/// The newest release.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    pub version: String,
    /// The build for this system, if the release has one.
    pub build: Option<Build>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Build {
    pub url: String,
    pub size: u64,
    pub sha256: Option<[u8; 32]>,
}

fn unhex32(s: &str) -> Option<[u8; 32]> {
    if s.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(s.get(i * 2..i * 2 + 2)?, 16).ok()?;
    }
    Some(out)
}

/// GitHub's answer about the latest release, and the file named `asset` in it.
pub fn parse_release(json: &str, asset: &str) -> Result<Release, String> {
    let v: serde_json::Value = serde_json::from_str(json).map_err(|_| "GitHub answered something unexpected".to_string())?;
    let tag = v["tag_name"].as_str().ok_or("the release has no version")?;
    let version = tag.trim_start_matches('v').to_string();
    let plain = version.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '+'));
    if version.is_empty() || version.len() > 40 || !plain {
        return Err("the release has a strange version".into());
    }
    // only a file on the project's own release page
    let home = format!("https://github.com/{REPO}/releases/download/");
    let build = v["assets"].as_array().and_then(|files| files.iter().find(|a| a["name"].as_str() == Some(asset))).and_then(|a| {
        let url = a["browser_download_url"].as_str().filter(|u| u.starts_with(&home))?;
        let sha256 = a["digest"].as_str().and_then(|d| d.strip_prefix("sha256:")).and_then(unhex32);
        Some(Build { url: url.to_string(), size: a["size"].as_u64()?, sha256 })
    });
    Ok(Release { version, build })
}

/// Dotted numbers: `2.1.0` → [2, 1, 0] (a `-…`/`+…` tail aside).
fn dotted(version: &str) -> Option<Vec<u64>> {
    let core = version.split(['-', '+']).next()?;
    let parts: Option<Vec<u64>> = core.split('.').map(|p| p.parse().ok()).collect();
    parts.filter(|p| p.len() >= 2)
}

/// Whether `latest` is worth updating to from `current`: by number when
/// both are numbered versions (never back), otherwise when they differ (a
/// build made by hand, with a date for a version).
pub fn newer(latest: &str, current: &str) -> bool {
    match (dotted(latest), dotted(current)) {
        (Some(latest), Some(current)) => latest > current,
        _ => latest != current,
    }
}

/// HTTPS with the usual web certificates.
fn http() -> Result<reqwest::Client, String> {
    let mut roots = rustls::RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let tls = rustls::ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
        .with_safe_default_protocol_versions()
        .map_err(|e| format!("cannot set up HTTPS: {e}"))?
        .with_root_certificates(roots)
        .with_no_client_auth();
    reqwest::Client::builder()
        .tls_backend_preconfigured(tls)
        .user_agent(format!("tovuer/{}", crate::VERSION))
        .connect_timeout(Duration::from_secs(10))
        .build()
        .map_err(|e| format!("cannot set up HTTPS: {e}"))
}

/// Ask GitHub for the newest release.
pub async fn latest(client: &reqwest::Client) -> Result<Release, String> {
    let url = format!("https://api.github.com/repos/{REPO}/releases/latest");
    let resp = client
        .get(&url)
        .header("Accept", "application/vnd.github+json")
        .timeout(Duration::from_secs(20))
        .send()
        .await
        .map_err(|e| format!("GitHub is out of reach: {e}"))?;
    match resp.status().as_u16() {
        200 => {}
        404 => return Err("there are no releases on GitHub yet".into()),
        403 | 429 => return Err("GitHub asks to wait a little — try again later".into()),
        code => return Err(format!("GitHub answered {code}")),
    }
    let text = resp.text().await.map_err(|e| format!("GitHub is out of reach: {e}"))?;
    parse_release(&text, &asset_name())
}

/// Fetch a build, checking it is the size and the checksum GitHub told.
async fn download(client: &reqwest::Client, build: &Build, mut progress: impl FnMut(u64, u64)) -> Result<Vec<u8>, String> {
    if build.size > MAX_BUILD {
        return Err("the build is too big".into());
    }
    let net = |e: reqwest::Error| format!("the download broke off: {e}");
    let mut resp = client.get(&build.url).timeout(Duration::from_secs(600)).send().await.map_err(net)?;
    if !resp.status().is_success() {
        return Err(format!("GitHub answered {} for the file", resp.status().as_u16()));
    }
    let mut data = Vec::with_capacity(build.size as usize);
    let mut last = Instant::now();
    while let Some(chunk) = resp.chunk().await.map_err(net)? {
        data.extend_from_slice(&chunk);
        if data.len() as u64 > build.size {
            return Err("the file is bigger than GitHub said".into());
        }
        if last.elapsed() > Duration::from_millis(100) {
            last = Instant::now();
            progress(data.len() as u64, build.size);
        }
    }
    if data.len() as u64 != build.size {
        return Err("the download stopped halfway — try again".into());
    }
    if let Some(sum) = build.sha256
        && Sha256::digest(&data).as_slice() != sum
    {
        return Err("the download is damaged (the checksum differs) — try again".into());
    }
    progress(build.size, build.size);
    Ok(data)
}

/// `/update`: the newest release from GitHub in place of this program.
/// `Ok(None)` — this is the newest already.
pub fn update(current: String, events: Events) {
    tokio::spawn(async move {
        let result = async {
            let client = http()?;
            let release = latest(&client).await?;
            if !newer(&release.version, &current) {
                return Ok(None);
            }
            let Some(build) = release.build else {
                return Err(format!("version {} has no build for this system ({}) yet", release.version, asset_name()));
            };
            let progress = {
                let events = events.clone();
                move |got, total| {
                    let _ = events.send(TaskEvent::UpdateProgress { got, total });
                }
            };
            let data = download(&client, &build, progress).await?;
            if !looks_like_executable(&data) {
                return Err("the download is not a program for this system".to_string());
            }
            let exe = tokio::task::spawn_blocking(move || replace_self(&data))
                .await
                .map_err(|e| format!("internal error: {e}"))?
                .map_err(|e| format!("cannot replace the program file: {e}"))?;
            Ok(Some((exe, release.version)))
        }
        .await;
        let _ = events.send(TaskEvent::UpdateDone(result));
    });
}

/// At start: is there a newer release for this system? Any trouble — quietly nothing.
pub fn check(current: String, events: Events) {
    tokio::spawn(async move {
        let Ok(client) = http() else { return };
        if let Ok(release) = latest(&client).await
            && release.build.is_some()
            && newer(&release.version, &current)
        {
            let _ = events.send(TaskEvent::UpdateFound(release.version));
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    const ANSWER: &str = r#"{
        "tag_name": "v2.1.0",
        "assets": [
            {"name": "tovuer-linux-amd64", "size": 24000000,
             "browser_download_url": "https://github.com/dszhlt/tovuer/releases/download/v2.1.0/tovuer-linux-amd64",
             "digest": "sha256:00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff"},
            {"name": "tovuer-windows-amd64.exe", "size": 25000000,
             "browser_download_url": "https://evil.example/tovuer-windows-amd64.exe"}
        ]
    }"#;

    #[test]
    fn a_release_gives_its_version_and_this_systems_build() {
        let r = parse_release(ANSWER, "tovuer-linux-amd64").unwrap();
        assert_eq!(r.version, "2.1.0");
        let b = r.build.unwrap();
        assert_eq!(b.size, 24_000_000);
        assert!(b.url.ends_with("/v2.1.0/tovuer-linux-amd64"));
        assert_eq!(b.sha256.unwrap()[1], 0x11);
        assert!(parse_release(ANSWER, "tovuer-macos-arm64").unwrap().build.is_none(), "no build for this system");
        assert!(parse_release(ANSWER, "tovuer-windows-amd64.exe").unwrap().build.is_none(), "a file from elsewhere is not taken");
        assert!(parse_release("<html>rate limited</html>", "x").is_err());
        assert!(parse_release(r#"{"tag_name": "v1; rm -rf"}"#, "x").is_err());
    }

    #[test]
    fn versions_go_forward_only() {
        assert!(newer("2.1.0", "2.0.0"));
        assert!(newer("2.10.0", "2.9.3"), "numbers, not letters");
        assert!(!newer("2.0.0", "2.0.0"));
        assert!(!newer("2.0.0", "2.1.0"), "never back");
        assert!(newer("2.1.0", "20261125-0445"), "a build made by hand gets the release");
        assert!(!newer("2.1.0-rc1", "2.1.0"));
    }

    #[test]
    fn checksums_read_from_hex() {
        assert_eq!(unhex32(&"ab".repeat(32)), Some([0xab; 32]));
        assert_eq!(unhex32("ab"), None);
        assert_eq!(unhex32(&"zz".repeat(32)), None);
    }

    #[tokio::test]
    async fn https_can_be_set_up() {
        assert!(http().is_ok());
    }

    /// A one-request web server on loopback that answers with `body`.
    async fn serve(body: Vec<u8>, length: usize) -> String {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/tovuer", listener.local_addr().unwrap());
        tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut req = [0u8; 2048];
            let _ = sock.read(&mut req).await;
            let head = format!("HTTP/1.1 200 OK\r\ncontent-length: {length}\r\nconnection: close\r\n\r\n");
            let _ = sock.write_all(head.as_bytes()).await;
            let _ = sock.write_all(&body).await;
        });
        url
    }

    #[tokio::test]
    async fn a_build_is_taken_only_whole_and_unchanged() {
        let client = http().unwrap();
        let data = b"\x7fELF a new build".repeat(1000);
        let sum: [u8; 32] = Sha256::digest(&data).into();
        let build = |url: String, sha256| Build { url, size: data.len() as u64, sha256 };

        let got = download(&client, &build(serve(data.clone(), data.len()).await, Some(sum)), |_, _| {}).await;
        assert_eq!(got.as_deref(), Ok(&data[..]));

        let mut damaged = data.clone();
        damaged[100] ^= 1;
        let got = download(&client, &build(serve(damaged, data.len()).await, Some(sum)), |_, _| {}).await;
        assert!(got.unwrap_err().contains("damaged"));

        let half = data[..data.len() / 2].to_vec();
        let got = download(&client, &build(serve(half.clone(), half.len()).await, Some(sum)), |_, _| {}).await;
        assert!(got.unwrap_err().contains("halfway"));

        let more = data.repeat(2);
        let got = download(&client, &build(serve(more.clone(), more.len()).await, None), |_, _| {}).await;
        assert!(got.unwrap_err().contains("bigger"));
    }
}
