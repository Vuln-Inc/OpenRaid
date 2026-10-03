//! Imported GitLab OAuth credentials, matching OpenCode's GitLab auth plugin.
//! OAuthSession owns the refresh mutex and persists the returned rotated record.
use anyhow::{bail, Context, Result};
use futures_util::StreamExt;
use serde_json::{json, Value};
use std::time::{SystemTime, UNIX_EPOCH};

const CLIENT_ID: &str = "1d89f9fdb23ee96d4e603201f6861dab6e143c5c3c00469a018a2d94bdc03d4e";
const REDIRECT_URI: &str = "http://127.0.0.1:8080/callback";
const REFRESH_SKEW_MS: u64 = 5 * 60 * 1000;
const MAX_TOKEN_BYTES: usize = 64 * 1024;

/// Supports OpenCode v1 enterpriseUrl and v2 credential metadata.instanceUrl.
pub fn instance_url(credential: &Value) -> Result<String> {
    let environment = std::env::var("GITLAB_INSTANCE_URL").ok();
    let source = credential["enterpriseUrl"]
        .as_str()
        .filter(|url| !url.trim().is_empty())
        .or_else(|| {
            credential["metadata"]["instanceUrl"]
                .as_str()
                .filter(|url| !url.trim().is_empty())
        })
        .or_else(|| environment.as_deref().filter(|url| !url.trim().is_empty()))
        .unwrap_or("https://gitlab.com");
    let mut url = reqwest::Url::parse(source.trim()).context("invalid GitLab instance URL")?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        bail!("GitLab instance URL must be an HTTP or HTTPS instance address");
    }
    let path = url.path().trim_end_matches('/').to_owned();
    url.set_path(&path);
    Ok(url.as_str().trim_end_matches('/').to_owned())
}

pub async fn refresh_if_needed(client: &reqwest::Client, credential: &Value) -> Result<Value> {
    if credential["type"].as_str() != Some("oauth") {
        bail!("GitLab account credential must use OAuth");
    }
    let now = now_ms()?;
    let expires = credential["expires"]
        .as_u64()
        .context("GitLab OAuth credential has no valid expiration")?;
    let access = nonempty(&credential["access"]);
    if access.is_some() && expires > now.saturating_add(REFRESH_SKEW_MS) {
        return Ok(credential.clone());
    }
    let refresh = nonempty(&credential["refresh"])
        .context("GitLab OAuth refresh token is missing; reconnect the account")?;
    let instance = instance_url(credential)?;
    let environment_client = std::env::var("GITLAB_OAUTH_CLIENT_ID").ok();
    let client_id = nonempty(&credential["metadata"]["clientId"])
        .or_else(|| nonempty(&credential["clientId"]))
        .or_else(|| {
            environment_client
                .as_deref()
                .filter(|value| !value.trim().is_empty())
        })
        .unwrap_or(CLIENT_ID);
    let redirect = nonempty(&credential["metadata"]["redirectUri"])
        .or_else(|| nonempty(&credential["redirectUri"]))
        .unwrap_or(REDIRECT_URI);
    let response = client
        .post(format!("{instance}/oauth/token"))
        .header("Accept", "application/json")
        .form(&[
            ("grant_type", "refresh_token"),
            ("client_id", client_id),
            ("refresh_token", refresh),
            ("redirect_uri", redirect),
        ])
        .send()
        .await
        .map_err(|_| {
            anyhow::anyhow!(
                "GitLab OAuth refresh connection failed; check the instance and sign-in"
            )
        })?;
    if !response.status().is_success() {
        // Never forward the response body: OAuth errors may echo credentials.
        bail!("GitLab OAuth refresh returned HTTP {}; reconnect the account or check GITLAB_OAUTH_CLIENT_ID", response.status());
    }
    let mut stream = response.bytes_stream();
    let mut bytes = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk =
            chunk.map_err(|_| anyhow::anyhow!("GitLab OAuth refresh response was interrupted"))?;
        if bytes.len().saturating_add(chunk.len()) > MAX_TOKEN_BYTES {
            bail!("GitLab OAuth refresh response exceeds the token response limit");
        }
        bytes.extend_from_slice(&chunk);
    }
    let tokens: Value = serde_json::from_slice(&bytes)
        .map_err(|_| anyhow::anyhow!("GitLab OAuth refresh returned invalid JSON"))?;
    let access = nonempty(&tokens["access_token"])
        .context("GitLab OAuth refresh returned no access token")?;
    let lifetime = tokens["expires_in"]
        .as_u64()
        .filter(|seconds| *seconds > 0)
        .context("GitLab OAuth refresh returned no valid token lifetime")?;
    let mut refreshed = credential.clone();
    refreshed["access"] = json!(access);
    if let Some(refresh) = nonempty(&tokens["refresh_token"]) {
        refreshed["refresh"] = json!(refresh);
    }
    refreshed["expires"] = json!(now_ms()?.saturating_add(lifetime.saturating_mul(1000)));
    Ok(refreshed)
}

