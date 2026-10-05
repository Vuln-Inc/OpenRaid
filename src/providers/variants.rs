//! Model-specific thinking controls, following OpenCode's ProviderTransform.
//! Variant values use OpenCode's SDK option names so custom profiles can share
//! the same vocabulary. `wire_options` converts them at the HTTP boundary.

use serde_json::{json, Map, Value};
use std::collections::BTreeMap;

const COMMON: &[&str] = &["low", "medium", "high"];
const ALL: &[&str] = &["none", "minimal", "low", "medium", "high", "xhigh"];

fn efforts(names: &[&str], option: impl Fn(&str) -> Value) -> BTreeMap<String, Value> {
    names
        .iter()
        .map(|name| ((*name).into(), option(name)))
        .collect()
}

fn gpt5_version(id: &str) -> Option<u32> {
    let model = id.rsplit('/').next().unwrap_or(id);
    let rest = model
        .strip_prefix("gpt-5.")
        .or_else(|| model.strip_prefix("gpt-5-"))?;
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    digits.parse().ok()
}

fn gpt5_family(id: &str) -> bool {
    let model = id.rsplit('/').next().unwrap_or(id);
    model == "gpt-5" || model.starts_with("gpt-5.") || model.starts_with("gpt-5-")
}

fn openai_efforts(id: &str, release: &str, compatible: bool) -> Vec<&'static str> {
    if !compatible && id.contains("deep-research") {
        return vec!["medium"];
    }
    if gpt5_family(id) {
        let version = gpt5_version(id);
        if id.contains("-chat") {
            return if version.is_some() {
                vec!["medium"]
            } else {
                vec![]
            };
        }
        if id
            .rsplit('/')
            .next()
            .is_some_and(|v| v.starts_with("gpt-5-pro"))
        {
            return vec!["high"];
        }
        if id.contains("codex") {
            let mut result = COMMON.to_vec();
            if id.contains("codex-max") || version.is_some_and(|v| v >= 2) {
                result.push("xhigh");
            }
            if version.is_some_and(|v| v >= 3) {
                result.insert(0, "none");
            }
            return result;
        }
        if let Some(version) = version {
            if id.contains("-pro") || id.contains(".pro") {
                return vec!["medium", "high", "xhigh"];
            }
            let mut result = vec!["none", "low", "medium", "high"];
            if version >= 2 {
                result.push("xhigh");
            }
            return result;
        }
    }
    if compatible {
        return ALL.to_vec();
    }
    let mut result = COMMON.to_vec();
    if gpt5_family(id) {
        result.insert(0, "minimal");
    }
    if release >= "2025-11-13" {
        result.insert(0, "none");
    }
    if release >= "2025-12-04" {
        result.push("xhigh");
    }
    result
}

fn claude_version(id: &str) -> Option<(u32, u32)> {
    let tail = id.split("claude-").nth(1)?;
    let parts: Vec<_> = tail.split(['-', '.', '@']).collect();
    let start = parts.iter().position(|part| part.parse::<u32>().is_ok())?;
    let major = parts[start].parse().ok()?;
    let minor = parts
        .get(start + 1)
        .filter(|p| p.len() <= 2)
        .and_then(|p| p.parse().ok())
        .unwrap_or(0);
    Some((major, minor))
}

fn adaptive_efforts(id: &str) -> Option<(&'static [&'static str], bool)> {
    if !id.contains("claude-") {
        return None;
    }
    match claude_version(id) {
        Some((major, minor)) if major > 4 || (major == 4 && minor >= 7) => {
            Some((&["low", "medium", "high", "xhigh", "max"], true))
        }
        None => Some((&["low", "medium", "high", "xhigh", "max"], true)),
        Some((4, 6)) if id.contains("opus") || id.contains("sonnet") => {
            Some((&["low", "medium", "high", "max"], false))
        }
        _ => None,
    }
}

fn gemini25(id: &str) -> bool {
    id.contains("gemini-2.5") || id.contains("gemini-2-5")
}

fn legacy_gemini(id: &str) -> bool {
    [
        "gemini-1",
        "gemini-2",
        "gemini-flash-1",
        "gemini-flash-2",
        "gemini-pro-1",
        "gemini-pro-2",
    ]
    .iter()
    .any(|prefix| {
        id.split_once(prefix)
            .is_some_and(|(_, tail)| tail.is_empty() || tail.starts_with(['.', '-']))
    })
}

