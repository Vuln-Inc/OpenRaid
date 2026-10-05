use openraid::variants::ordered_names;
use serde_json::Value;
use std::collections::BTreeMap;

fn choices(names: &[&str]) -> BTreeMap<String, Value> {
    names
        .iter()
        .map(|name| ((*name).to_owned(), Value::Null))
        .collect()
}

#[test]
fn reasoning_levels_are_ordered_by_strength_instead_of_alphabetically() {
    let variants = choices(&["high", "low", "max", "medium", "ultra", "xhigh"]);
    assert_eq!(
        ordered_names(&variants),
        ["low", "medium", "high", "xhigh", "ultra", "max"]
    );
}

#[test]
fn optional_and_custom_variants_keep_deterministic_order_without_changing_ids() {
    let variants = choices(&[
        "z-custom", "minimal", "thinking", "none", "high", "a-custom",
    ]);
    assert_eq!(
        ordered_names(&variants),
        ["none", "minimal", "high", "thinking", "a-custom", "z-custom"]
    );
    assert_eq!(variants.len(), 6);
    assert!(ordered_names(&choices(&[])).is_empty());
}
