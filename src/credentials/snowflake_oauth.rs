//! Snowflake Cortex's built-in OpenCode OAuth account format and refresh flow.
//! The parent OAuthSession owns refresh serialization and credential persistence.

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use std::time::{SystemTime, UNIX_EPOCH};

const CLIENT_ID: &str = "LOCAL_APPLICATION";
const REFRESH_SKEW_MS: u64 = 120_000;

fn field<'a>(value: &'a Value, name: &str) -> Option<&'a str> {
    value[name]
        .as_str()
        .filter(|value| !value.trim().is_empty())
        .or_else(|| {
            value["metadata"][name]
                .as_str()
                .filter(|value| !value.trim().is_empty())
        })
}

/// Accept the account identifier, or the account URL accepted by OpenCode's
/// login form. This is routing metadata, not an access or refresh token.
pub fn account_id(credential: &Value) -> Result<String> {
    let raw = field(credential, "accountId")
        .or_else(|| field(credential, "account_id"))
        .context("Snowflake OAuth account is missing accountId; reconnect with a Snowflake account identifier")?;
    normalize_account(raw)
}

fn normalize_account(raw: &str) -> Result<String> {
    let raw = raw.trim();
    let account = raw
        .strip_prefix("https://")
        .or_else(|| raw.strip_prefix("http://"))
        .unwrap_or(raw)
        .trim_end_matches('/');
    let account = account
        .strip_suffix(".snowflakecomputing.com")
        .unwrap_or(account);
    if account.is_empty()
        || account.starts_with('.')
        || account.ends_with('.')
        || account.contains("..")
        || !account.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '.' | '-' | '_')
        })
    {
        bail!("Snowflake account must be an account identifier or a Snowflake account hostname");
    }
    Ok(account.to_owned())
}

pub fn api_base_url(credential: &Value) -> Result<String> {
    Ok(format!(
        "https://{}.snowflakecomputing.com/api/v2/cortex/v1",
        account_id(credential)?
    ))
}

/// Refresh before the two-minute expiry window, matching the upstream loader.
/// Call this while holding the session refresh lock; returned values never
/// modify the imported OpenCode file and retain unknown credential metadata.
pub async fn refresh_if_needed(client: &reqwest::Client, credential: &Value) -> Result<Value> {
    let account = account_id(credential)?;
    let endpoint = format!("https://{account}.snowflakecomputing.com/oauth/token-request");
    refresh_if_needed_at(client, credential, &endpoint).await
}

/// An explicit refresh endpoint supports isolated delegated-session tests and
/// account proxies. Production callers normally use `refresh_if_needed`.
pub async fn refresh_if_needed_at(
    client: &reqwest::Client,
    credential: &Value,
    endpoint: &str,
) -> Result<Value> {
    account_id(credential)?;
    let now = now_ms()?;
    if field(credential, "access").is_some()
        && credential["expires"].as_u64().unwrap_or(0) > now.saturating_add(REFRESH_SKEW_MS)
    {
        return Ok(credential.clone());
    }
    refresh_at(client, credential, endpoint, now).await
}

async fn refresh_at(
    client: &reqwest::Client,
    credential: &Value,
    endpoint: &str,
    now: u64,
) -> Result<Value> {
    let refresh = field(credential, "refresh")
        .context("Snowflake OAuth access expired without a refresh token; reconnect the account")?;
    let response = client
        .post(endpoint)
        .basic_auth(CLIENT_ID, Some(CLIENT_ID))
        .header(reqwest::header::ACCEPT, "application/json")
        .header(
            reqwest::header::USER_AGENT,
            concat!("openraid/", env!("CARGO_PKG_VERSION")),
        )
        .form(&[
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh),
            ("client_id", CLIENT_ID),
        ])
        .send()
        .await
        .map_err(|_| anyhow::anyhow!("Snowflake OAuth refresh connection failed"))?;
    if !response.status().is_success() {
        // Server bodies can echo credentials; report only status and action.
        bail!("Snowflake OAuth refresh returned HTTP {}; reconnect the account if authorization was revoked", response.status());
    }
    let tokens: Value = response
        .json()
        .await
        .map_err(|_| anyhow::anyhow!("Snowflake OAuth refresh returned invalid JSON"))?;
    let access = field(&tokens, "access_token")
        .context("Snowflake OAuth refresh returned no access token")?;
    let mut updated = credential.clone();
    updated["access"] = json!(access);
    if let Some(refresh) = field(&tokens, "refresh_token") {
        updated["refresh"] = json!(refresh);
    }
    updated["expires"] = json!(now.saturating_add(
        tokens["expires_in"]
            .as_u64()
            .unwrap_or(600)
            .saturating_mul(1_000)
    ));
    Ok(updated)
}

