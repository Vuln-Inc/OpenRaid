import { useEffect, useId, useState, type CSSProperties } from "react";
import { native } from "./bridge";
import "./theme-picker.css";
import { Button } from "./ui";
import { Input } from "./components/base/input/input";
import { Badge } from "./components/base/badges/badges";
import { desktopPalette } from "./appearance";

interface ThemeChoice {
  id: string;
  name?: string;
  dark: boolean;
  palette?: Record<string, string>;
}
interface ThemeCatalog { active: string; themes: ThemeChoice[] }

export function themeName(theme: ThemeChoice): string {
  return theme.name || theme.id.replace(/[-_]/g, " ").replace(/\b\w/g, letter => letter.toUpperCase());
}

export function filterThemes(themes: ThemeChoice[], search: string): ThemeChoice[] {
  const query = search.trim().toLocaleLowerCase();
  return themes.filter(theme => `${themeName(theme)} ${theme.id} ${theme.dark ? "dark" : "light"}`.toLocaleLowerCase().includes(query));
}

/** Choices come from the Rust catalog; previews use the desktop appearance mapping. */
export function ThemePicker({ busy = false, onSelect }: { busy?: boolean; onSelect: (name: string) => Promise<void> }) {
  const [catalog, setCatalog] = useState<ThemeCatalog | null>(null);
  const [search, setSearch] = useState("");
  const [loading, setLoading] = useState(true);
  const [changing, setChanging] = useState("");
  const [error, setError] = useState("");
  const [refresh, setRefresh] = useState(0);
  const id = useId();

  useEffect(() => {
    let disposed = false;
    setLoading(true);
    setError("");
    native.query("themes").then(value => {
      if (!disposed) setCatalog(value as ThemeCatalog);
    }).catch(reason => {
      if (!disposed) setError(`Could not load themes: ${String(reason)}`);
    }).finally(() => { if (!disposed) setLoading(false); });
    return () => { disposed = true; };
  }, [refresh]);

  async function choose(theme: ThemeChoice) {
    if (busy || changing || theme.id === catalog?.active) return;
    setChanging(theme.id);
    setError("");
    try {
      await onSelect(theme.id);
      // Read the persisted preference, so unsuccessful native controls never look selected.
      const value = await native.query("themes") as ThemeCatalog;
      setCatalog(value);
      if (value.active !== theme.id) setError(`The theme was not changed to ${themeName(theme)}. Try again.`);
    } catch (reason) { setError(`Could not change theme: ${String(reason)}`); }
    finally { setChanging(""); }
  }

  const choices = filterThemes(catalog?.themes ?? [], search);
  const current = catalog?.themes.find(theme => theme.id === catalog.active);
  return <section className="theme-picker" aria-labelledby={`${id}-heading`} aria-busy={loading || !!changing}>
    <div className="theme-picker-heading"><div><h3 id={`${id}-heading`}>Appearance</h3><p>Choose a theme shared with your terminal.</p></div>
      {current && <Badge color="gray" type="pill-color">{themeName(current)}</Badge>}
    </div>
    <Input className="theme-search" label="Find a theme" type="search" placeholder="Search names, light or dark…" value={search} onChange={setSearch} isDisabled={loading} />
    {loading && <p role="status" className="theme-feedback">Loading available themes…</p>}
    {error && <div className="theme-feedback theme-error" role="alert"><p>{error}</p><Button disabled={!!changing} onClick={() => setRefresh(value => value + 1)}>Retry</Button></div>}
    {!loading && catalog && <>
      <div className="theme-grid" role="group" aria-label="Available themes">
        {choices.map(theme => {
          const palette = desktopPalette(theme);
          const previewStyle = {
            "--preview-background": palette.background || (theme.dark ? "#13151b" : "#f4f5f7"),
            "--preview-surface": palette.surface || (theme.dark ? "#252936" : "#ffffff"),
            "--preview-text": palette.text || (theme.dark ? "#f4f5f7" : "#212633"),
            "--preview-muted": palette.muted || (theme.dark ? "#a4aabe" : "#62697c"),
            "--preview-accent": palette.accent || "#8274ef",
            "--preview-border": palette.border || (theme.dark ? "#41485c" : "#d2d6df"),
          } as CSSProperties;
          const selected = theme.id === catalog.active;
          return <Button key={theme.id} className={`theme-option${selected ? " is-current" : ""}`} aria-pressed={selected} disabled={busy || !!changing} onClick={() => void choose(theme)}>
            <span className="theme-preview" style={previewStyle} aria-hidden="true"><span className="theme-preview-sidebar"><i /><i /><i /></span><span className="theme-preview-content"><span className="theme-preview-title" /><span className="theme-preview-card"><i /><i /><i /></span><span className="theme-preview-action" /></span></span>
            <span className="theme-option-label"><strong>{themeName(theme)}</strong><small>{changing === theme.id ? "Applying…" : selected ? "Current theme ✓" : theme.dark ? "Dark" : "Light"}</small></span>
          </Button>;
        })}
      </div>
      {!choices.length && <p className="theme-feedback" role="status">{catalog.themes.length ? "No themes match your search." : "No themes are available."}</p>}
    </>}
    <span className="theme-picker-status" role="status">{changing ? "Applying theme…" : current ? `${themeName(current)} theme selected.` : ""}</span>
  </section>;
}
