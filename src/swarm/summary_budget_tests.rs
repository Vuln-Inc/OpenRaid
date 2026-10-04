use super::*;

#[test]
fn long_context_compaction_preserves_detailed_summary_and_recent_history() -> Result<()> {
    let config = Config {
        context_budget: 128_000,
        max_output_tokens: 16_000,
        ..Config::default()
    };
    let budget = context_config(&config);
    assert_eq!(budget.summary_max_tokens, 14_000);
    let mut context = ContextWindow::new(Arc::from("immutable architecture constraints"), budget);
    for turn in 0..12 {
        context.push(json!({
            "role": "user",
            "content": format!("completed architecture decision {turn}: {}", "x".repeat(24_000)),
        }));
    }
    context.push(json!({"role":"assistant","content":"recent verified test results"}));
    context.push(json!({"role":"user","content":"next action: inspect parser"}));
    let original = context.messages();
    assert!(context.estimated_tokens() > 80_000);
    let plan = context
        .compaction_plan()
        .expect("long history needs compaction");
    assert!(plan.prompt.contains("within 14000 tokens"));

    // A detailed continuation larger than the old 2048-token ceiling remains
    // usable, while the immutable system and the entire recent suffix survive.
    let summary = format!(
        "architecture, changed paths, evidence, board cursor: {}",
        "s".repeat(9_000)
    );
    context.apply_summary(&plan, &summary)?;
    let compacted = context.messages();
    assert_eq!(compacted[0], original[0]);
    assert!(compacted[1]["content"].as_str().unwrap().contains(&summary));
    let suffix_len = compacted.len() - 2;
    assert_eq!(&compacted[2..], &original[original.len() - suffix_len..]);
    assert!(context.estimated_tokens() < context.available_tokens() * 3 / 4);
    assert_eq!(context.compactions(), 1);
    Ok(())
}

#[test]
fn summary_budget_scales_on_model_selection_and_respects_output_capacity() {
    let mut config = Config {
        context_budget: 100_000,
        max_output_tokens: 16_000,
        ..Config::default()
    };
    assert_eq!(context_config(&config).summary_max_tokens, 10_500);
    let mut context = ContextWindow::new(Arc::from("immutable system"), context_config(&config));
    for _ in 0..25 {
        context.push(json!({"role":"user","content":"x".repeat(24_000)}));
    }
    assert!(context
        .compaction_plan()
        .unwrap()
        .prompt
        .contains("within 10500 tokens"));
    config.context_budget = 300_000;
    assert_eq!(context_config(&config).summary_max_tokens, 16_000);
    config.max_output_tokens = 64_000;
    assert_eq!(context_config(&config).summary_max_tokens, 29_500);
    context.select_model(1, context_config(&config));
    assert!(context
        .compaction_plan()
        .unwrap()
        .prompt
        .contains("within 29500 tokens"));

    config.context_budget = 32_000;
    config.max_output_tokens = 4_096;
    assert_eq!(context_config(&config).summary_max_tokens, 3_488);
    context.select_model(2, context_config(&config));
    assert!(context
        .compaction_plan()
        .unwrap()
        .prompt
        .contains("within 3488 tokens"));
    config.context_budget = 300_000;
    config.max_output_tokens = 1_024;
    assert_eq!(context_config(&config).summary_max_tokens, 1_024);
    config.context_budget = 1_024;
    config.max_output_tokens = 1_000;
    assert_eq!(context_config(&config).summary_max_tokens, 64);
    config.max_output_tokens = 32;
    assert_eq!(context_config(&config).summary_max_tokens, 32);
}