fn nonempty(value: &Value) -> Option<&str> {
    value.as_str().filter(|value| !value.trim().is_empty())
}

fn now_ms() -> Result<u64> {
    Ok(SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock precedes the Unix epoch")?
        .as_millis()
        .min(u128::from(u64::MAX)) as u64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    async fn mock_refresh(
        status: &str,
        response: Value,
    ) -> Result<(String, tokio::task::JoinHandle<Result<String>>)> {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let base = format!("http://{}/gitlab", listener.local_addr()?);
        let status = status.to_owned();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await?;
            let mut bytes = Vec::new();
            let header_end = loop {
                let mut chunk = [0u8; 2048];
                let count = socket.read(&mut chunk).await?;
                if count == 0 {
                    bail!("mock refresh closed before headers");
                }
                bytes.extend_from_slice(&chunk[..count]);
                if let Some(end) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
                    break end + 4;
                }
            };
            let headers = std::str::from_utf8(&bytes[..header_end])?;
            let length: usize = headers
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().ok())
                        .flatten()
                })
                .context("mock request lacks content length")?;
            while bytes.len() < header_end + length {
                let mut chunk = [0u8; 2048];
                let count = socket.read(&mut chunk).await?;
                if count == 0 {
                    bail!("mock refresh closed before body");
                }
                bytes.extend_from_slice(&chunk[..count]);
            }
            let body = response.to_string();
            let reply = format!("HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
            socket.write_all(reply.as_bytes()).await?;
            Ok(String::from_utf8(bytes)?)
        });
        Ok((base, server))
    }

    #[tokio::test]
    async fn refresh_rotates_tokens_preserves_metadata_and_sends_pkce_client_form() -> Result<()> {
        let (base, server) = mock_refresh(
            "200 OK",
            json!({"access_token":"new-access","refresh_token":"new-refresh","expires_in":7200}),
        )
        .await?;
        let credential = json!({"type":"oauth","access":"old-access","refresh":"old-refresh","expires":1,
            "enterpriseUrl":base,"metadata":{"clientId":"custom-client","instanceUrl":base,"extra":"kept"},"unknown":"kept"});
        let refreshed = refresh_if_needed(&reqwest::Client::new(), &credential).await?;
        let request = server.await??;
        assert!(request.starts_with("POST /gitlab/oauth/token HTTP/1.1\r\n"));
        let form: std::collections::BTreeMap<_, _> = reqwest::Url::parse(&format!(
            "http://localhost/?{}",
            request.split_once("\r\n\r\n").unwrap().1
        ))?
        .query_pairs()
        .into_owned()
        .collect();
        assert_eq!(form["grant_type"], "refresh_token");
        assert_eq!(form["client_id"], "custom-client");
        assert_eq!(form["refresh_token"], "old-refresh");
        assert_eq!(form["redirect_uri"], REDIRECT_URI);
        assert_eq!(refreshed["access"], "new-access");
        assert_eq!(refreshed["refresh"], "new-refresh");
        assert_eq!(refreshed["metadata"], credential["metadata"]);
        assert_eq!(refreshed["unknown"], "kept");
        assert!(refreshed["expires"].as_u64().unwrap() > now_ms()?.saturating_add(7_000_000));
        Ok(())
    }

    #[tokio::test]
    async fn valid_tokens_avoid_network_and_failed_refresh_never_echoes_secret_body() -> Result<()>
    {
        let valid = json!({"type":"oauth","access":"valid-access","refresh":"valid-refresh","expires":now_ms()? + 600_000,
            "enterpriseUrl":"http://127.0.0.1:1"});
        assert_eq!(
            refresh_if_needed(&reqwest::Client::new(), &valid).await?,
            valid
        );
        let (base, server) = mock_refresh(
            "401 Unauthorized",
            json!({"error":"secret-echo-old-refresh"}),
        )
        .await?;
        let invalid = json!({"type":"oauth","access":"old-access","refresh":"old-refresh","expires":0,"enterpriseUrl":base});
        let error = refresh_if_needed(&reqwest::Client::new(), &invalid)
            .await
            .unwrap_err();
        server.await??;
        assert!(error.to_string().contains("401"));
        assert!(!format!("{error:#}").contains("secret-echo"));
        assert!(!format!("{error:#}").contains("old-refresh"));
        Ok(())
    }

    #[test]
    fn instance_metadata_and_enterprise_addresses_resolve_without_losing_base_paths() -> Result<()>
    {
        assert_eq!(
            instance_url(&json!({"enterpriseUrl":"https://gitlab.example/group/"}))?,
            "https://gitlab.example/group"
        );
        assert_eq!(
            instance_url(&json!({"metadata":{"instanceUrl":"https://gitlab.example/"}}))?,
            "https://gitlab.example"
        );
        assert!(instance_url(&json!({"enterpriseUrl":"file:///tmp/gitlab"})).is_err());
        assert!(
            instance_url(&json!({"enterpriseUrl":"https://user:secret@gitlab.example"})).is_err()
        );
        Ok(())
    }
}
