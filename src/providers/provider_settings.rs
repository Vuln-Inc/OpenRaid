//! Resolve OpenCode-style configuration values and cloud endpoint templates.
//! An unresolved catalog template is not a URL: SDKs should use their own
//! environment/credential-chain defaults instead of sending literal braces.

use anyhow::{Context, Result};
use regex::Regex;
use serde_json::Value;
use std::{
    path::{Path, PathBuf},
    sync::OnceLock,
};

fn env_pattern() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| Regex::new(r"\{env:([A-Za-z_][A-Za-z0-9_]*)\}").unwrap())
}

fn file_pattern() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| Regex::new(r"\{file:([^{}]+)\}").unwrap())
}

fn endpoint_pattern() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| Regex::new(r"\$?\{([^{}]+)\}").unwrap())
}

/// Expand string values after JSON/JSONC parsing, preserving quotes/newlines
/// inside credentials and prompts rather than injecting them into JSON text.
pub fn expand_config(value: &Value, workspace: &Path) -> Result<Value> {
    expand_config_with(value, workspace, &|name| std::env::var(name).ok())
}

fn expand_config_with(
    value: &Value,
    workspace: &Path,
    env: &impl Fn(&str) -> Option<String>,
) -> Result<Value> {
    match value {
        Value::String(text) => {
            let expanded = env_pattern().replace_all(text, |captures: &regex::Captures<'_>| {
                env(&captures[1]).unwrap_or_default()
            });
            let mut output = String::new();
            let mut position = 0;
            for captures in file_pattern().captures_iter(&expanded) {
                let matched = captures.get(0).unwrap();
                output.push_str(&expanded[position..matched.start()]);
                let path = config_file_path(&captures[1], workspace, env)?;
                let content = std::fs::read_to_string(&path).with_context(|| {
                    format!("reading configuration file reference {}", path.display())
                })?;
                output.push_str(content.trim());
                position = matched.end();
            }
            output.push_str(&expanded[position..]);
            Ok(Value::String(output))
        }
        Value::Array(values) => values
            .iter()
            .map(|value| expand_config_with(value, workspace, env))
            .collect::<Result<Vec<_>>>()
            .map(Value::Array),
        Value::Object(values) => values
            .iter()
            .map(|(key, value)| Ok((key.clone(), expand_config_with(value, workspace, env)?)))
            .collect::<Result<serde_json::Map<_, _>>>()
            .map(Value::Object),
        _ => Ok(value.clone()),
    }
}

fn config_file_path(
    raw: &str,
    workspace: &Path,
    env: &impl Fn(&str) -> Option<String>,
) -> Result<PathBuf> {
    if raw == "~" || raw.starts_with("~/") || raw.starts_with("~\\") {
        let home = env("HOME")
            .or_else(|| env("USERPROFILE"))
            .context("home directory is unavailable for configuration file reference")?;
        return Ok(PathBuf::from(home).join(raw.get(2..).unwrap_or_default()));
    }
    let path = PathBuf::from(raw);
    Ok(if path.is_absolute() {
        path
    } else {
        workspace.join(path)
    })
}

/// Settings can be a factory settings object or the full provider-options
/// object with a nested `sdkSettings`. Explicit settings take precedence over
/// environment defaults. Missing template values return None for SDK routing.
pub fn expand_endpoint(template: &str, settings: &Value) -> Result<Option<String>> {
    expand_endpoint_with(template, settings, &|name| std::env::var(name).ok())
}

fn expand_endpoint_with(
    template: &str,
    settings: &Value,
    env: &impl Fn(&str) -> Option<String>,
) -> Result<Option<String>> {
    let mut result = String::new();
    let mut position = 0;
    for captures in endpoint_pattern().captures_iter(template) {
        let matched = captures.get(0).unwrap();
        let token = &captures[1];
        let Some(value) =
            endpoint_value(token, settings, env).filter(|value| !value.trim().is_empty())
        else {
            return Ok(None);
        };
        result.push_str(&template[position..matched.start()]);
        result.push_str(&value);
        position = matched.end();
    }
    result.push_str(&template[position..]);
    Ok(Some(result))
}

fn scalar(value: &Value) -> Option<String> {
    match value {
        Value::String(value) => Some(value.clone()),
        Value::Number(value) => Some(value.to_string()),
        _ => None,
    }
}