fn google_variants(id: &str) -> BTreeMap<String, Value> {
    if gemini25(id) {
        let maximum = if id.contains("pro") && !id.contains("flash") {
            32_768
        } else {
            24_576
        };
        return BTreeMap::from([
            (
                "high".into(),
                json!({"thinkingConfig":{"includeThoughts":true,"thinkingBudget":16000}}),
            ),
            (
                "max".into(),
                json!({"thinkingConfig":{"includeThoughts":true,"thinkingBudget":maximum}}),
            ),
        ]);
    }
    let names: &[&str] = if id.contains("gemma") || id.contains("flash-image") {
        &["minimal", "high"]
    } else if legacy_gemini(id) {
        &["low", "high"]
    } else if id.contains("pro-image") {
        &["high"]
    } else if id.contains("flash") {
        &["minimal", "low", "medium", "high"]
    } else {
        COMMON
    };
    efforts(
        names,
        |effort| json!({"thinkingConfig":{"includeThoughts":true,"thinkingLevel":effort}}),
    )
}

/// Derive only the thinking variants supported by this provider/model pair.
/// Empty means that the model uses its own defaults rather than selectable tiers.
#[allow(clippy::too_many_arguments)]
pub fn variants(
    provider_id: &str,
    npm: &str,
    model_id: &str,
    api_id: &str,
    reasoning: bool,
    release_date: &str,
    output_limit: usize,
) -> BTreeMap<String, Value> {
    if !reasoning {
        return BTreeMap::new();
    }
    let id = model_id.to_lowercase();
    let api = api_id.to_lowercase();
    let anthropic = matches!(npm, "@ai-sdk/anthropic" | "@ai-sdk/google-vertex/anthropic");
    let glm52 = ["glm-5.2", "glm-5-2", "glm-5p2"]
        .iter()
        .any(|name| id.contains(name) || api.contains(name));
    if api.contains("minimax-m3") && (anthropic || npm == "@ai-sdk/openai-compatible") {
        return if matches!(provider_id, "nvidia" | "lilac") {
            BTreeMap::from([
                (
                    "none".into(),
                    json!({"chat_template_kwargs":{"thinking_mode":"disabled"}}),
                ),
                (
                    "thinking".into(),
                    json!({"chat_template_kwargs":{"thinking_mode":"enabled"}}),
                ),
            ])
        } else {
            BTreeMap::from([
                ("none".into(), json!({"thinking":{"type":"disabled"}})),
                ("thinking".into(), json!({"thinking":{"type":"adaptive"}})),
            ])
        };
    }
    if glm52 {
        match npm {
            "@openrouter/ai-sdk-provider" => {
                return efforts(&["high", "xhigh"], |v| json!({"reasoning":{"effort":v}}))
            }
            "@ai-sdk/openai-compatible" => {
                return efforts(&["high", "max"], |v| json!({"reasoningEffort":v}))
            }
            "@ai-sdk/anthropic" => return efforts(&["high", "max"], |v| json!({"effort":v})),
            _ => {}
        }
    }
    if anthropic
        && [provider_id, api.as_str()]
            .iter()
            .any(|v| v.contains("kimi") || v.contains("moonshot"))
    {
        return efforts(
            &["low", "medium", "high", "xhigh", "max"],
            |v| json!({"thinking":{"type":"adaptive","display":"summarized"},"effort":v}),
        );
    }
    if [
        "deepseek-chat",
        "deepseek-reasoner",
        "deepseek-r1",
        "deepseek-v3",
        "minimax",
        "kimi",
        "k2p",
        "qwen",
        "big-pickle",
    ]
    .iter()
    .any(|name| id.contains(name))
        || (id.contains("glm") && !glm52)
    {
        return BTreeMap::new();
    }
    if id.contains("grok-3-mini") {
        return efforts(&["low", "high"], |v| {
            if npm == "@openrouter/ai-sdk-provider" {
                json!({"reasoning":{"effort":v}})
            } else {
                json!({"reasoningEffort":v})
            }
        });
    }
    let adaptive = adaptive_efforts(&api);
    match npm {
        "@openrouter/ai-sdk-provider" => {
            let names = if api.starts_with("openai/") || id.contains("gpt") {
                openai_efforts(&api, release_date, true)
            } else {
                COMMON.to_vec()
            };
            efforts(&names, |v| json!({"reasoning":{"effort":v}}))
        }
        "ai-gateway-provider" => {
            let names = if api.starts_with("openai/") {
                openai_efforts(&api, release_date, false)
            } else {
                COMMON.to_vec()
            };
            efforts(&names, |v| json!({"reasoningEffort":v}))
        }
        "@ai-sdk/gateway" if api.contains("google") => {
            if gemini25(&api) {
                google_variants(&api)
            } else {
                efforts(
                    &["low", "high"],
                    |v| json!({"includeThoughts":true,"thinkingLevel":v}),
                )
            }
        }
        "@ai-sdk/gateway" if !api.contains("anthropic") => efforts(
            &openai_efforts(&api, release_date, true),
            |v| json!({"reasoningEffort":v}),
        ),
        "@ai-sdk/github-copilot" => {
            if id.contains("gemini") {
                return BTreeMap::new();
            }
            if id.contains("claude") {
                return efforts(COMMON, |v| json!({"reasoningEffort":v}));
            }
            let mut names = COMMON.to_vec();
            if id.contains("5.1-codex-max")
                || id.contains("5.2")
                || id.contains("5.3")
                || (id.contains("gpt-5") && release_date >= "2025-12-04")
            {
                names.push("xhigh");
            }
            efforts(
                &names,
                |v| json!({"reasoningEffort":v,"reasoningSummary":"auto","include":["reasoning.encrypted_content"]}),
            )
        }
        "@ai-sdk/openai" | "@ai-sdk/azure" | "@ai-sdk/amazon-bedrock/mantle" => {
            if npm == "@ai-sdk/azure" && id == "o1-mini" {
                return BTreeMap::new();
            }
            let names = if provider_id == "meta" {
                ALL.to_vec()
            } else {
                openai_efforts(
                    if npm == "@ai-sdk/azure" { &id } else { &api },
                    release_date,
                    false,
                )
            };
            efforts(
                &names,
                |v| json!({"reasoningEffort":v,"reasoningSummary":"auto","include":["reasoning.encrypted_content"]}),
            )
        }
        "@ai-sdk/anthropic" | "@ai-sdk/google-vertex/anthropic" | "@ai-sdk/gateway" => {
            if let Some((names, modern)) = adaptive {
                let names: Vec<_> = names
                    .iter()
                    .copied()
                    .filter(|v| provider_id != "github-copilot" || (*v != "max" && *v != "xhigh"))
                    .collect();
                let names = if provider_id == "github-copilot" && api.contains("opus-4.7") {
                    vec!["medium"]
                } else {
                    names
                };
                return efforts(&names, |v| {
                    let mut value = json!({"thinking":{"type":"adaptive"},"effort":v});
                    if modern {
                        value["thinking"]["display"] = json!("summarized");
                    }
                    value
                });
            }
            if api.contains("opus-4-5") || api.contains("opus-4.5") {
                return efforts(
                    COMMON,
                    |v| json!({"thinking":{"type":"enabled","budgetTokens":output_limit.saturating_div(2).saturating_sub(1).min(16_000)},"effort":v}),
                );
            }
            let high = if npm == "@ai-sdk/gateway" {
                16_000
            } else {
                output_limit.saturating_div(2).saturating_sub(1).min(16_000)
            };
            let max = if npm == "@ai-sdk/gateway" {
                31_999
            } else {
                output_limit.saturating_sub(1).min(31_999)
            };
            BTreeMap::from([
                (
                    "high".into(),
                    json!({"thinking":{"type":"enabled","budgetTokens":high}}),
                ),
                (
                    "max".into(),
                    json!({"thinking":{"type":"enabled","budgetTokens":max}}),
                ),
            ])
        }
        "@ai-sdk/amazon-bedrock" => {
            if let Some((names, modern)) = adaptive {
                return efforts(names, |v| {
                    let mut value =
                        json!({"reasoningConfig":{"type":"adaptive","maxReasoningEffort":v}});
                    if modern {
                        value["reasoningConfig"]["display"] = json!("summarized");
                    }
                    value
                });
            }
            if api.contains("anthropic") {
                return BTreeMap::from([
                    (
                        "high".into(),
                        json!({"reasoningConfig":{"type":"enabled","budgetTokens":16000}}),
                    ),
                    (
                        "max".into(),
                        json!({"reasoningConfig":{"type":"enabled","budgetTokens":31999}}),
                    ),
                ]);
            }
            efforts(
                COMMON,
                |v| json!({"reasoningConfig":{"type":"enabled","maxReasoningEffort":v}}),
            )
        }
        "@ai-sdk/google" | "@ai-sdk/google-vertex" => google_variants(&api),
        "@ai-sdk/groq" => efforts(
            &["none", "low", "medium", "high"],
            |v| json!({"reasoningEffort":v}),
        ),
        "@ai-sdk/mistral" => {
            if [
                "mistral-small-2603",
                "mistral-small-latest",
                "mistral-medium-3.5",
                "mistral-medium-2604",
            ]
            .iter()
            .any(|v| api.contains(v))
            {
                efforts(&["high"], |v| json!({"reasoningEffort":v}))
            } else {
                BTreeMap::new()
            }
        }
        "@ai-sdk/cerebras"
        | "@ai-sdk/togetherai"
        | "@ai-sdk/xai"
        | "@ai-sdk/deepinfra"
        | "venice-ai-sdk-provider"
        | "@ai-sdk/openai-compatible" => {
            let names = if api.contains("north-mini-code") {
                vec!["none", "high"]
            } else if api.contains("deepseek-v4") {
                vec!["low", "medium", "high", "max"]
            } else {
                COMMON.to_vec()
            };
            efforts(&names, |v| json!({"reasoningEffort":v}))
        }
        "@jerome-benoit/sap-ai-provider-v2" => {
            let values = if id.contains("anthropic") {
                if let Some((names, modern)) = adaptive {
                    efforts(names, |v| {
                        let mut value =
                            json!({"thinking":{"type":"adaptive"},"output_config":{"effort":v}});
                        if modern {
                            value["thinking"]["display"] = json!("summarized");
                        }
                        value
                    })
                } else {
                    BTreeMap::from([
                        (
                            "high".into(),
                            json!({"thinking":{"type":"enabled","budget_tokens":16000}}),
                        ),
                        (
                            "max".into(),
                            json!({"thinking":{"type":"enabled","budget_tokens":31999}}),
                        ),
                    ])
                }
            } else if gemini25(&id) || gemini25(&api) {
                google_variants(&api)
            } else {
                let o_series = id.split(|c: char| !c.is_ascii_alphanumeric()).any(|part| {
                    part.as_bytes().first() == Some(&b'o')
                        && part.as_bytes().get(1).is_some_and(u8::is_ascii_digit)
                });
                efforts(
                    &if id.contains("gpt") || o_series {
                        openai_efforts(&id, release_date, false)
                    } else {
                        COMMON.to_vec()
                    },
                    |v| json!({"reasoning_effort":v}),
                )
            };
            values
                .into_iter()
                .map(|(name, value)| (name, json!({"modelParams":value})))
                .collect()
        }
        _ => BTreeMap::new(),
    }
}

