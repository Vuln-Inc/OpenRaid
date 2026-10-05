import { useEffect, useId, useState } from "react";
import { native } from "./bridge";
import "./model-picker.css";
import { Button, SearchSelect } from "./ui";

export interface CatalogModel { id: string; name: string; variants: string[]; available?: boolean }
export interface CatalogProvider { id: string; name: string; models: CatalogModel[]; configured?: boolean; authentication?: "connected" | "not_required" | "missing"; authHint?: string }
export function modelVariants(model: CatalogModel | undefined): string[] { return model?.variants ?? []; }
export function defaultModel(provider: CatalogProvider | undefined): string { return provider?.models.find(item => item.available !== false)?.id ?? ""; }
export function catalogLabel(item: { id: string; name: string }): string { return item.name === item.id ? item.name : `${item.name} · ${item.id}`; }

interface Props {
  provider: string;
  model: string;
  variant: string | null;
  busy: boolean;
  compact?: boolean;
  onSelect: (provider: string, model: string, variant: string | null) => Promise<void>;
}

export function ModelPicker({ provider, model, variant, busy, compact = false, onSelect }: Props) {
  const headingId = useId();
  const [catalog, setCatalog] = useState<CatalogProvider[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState("");
  const [reload, setReload] = useState(0);
  const [draftProvider, setProvider] = useState(provider);
  const [draftModel, setModel] = useState(model);
  const [draftVariant, setVariant] = useState(variant ?? "");
  const [applying, setApplying] = useState(false);
  useEffect(() => {
    setProvider(provider); setModel(model); setVariant(variant ?? "");
  }, [provider, model, variant]);
  useEffect(() => {
    let cancelled = false;
    setLoading(true); setError("");
    native.query("catalog").then(value => {
      if (!Array.isArray(value)) throw new Error("The model catalog is unavailable. Try refreshing it.");
      if (!cancelled) setCatalog(value as CatalogProvider[]);
    }).catch(reason => { if (!cancelled) setError(String(reason)); })
      .finally(() => { if (!cancelled) setLoading(false); });
    return () => { cancelled = true; };
  }, [reload]);
  const chosenProvider = catalog.find(item => item.id === draftProvider);
  const chosenModel = chosenProvider?.models.find(item => item.id === draftModel);
  const variants = modelVariants(chosenModel);
  const changed = draftProvider !== provider || draftModel !== model || draftVariant !== (variant ?? "");
  const disabled = busy || applying || loading;
  return <section className={`model-picker${compact ? " model-picker-compact" : ""}`} aria-labelledby={headingId} aria-busy={loading || applying}>
    <div className="picker-heading"><div><h2 id={headingId}>Provider & model</h2>{!compact && <p>Choose how your agents reason. Search directly inside each selector.</p>}</div><Button className={compact ? "ghost" : ""} disabled={disabled} onClick={() => setReload(value => value + 1)}>Refresh catalog</Button></div>
    {!compact && <p className="model-current"><span>Current selection</span><strong>{catalog.find(item => item.id === provider)?.name ?? provider} / {catalog.find(item => item.id === provider)?.models.find(item => item.id === model)?.name ?? model}</strong><small>{variant || "Default variant"}</small></p>}
    {loading && <p role="status">Loading available providers and models…</p>}
    {error && <p className="picker-error" role="alert">{error}</p>}
    {!loading && !error && catalog.length === 0 && <p role="status">No providers are available. Check your shared OpenRaid configuration, then refresh.</p>}
    <div className="model-picker-grid">
      <SearchSelect label="Provider" value={draftProvider} disabled={disabled || catalog.length === 0}
        options={[...(!chosenProvider && draftProvider ? [{ id: draftProvider, label: `${draftProvider} · current / unavailable`, disabled: true }] : []), ...catalog.map(item => ({ id: item.id, label: `${catalogLabel(item)}${item.configured === false ? " · setup needed" : ""}` }))]}
         hint={compact ? undefined : `${catalog.length} providers available`} onChange={id => { const next = catalog.find(item => item.id === id); setProvider(id); setModel(defaultModel(next)); setVariant(""); }} />
      <SearchSelect key={draftProvider} label="Model" value={draftModel} disabled={disabled || !chosenProvider || !chosenProvider.models.length}
        options={[...(!chosenModel && draftModel ? [{ id: draftModel, label: `${draftModel} · current / unavailable`, disabled: true }] : []), ...(chosenProvider?.models ?? []).map(item => ({ id: item.id, label: `${catalogLabel(item)}${item.available === false ? " · unavailable" : ""}`, disabled: item.available === false }))]}
         hint={compact ? undefined : chosenProvider ? `${chosenProvider.models.length} models for this provider` : "Choose a provider first."} onChange={id => { setModel(id); setVariant(""); }} />
    </div>
    {chosenProvider && (!compact || chosenProvider.configured === false || chosenProvider.authentication === "missing") && <p className="credential-note"><strong>{chosenProvider.authentication === "missing" ? "Setup needed. " : chosenProvider.authentication === "connected" ? "Credentials available. " : chosenProvider.authentication === "not_required" ? "Provider-managed or public access. " : ""}</strong>{chosenProvider.authHint ?? (chosenProvider.configured === false ? "This provider needs configuration or credentials. Use the shared OpenRaid authentication/configuration flow, then refresh the catalog. Credentials are never displayed here." : "Uses your shared OpenRaid provider configuration and credentials. No secrets are displayed.")}</p>}
    <div className="variant-field"><SearchSelect key={`${draftProvider}/${draftModel}`} label="Reasoning variant" value={draftVariant || "__default__"} disabled={disabled || !chosenModel || variants.length === 0}
      options={[{ id: "__default__", label: "Default · provider preference" }, ...(draftVariant && !variants.includes(draftVariant) ? [{ id: draftVariant, label: `${draftVariant} · current / unavailable`, disabled: true }] : []), ...variants.map(item => ({ id: item, label: item }))]}
       onChange={id => setVariant(id === "__default__" ? "" : id)} hint={compact ? undefined : variants.length ? "Variants are specific to the selected model." : "This model uses its default behavior; no variants are available."} /></div>
    <div className="picker-actions"><Button className="primary" disabled={disabled || !changed || !chosenModel || chosenModel.available === false || chosenProvider?.configured === false} onClick={async () => { setApplying(true); setError(""); try { await onSelect(draftProvider, draftModel, draftVariant || null); } catch (reason) { setError(String(reason)); } finally { setApplying(false); } }}>{applying ? "Applying…" : "Apply model selection"}</Button><span>{chosenProvider?.configured === false ? "Connect or configure this provider before applying." : chosenModel?.available === false ? "Choose a model available with your current credentials." : !chosenModel ? "Choose an available provider and model to continue." : !changed ? "Your current selection is active." : "Review your selection, then apply it."}</span></div>
  </section>;
}