fn setting(settings: &Value, names: &[&str]) -> Option<String> {
    for source in [
        settings.get("sdkSettings").unwrap_or(&Value::Null),
        settings,
    ] {
        for name in names {
            if let Some(value) = source
                .get(*name)
                .and_then(scalar)
                .filter(|value| !value.trim().is_empty())
            {
                return Some(value);
            }
        }
    }
    None
}

fn endpoint_value(
    token: &str,
    settings: &Value,
    env: &impl Fn(&str) -> Option<String>,
) -> Option<String> {
    if let Some(name) = token.strip_prefix("env:") {
        return env(name);
    }
    let (names, variables): (&[&str], &[&str]) = match token {
        "region" | "AWS_REGION" | "AWS_DEFAULT_REGION" | "AWS_BEDROCK_REGION" => (
            &["region"],
            &["AWS_REGION", "AWS_DEFAULT_REGION", "AWS_BEDROCK_REGION"],
        ),
        "project"
        | "projectId"
        | "GOOGLE_CLOUD_PROJECT"
        | "GOOGLE_VERTEX_PROJECT"
        | "GCLOUD_PROJECT"
        | "GCP_PROJECT" => (
            &["project", "projectId"],
            &[
                "GOOGLE_CLOUD_PROJECT",
                "GOOGLE_VERTEX_PROJECT",
                "GCLOUD_PROJECT",
                "GCP_PROJECT",
            ],
        ),
        "location" | "GOOGLE_CLOUD_LOCATION" | "GOOGLE_VERTEX_LOCATION" | "VERTEX_LOCATION" => (
            &["location"],
            &[
                "GOOGLE_CLOUD_LOCATION",
                "GOOGLE_VERTEX_LOCATION",
                "VERTEX_LOCATION",
            ],
        ),
        "resourceName"
        | "AZURE_RESOURCE_NAME"
        | "AZURE_OPENAI_RESOURCE_NAME"
        | "AZURE_COGNITIVE_SERVICES_RESOURCE_NAME" => (
            &["resourceName"],
            &[
                "AZURE_RESOURCE_NAME",
                "AZURE_OPENAI_RESOURCE_NAME",
                "AZURE_COGNITIVE_SERVICES_RESOURCE_NAME",
            ],
        ),
        "accountId" | "account_id" | "CLOUDFLARE_ACCOUNT_ID" => {
            (&["accountId", "account_id"], &["CLOUDFLARE_ACCOUNT_ID"])
        }
        "gatewayId" | "gateway_id" | "CLOUDFLARE_GATEWAY_ID" => {
            (&["gatewayId", "gateway_id"], &["CLOUDFLARE_GATEWAY_ID"])
        }
        "deployment" | "deploymentId" | "deploymentName" => (
            &["deployment", "deploymentId", "deploymentName"],
            &["AZURE_OPENAI_DEPLOYMENT", "AZURE_DEPLOYMENT_NAME"],
        ),
        "tenantId" | "tenant_id" => (&["tenantId", "tenant_id"], &["AZURE_TENANT_ID"]),
        "SNOWFLAKE_ACCOUNT" => (
            &["account", "accountId", "account_id"],
            &["SNOWFLAKE_ACCOUNT"],
        ),
        _ => (&[], &[]),
    };
    if let Some(value) = setting(settings, &[token]).or_else(|| setting(settings, names)) {
        return Some(value);
    }
    if let Some(value) = env(token).filter(|value| !value.trim().is_empty()) {
        return Some(value);
    }
    variables
        .iter()
        .find_map(|name| env(name).filter(|value| !value.trim().is_empty()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn env_values_preserve_json_structure_and_secret_punctuation() -> Result<()> {
        let config = json!({"apiKey":"{env:KEY}","nested":["prefix {env:KEY} suffix",42],"missing":"{env:UNSET}"});
        let result = expand_config_with(&config, Path::new("."), &|name| {
            (name == "KEY").then(|| "quote\"\nslash\\".into())
        })?;
        assert_eq!(result["apiKey"], "quote\"\nslash\\");
        assert_eq!(result["nested"][1], 42);
        assert_eq!(result["missing"], "");
        assert_eq!(config["apiKey"], "{env:KEY}");
        Ok(())
    }

    #[test]
    fn file_references_are_relative_trimmed_and_expand_home() -> Result<()> {
        let directory = tempfile::tempdir()?;
        std::fs::write(directory.path().join("secret.txt"), "  key\n")?;
        let input = json!({"relative":"value {file:secret.txt}","home":"{file:~/secret.txt}"});
        let result = expand_config_with(&input, directory.path(), &|name| {
            (name == "HOME").then(|| directory.path().to_string_lossy().into_owned())
        })?;
        assert_eq!(result["relative"], "value key");
        assert_eq!(result["home"], "key");
        assert!(
            expand_config_with(&json!("{file:missing.txt}"), directory.path(), &|_| None).is_err()
        );
        Ok(())
    }

    #[test]
    fn nested_sdk_settings_override_environment_templates() -> Result<()> {
        let endpoint = expand_endpoint_with(
            "https://bedrock-runtime.{region}.amazonaws.com",
            &json!({"region":"top","sdkSettings":{"region":"chosen"}}),
            &|_| Some("environment".into()),
        )?;
        assert_eq!(
            endpoint.as_deref(),
            Some("https://bedrock-runtime.chosen.amazonaws.com")
        );
        let endpoint = expand_endpoint_with(
            "https://api.cloudflare.com/accounts/{CLOUDFLARE_ACCOUNT_ID}/gateway/{gatewayId}",
            &json!({"sdkSettings":{"accountId":"account", "gatewayId":"gateway"}}),
            &|_| None,
        )?;
        assert_eq!(
            endpoint.as_deref(),
            Some("https://api.cloudflare.com/accounts/account/gateway/gateway")
        );
        let endpoint = expand_endpoint_with(
            "https://{AZURE_COGNITIVE_SERVICES_RESOURCE_NAME}.services.ai.azure.com/models",
            &json!({"sdkSettings":{"resourceName":"chosen-resource"}}),
            &|_| None,
        )?;
        assert_eq!(
            endpoint.as_deref(),
            Some("https://chosen-resource.services.ai.azure.com/models")
        );
        let endpoint = expand_endpoint_with(
            "https://{SNOWFLAKE_ACCOUNT}.snowflakecomputing.com/api/v2/cortex/v1",
            &json!({"sdkSettings":{"accountId":"org-account"}}),
            &|_| None,
        )?;
        assert_eq!(
            endpoint.as_deref(),
            Some("https://org-account.snowflakecomputing.com/api/v2/cortex/v1")
        );
        Ok(())
    }

    #[test]
    fn cloud_aliases_and_unresolved_templates_use_sdk_defaults() -> Result<()> {
        let endpoint = expand_endpoint_with(
            "https://{location}-aiplatform.googleapis.com/projects/{project}",
            &json!({}),
            &|name| match name {
                "GOOGLE_VERTEX_LOCATION" => Some("europe-west1".into()),
                "GOOGLE_CLOUD_PROJECT" => Some("example".into()),
                _ => None,
            },
        )?;
        assert_eq!(
            endpoint.as_deref(),
            Some("https://europe-west1-aiplatform.googleapis.com/projects/example")
        );
        assert!(expand_endpoint_with(
            "https://{resourceName}.openai.azure.com",
            &json!({}),
            &|_| None
        )?
        .is_none());
        assert_eq!(
            expand_endpoint_with("https://example.com/v1", &json!({}), &|_| None)?.as_deref(),
            Some("https://example.com/v1")
        );
        Ok(())
    }

    #[test]
    fn models_dev_dollar_templates_do_not_leave_literal_dollars_in_urls() -> Result<()> {
        assert_eq!(
            expand_endpoint_with(
                "https://${SNOWFLAKE_ACCOUNT}.snowflakecomputing.com/api/v2/cortex/v1",
                &json!({"account":"org-account"}),
                &|_| None
            )?
            .as_deref(),
            Some("https://org-account.snowflakecomputing.com/api/v2/cortex/v1")
        );
        assert_eq!(
            expand_endpoint_with(
                "https://api.cloudflare.com/client/v4/accounts/${CLOUDFLARE_ACCOUNT_ID}/ai/v1",
                &json!({"accountId":"account"}),
                &|_| None
            )?
            .as_deref(),
            Some("https://api.cloudflare.com/client/v4/accounts/account/ai/v1")
        );
        Ok(())
    }
}