fn now_ms() -> Result<u64> {
    Ok(SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock is before the Unix epoch")?
        .as_millis()
        .min(u64::MAX as u128) as u64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[test]
    fn snowflake_account_routing_matches_imported_metadata() -> Result<()> {
        assert_eq!(
            account_id(&json!({"accountId":" https://org-account.snowflakecomputing.com/ "}))?,
            "org-account"
        );
        assert_eq!(
            api_base_url(&json!({"metadata":{"accountId":"xy12345.us-east-1"}}))?,
            "https://xy12345.us-east-1.snowflakecomputing.com/api/v2/cortex/v1"
        );
        assert!(account_id(&json!({"accountId":"account@other-host"})).is_err());
        assert!(
            account_id(&json!({"accountId":"https://account.snowflakecomputing.com/path"}))
                .is_err()
        );
        assert!(account_id(&json!({})).is_err());
        Ok(())
    }

    #[tokio::test]
    async fn unexpired_credential_is_reused_without_a_refresh_request() -> Result<()> {
        let credential = json!({"type":"oauth", "accountId":"org-account", "access":"already-valid", "refresh":"rotating", "expires":now_ms()?.saturating_add(600_000), "extra":{"retained":true}});
        assert_eq!(
            refresh_if_needed(&reqwest::Client::new(), &credential).await?,
            credential
        );
        Ok(())
    }

    async fn token_server(
        status: &'static str,
        body: Value,
    ) -> Result<(String, tokio::task::JoinHandle<Result<String>>)> {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        let task = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await?;
            let mut bytes = Vec::new();
            let end = loop {
                let mut chunk = [0; 4096];
                let read = socket.read(&mut chunk).await?;
                if read == 0 {
                    bail!("request ended before HTTP headers");
                }
                bytes.extend_from_slice(&chunk[..read]);
                if let Some(end) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
                    break end + 4;
                }
            };
            let headers = std::str::from_utf8(&bytes[..end])?;
            let length = headers
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().ok())
                        .flatten()
                })
                .context("missing request length")?;
            while bytes.len() < end + length {
                let mut chunk = [0; 4096];
                let read = socket.read(&mut chunk).await?;
                if read == 0 {
                    bail!("request ended before HTTP body");
                }
                bytes.extend_from_slice(&chunk[..read]);
            }
            let body = body.to_string();
            socket.write_all(format!("HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await?;
            socket.shutdown().await?;
            Ok(String::from_utf8(bytes)?)
        });
        Ok((format!("http://{address}/oauth/token-request"), task))
    }

    #[tokio::test]
    async fn refresh_uses_public_client_basic_auth_and_preserves_rotation_metadata() -> Result<()> {
        let (endpoint, server) = token_server(
            "200 OK",
            json!({"access_token":"new-access", "refresh_token":"new-refresh", "expires_in":600}),
        )
        .await?;
        let original = json!({"type":"oauth", "access":"old-access", "refresh":"old-refresh", "expires":0, "accountId":"org-account", "extra":{"retained":true}});
        let updated = refresh_at(&reqwest::Client::new(), &original, &endpoint, 1000).await?;
        assert_eq!(updated["access"], "new-access");
        assert_eq!(updated["refresh"], "new-refresh");
        assert_eq!(updated["expires"], 601_000);
        assert_eq!(updated["extra"], original["extra"]);
        assert_eq!(original["access"], "old-access");
        let request = server.await??;
        assert!(request.to_ascii_lowercase().contains(
            "authorization: basic TE9DQUxfQVBQTElDQVRJT046TE9DQUxfQVBQTElDQVRJT04="
                .to_ascii_lowercase()
                .as_str()
        ));
        assert!(request.contains("grant_type=refresh_token"));
        assert!(request.contains("refresh_token=old-refresh"));
        assert!(request.contains("client_id=LOCAL_APPLICATION"));
        Ok(())
    }

    #[tokio::test]
    async fn optional_rotation_is_preserved_and_server_secrets_are_not_reported() -> Result<()> {
        let original = json!({"type":"oauth", "refresh":"preserved", "accountId":"org-account"});
        let (endpoint, server) =
            token_server("200 OK", json!({"access_token":"new-access"})).await?;
        let updated = refresh_at(&reqwest::Client::new(), &original, &endpoint, 1000).await?;
        assert_eq!(updated["refresh"], "preserved");
        assert_eq!(updated["expires"], 601_000);
        server.await??;
        let (endpoint, server) = token_server(
            "401 Unauthorized",
            json!({"error":"credential-secret-echo"}),
        )
        .await?;
        let error = refresh_at(&reqwest::Client::new(), &original, &endpoint, 1000)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("401"));
        assert!(!format!("{error:#}").contains("credential-secret-echo"));
        server.await??;
        Ok(())
    }
}
