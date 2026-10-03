//! Azure's OpenCode OAuth entries are account markers, not bearer tokens.
//! Obtain the actual short-lived token from the operator's Azure CLI session.
use anyhow::{bail, Context, Result};
use serde_json::Value;
use std::{
    future::Future,
    path::{Path, PathBuf},
    process::Stdio,
    time::{SystemTime, UNIX_EPOCH},
};
use tokio::process::Command;

pub const COGNITIVE_SERVICES_SCOPE: &str = "https://cognitiveservices.azure.com/.default";
pub const FOUNDRY_SCOPE: &str = "https://ai.azure.com/.default";

/// Deliberately has no Debug implementation to keep access tokens out of logs.
pub struct AzureCliToken {
    pub access: String,
    /// UTC epoch milliseconds, matching persisted OpenCode OAuth expiry.
    pub expires: u64,
}

pub async fn acquire(scope: &str) -> Result<AzureCliToken> {
    let command = std::env::var_os("OPENRAID_AZ")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("az"));
    acquire_with_command(scope, &command).await
}

/// Explicit command injection keeps token-cache tests isolated from process env.
pub async fn acquire_with_command(scope: &str, program: &Path) -> Result<AzureCliToken> {
    acquire_with(scope, |arguments| run_cli(program.to_owned(), arguments)).await
}

async fn acquire_with<F, Fut>(scope: &str, runner: F) -> Result<AzureCliToken>
where
    F: FnOnce(Vec<String>) -> Fut,
    Fut: Future<Output = Result<Value>>,
{
    // Fixed audiences avoid passing shell syntax through Windows' az.cmd shim.
    if !matches!(scope, COGNITIVE_SERVICES_SCOPE | FOUNDRY_SCOPE) {
        bail!("unsupported Azure OAuth scope; choose Cognitive Services or Foundry");
    }
    let arguments = [
        "account",
        "get-access-token",
        "--scope",
        scope,
        "--output",
        "json",
        "--only-show-errors",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    let response = runner(arguments).await?;
    parse_token(&response, now_ms()?)
}

async fn run_cli(program: PathBuf, arguments: Vec<String>) -> Result<Value> {
    #[cfg(windows)]
    let mut command = {
        use std::os::windows::process::CommandExt;
        let native = program
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| extension.eq_ignore_ascii_case("exe"));
        let mut command = if native {
            let mut command = Command::new(&program);
            command.args(&arguments);
            command
        } else {
            let path = program
                .to_str()
                .context("Azure CLI command path is not valid Unicode")?;
            if path.contains('"') {
                bail!("Azure CLI command path contains an invalid quote");
            }
            let mut command = Command::new("cmd.exe");
            command.args(["/D", "/S", "/C"]);
            // Only fixed, validated arguments enter this command string; the
            // outer quotes preserve a Windows shim path containing spaces.
            command
                .as_std_mut()
                .raw_arg(format!("\"\"{path}\" {}\"", arguments.join(" ")));
            command
        };
        command.creation_flags(0x08000000);
        command
    };
    #[cfg(not(windows))]
    let mut command = {
        let mut command = Command::new(&program);
        command.args(&arguments);
        command
    };
    let output = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .output()
        .await
        .map_err(|_| {
            anyhow::anyhow!("Azure OAuth requires Azure CLI; install it and run az login")
        })?;
    if !output.status.success() {
        bail!("Azure CLI could not issue an access token; run az login and verify the selected account/subscription");
    }
    serde_json::from_slice(&output.stdout).map_err(|_| {
        anyhow::anyhow!(
            "Azure CLI returned invalid access-token JSON; update Azure CLI and run az login"
        )
    })
}

fn parse_token(response: &Value, now: u64) -> Result<AzureCliToken> {
    let access = response["accessToken"]
        .as_str()
        .filter(|token| !token.trim().is_empty())
        .context("Azure CLI returned no access token; run az login")?;
    // expires_on is a UTC POSIX timestamp. expiresOn is local-wall-clock text
    // in legacy CLIs and must not be interpreted as UTC or guessed from it.
    let seconds = response["expires_on"]
        .as_u64()
        .or_else(|| {
            response["expires_on"]
                .as_str()
                .and_then(|value| value.parse().ok())
        })
        .context("Azure CLI returned no valid UTC expires_on timestamp; update Azure CLI")?;
    let expires = seconds
        .checked_mul(1_000)
        .context("Azure CLI returned an invalid token expiry")?;
    if expires <= now {
        bail!("Azure CLI returned an expired access token; run az login");
    }
    Ok(AzureCliToken {
        access: access.to_owned(),
        expires,
    })
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
    use serde_json::json;

    #[tokio::test]
    async fn mock_cli_uses_exact_audiences_and_returns_epoch_milliseconds() -> Result<()> {
        for scope in [COGNITIVE_SERVICES_SCOPE, FOUNDRY_SCOPE] {
            let seconds = now_ms()? / 1_000 + 3_600;
            let token = acquire_with(scope, |arguments| async move {
                assert_eq!(arguments, ["account", "get-access-token", "--scope", scope, "--output", "json", "--only-show-errors"]);
                Ok(json!({"accessToken":"isolated-token","expires_on":seconds,"expiresOn":"misleading-local-time"}))
            }).await?;
            assert_eq!(token.access, "isolated-token");
            assert_eq!(token.expires, seconds * 1_000);
        }
        Ok(())
    }

    #[test]
    fn invalid_cli_results_never_echo_tokens_and_utc_expiry_is_required() -> Result<()> {
        let token = parse_token(
            &json!({"accessToken":"isolated-token","expires_on":"123"}),
            1,
        )?;
        assert_eq!(token.expires, 123_000);
        for response in [
            json!({"accessToken":"do-not-print-me","expiresOn":"2099-01-01 00:00:00"}),
            json!({"accessToken":"do-not-print-me","expires_on":1}),
            json!({"accessToken":"do-not-print-me","expires_on":u64::MAX}),
            json!({"expires_on":123}),
        ] {
            let error = parse_token(&response, 2_000)
                .err()
                .context("malformed token unexpectedly accepted")?;
            assert!(!error.to_string().contains("do-not-print-me"));
        }
        Ok(())
    }

    #[tokio::test]
    async fn unknown_scope_never_runs_the_command() {
        let result = acquire_with("https://example.test/.default & injected", |_| async {
            panic!("invalid scope must be rejected before command invocation");
            #[allow(unreachable_code)]
            Ok(json!({}))
        })
        .await;
        assert!(result.is_err());
    }
}