/// Sort variant names from weakest to strongest; custom names follow built-ins.
pub fn ordered_names(variants: &BTreeMap<String, Value>) -> Vec<String> {
    let rank = |name: &str| match name {
        "none" => 0,
        "minimal" => 1,
        "low" => 2,
        "medium" => 3,
        "high" => 4,
        "xhigh" => 5,
        "ultra" => 6,
        "max" => 7,
        "thinking" => 8,
        _ => 9,
    };
    let mut names: Vec<_> = variants.keys().cloned().collect();
    names.sort_by(|a, b| rank(a).cmp(&rank(b)).then_with(|| a.cmp(b)));
    names
}

fn merge(target: &mut Value, patch: &Value) {
    if let (Some(target), Some(patch)) = (target.as_object_mut(), patch.as_object()) {
        for (key, value) in patch {
            if let Some(existing) = target.get_mut(key) {
                merge(existing, value);
            } else {
                target.insert(key.clone(), value.clone());
            }
        }
    } else {
        *target = patch.clone();
    }
}

/// Apply configured variants like OpenCode: merge overrides and remove disabled
/// entries. The UI-only `disabled` flag is never forwarded to a provider.
pub fn apply_overrides(base: &mut BTreeMap<String, Value>, overrides: &Value) {
    let Some(overrides) = overrides.as_object() else {
        return;
    };
    for (name, options) in overrides {
        if options.get("disabled").and_then(Value::as_bool) == Some(true) {
            base.remove(name);
            continue;
        }
        let mut options = options.clone();
        if let Some(object) = options.as_object_mut() {
            object.remove("disabled");
        }
        merge(
            base.entry(name.clone()).or_insert_with(|| json!({})),
            &options,
        );
    }
}

