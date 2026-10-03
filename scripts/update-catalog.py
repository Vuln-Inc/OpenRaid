#!/usr/bin/env python3
"""Refresh the offline provider/model catalog from OpenCode's models.dev source.

Run from any directory: python scripts/update-catalog.py
Only metadata used by Openraid is retained; all providers and model IDs survive.
"""
import json
from pathlib import Path
from urllib.request import Request, urlopen


def main():
    request = Request("https://models.dev/api.json", headers={"User-Agent": "openraid-catalog/0.1"})
    with urlopen(request, timeout=60) as response:
        source = json.load(response)
    catalog = {}
    provider_fields = ("id", "name", "env", "api", "npm", "doc")
    model_fields = (
        "id", "name", "family", "reasoning", "tool_call", "attachment", "temperature",
        "release_date", "limit", "cost", "provider", "modalities", "status", "interleaved", "experimental",
    )
    for provider_id, provider in sorted(source.items()):
        item = {key: provider[key] for key in provider_fields if key in provider}
        item["id"] = provider_id
        item["models"] = {}
        for model_id, model in sorted(provider.get("models", {}).items()):
            metadata = {key: model[key] for key in model_fields if key in model}
            metadata["id"] = model_id
            item["models"][model_id] = metadata
        catalog[provider_id] = item
    destination = Path(__file__).resolve().parents[1] / "data" / "models.json"
    destination.parent.mkdir(exist_ok=True)
    destination.write_text(json.dumps(catalog, ensure_ascii=False, separators=(",", ":")) + "\n", encoding="utf-8")
    print(f"Saved {len(catalog)} providers and {sum(len(p['models']) for p in catalog.values())} models to {destination}")
    mode_count = sum(len(model.get("experimental", {}).get("modes", {})) for provider in catalog.values() for model in provider["models"].values())
    print(f"Retained {mode_count} experimental model modes for OpenCode-compatible expansion")


if __name__ == "__main__":
    main()
