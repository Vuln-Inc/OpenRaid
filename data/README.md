# Offline provider catalog

`models.json` is the complete provider/model catalog used for offline selection,
downloaded from <https://models.dev/api.json> on **2026-10-03**. OpenCode uses the
same source. This snapshot contains **226 providers and 8,385 models**; OpenRaid
adds the custom `codex-lb` provider at load time.
OpenCode's **105 experimental mode aliases** are expanded into separate model
choices as well, retaining the original API model ID and mode-specific options,
headers and pricing. Aliases already present as explicit catalog entries are
deduplicated by ID, matching OpenCode's model map. This snapshot yields **8,481
upstream model choices**. Codex LB models are loaded from the configured server's
live model API rather than added to this offline snapshot.

Refresh it with:

```sh
python scripts/update-catalog.py
```

The refresh script retains every provider and model ID, including each model's
adapter override, capabilities, limits, pricing, modalities and release date.
The snapshot is bundled into the Rust executable, so listing and selecting
providers does not require a network connection or a Python installation.

Live custom-endpoint discovery replaces the previous list only after a
valid, nonempty response. Codex LB's `/v1/models` metadata and native
`/backend-api/codex/models` shape are both supported, including the server's
advertised thinking levels and context/output budgets.