/// Normalize SDK options into a native request-body patch. Unknown options pass
/// through unchanged, preserving custom compatible-provider extensions.
pub fn wire_options(npm: &str, options: &Value, responses: bool) -> Value {
    let Some(options) = options.as_object() else {
        return json!({});
    };
    let mut result = Map::new();
    let anthropic = matches!(npm, "@ai-sdk/anthropic" | "@ai-sdk/google-vertex/anthropic");
    let google = matches!(npm, "@ai-sdk/google" | "@ai-sdk/google-vertex");
    for (key, value) in options {
        match key.as_str() {
            "disabled" | "sdkSettings" => {}
            "reasoningEffort" if responses => {
                result.entry("reasoning").or_insert_with(|| json!({}))["effort"] = value.clone()
            }
            "reasoningEffort" => {
                result.insert("reasoning_effort".into(), value.clone());
            }
            "reasoningSummary" if responses => {
                result.entry("reasoning").or_insert_with(|| json!({}))["summary"] = value.clone()
            }
            "reasoningMode" if responses => {
                result.entry("reasoning").or_insert_with(|| json!({}))["mode"] = value.clone()
            }
            "reasoningMode" if !responses => {}
            "reasoningSummary" | "include" if !responses => {}
            "effort" if anthropic => {
                result.entry("output_config").or_insert_with(|| json!({}))["effort"] = value.clone()
            }
            "thinking" if anthropic => {
                let mut thinking = value.clone();
                if let Some(object) = thinking.as_object_mut() {
                    if let Some(budget) = object.remove("budgetTokens") {
                        object.insert("budget_tokens".into(), budget);
                    }
                    if let Some(binding) = object.remove("blockBinding") {
                        object.insert("block_binding".into(), binding);
                    }
                }
                result.insert(key.clone(), thinking);
            }
            "thinkingConfig" | "thinkingLevel" | "thinkingBudget" | "includeThoughts" if google => {
                let config = result
                    .entry("generationConfig")
                    .or_insert_with(|| json!({}));
                if key == "thinkingConfig" {
                    merge(&mut config["thinkingConfig"], value);
                } else {
                    if !config["thinkingConfig"].is_object() {
                        config["thinkingConfig"] = json!({});
                    }
                    config["thinkingConfig"][key] = value.clone();
                }
            }
            "textVerbosity" if responses => {
                result.entry("text").or_insert_with(|| json!({}))["verbosity"] = value.clone()
            }
            "textVerbosity" => {
                result.insert("verbosity".into(), value.clone());
            }
            "promptCacheKey" => {
                result.insert("prompt_cache_key".into(), value.clone());
            }
            "serviceTier" => {
                result.insert("service_tier".into(), value.clone());
            }
            "parallelToolCalls" => {
                result.insert("parallel_tool_calls".into(), value.clone());
            }
            "promptCacheRetention" => {
                result.insert("prompt_cache_retention".into(), value.clone());
            }
            "safetyIdentifier" => {
                result.insert("safety_identifier".into(), value.clone());
            }
            "previousResponseId" if responses => {
                result.insert("previous_response_id".into(), value.clone());
            }
            "previousResponseId" if !responses => {}
            "maxCompletionTokens" if !responses => {
                result.insert("max_completion_tokens".into(), value.clone());
            }
            "maxCompletionTokens" if responses => {}
            _ => {
                result.insert(key.clone(), value.clone());
            }
        }
    }
    Value::Object(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(npm: &str, id: &str, date: &str) -> Vec<String> {
        ordered_names(&variants("test", npm, id, id, true, date, 32_000))
    }

    #[test]
    fn openai_model_specific_supported_efforts() {
        assert_eq!(
            names("@ai-sdk/openai", "gpt-5", "2025-08-07"),
            ["minimal", "low", "medium", "high"]
        );
        assert_eq!(
            names("@ai-sdk/openai", "gpt-5.1", "2025-11-13"),
            ["none", "low", "medium", "high"]
        );
        assert_eq!(
            names("@ai-sdk/openai", "gpt-5.3-codex", "2026-02-05"),
            ["none", "low", "medium", "high", "xhigh"]
        );
        assert_eq!(
            names("@ai-sdk/openai", "gpt-5.2-pro", "2025-12-11"),
            ["medium", "high", "xhigh"]
        );
        assert_eq!(names("@ai-sdk/openai", "gpt-5-pro", "2025-10-06"), ["high"]);
        assert!(names("@ai-sdk/openai", "gpt-5-chat", "2025-08-07").is_empty());
        assert_eq!(
            names("@ai-sdk/openai", "o3-deep-research", "2025-06-26"),
            ["medium"]
        );
    }

    #[test]
    fn native_thinking_budgets_and_adaptive_controls() {
        let claude = variants(
            "anthropic",
            "@ai-sdk/anthropic",
            "claude-sonnet-4-5",
            "claude-sonnet-4-5",
            true,
            "",
            8_192,
        );
        assert_eq!(claude["high"]["thinking"]["budgetTokens"], 4_095);
        assert_eq!(claude["max"]["thinking"]["budgetTokens"], 8_191);
        let adaptive = variants(
            "anthropic",
            "@ai-sdk/anthropic",
            "claude-opus-4.7",
            "claude-opus-4.7",
            true,
            "",
            32_000,
        );
        assert_eq!(
            ordered_names(&adaptive),
            ["low", "medium", "high", "xhigh", "max"]
        );
        assert_eq!(adaptive["max"]["thinking"]["display"], "summarized");
        let gemini = variants(
            "google",
            "@ai-sdk/google",
            "gemini-2.5-pro",
            "gemini-2.5-pro",
            true,
            "",
            32_000,
        );
        assert_eq!(gemini["max"]["thinkingConfig"]["thinkingBudget"], 32_768);
        assert_eq!(
            names("@ai-sdk/google", "gemini-3-flash", ""),
            ["minimal", "low", "medium", "high"]
        );
    }

    #[test]
    fn unavailable_controls_are_not_invented() {
        assert!(variants(
            "openai",
            "@ai-sdk/openai",
            "gpt-4.1",
            "gpt-4.1",
            false,
            "",
            32_000
        )
        .is_empty());
        assert!(names("@ai-sdk/openai-compatible", "deepseek-reasoner", "").is_empty());
        assert!(names("@ai-sdk/openai-compatible", "qwen3", "").is_empty());
        assert_eq!(names("@ai-sdk/xai", "grok-3-mini", ""), ["low", "high"]);
        assert!(names("@ai-sdk/mistral", "mistral-large", "").is_empty());
    }

    #[test]
    fn wire_options_preserve_protocol_shapes_and_custom_fields() {
        let options = json!({"reasoningEffort":"xhigh","reasoningSummary":"auto","include":["reasoning.encrypted_content"],"custom":true,"sdkSettings":{"region":"us-east-1","apiKey":"factory-only-secret"}});
        assert_eq!(
            wire_options("@ai-sdk/openai", &options, true),
            json!({"reasoning":{"effort":"xhigh","summary":"auto"},"include":["reasoning.encrypted_content"],"custom":true})
        );
        assert_eq!(
            wire_options("@ai-sdk/openai", &options, false),
            json!({"reasoning_effort":"xhigh","custom":true})
        );
        assert_eq!(
            wire_options(
                "@ai-sdk/anthropic",
                &json!({"thinking":{"type":"enabled","budgetTokens":16000},"effort":"high"}),
                false
            ),
            json!({"thinking":{"type":"enabled","budget_tokens":16000},"output_config":{"effort":"high"}})
        );
        assert_eq!(
            wire_options(
                "@ai-sdk/google",
                &json!({"thinkingConfig":{"includeThoughts":true,"thinkingBudget":16000}}),
                false
            ),
            json!({"generationConfig":{"thinkingConfig":{"includeThoughts":true,"thinkingBudget":16000}}})
        );
        assert_eq!(
            wire_options(
                "@ai-sdk/openai",
                &json!({"reasoningMode":"pro","reasoningEffort":"high","parallelToolCalls":true,"serviceTier":"priority","promptCacheRetention":"24h","safetyIdentifier":"user-a","previousResponseId":"resp-a"}),
                true
            ),
            json!({"reasoning":{"mode":"pro","effort":"high"},"parallel_tool_calls":true,"service_tier":"priority","prompt_cache_retention":"24h","safety_identifier":"user-a","previous_response_id":"resp-a"})
        );
    }

    #[test]
    fn custom_overrides_merge_and_strip_disabled() {
        let mut base = BTreeMap::from([(
            "high".into(),
            json!({"thinking":{"type":"enabled","budgetTokens":16000}}),
        )]);
        apply_overrides(
            &mut base,
            &json!({"high":{"thinking":{"budgetTokens":20000}},"fast":{"reasoningEffort":"low","disabled":false},"max":{"disabled":true}}),
        );
        assert_eq!(
            base["high"],
            json!({"thinking":{"type":"enabled","budgetTokens":20000}})
        );
        assert_eq!(base["fast"], json!({"reasoningEffort":"low"}));
        assert!(!base.contains_key("max"));
    }
}
