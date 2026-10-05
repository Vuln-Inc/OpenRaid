import type { CSSProperties } from "react";

export function desktopPalette(theme: { id: string; dark: boolean; palette?: Record<string, string> }) {
    if (!["openraid", "dark", "light"].includes(theme.id)) return theme.palette ?? {};
    return theme.dark
        ? { background: "#161616", surface: "#202020", text: "#d4d4d4", muted: "#909090", accent: "#ffffff", border: "#ffffff1a" }
        : { background: "#f0f0f0", surface: "#ffffff", text: "#171717", muted: "#737373", accent: "#000000", border: "#0d0d0d1a" };
}

export function appearanceStyle(colors: Record<string, string>): CSSProperties {
    return Object.fromEntries(Object.entries({ background: "--background", surface: "--panel", text: "--text", muted: "--muted", accent: "--accent", border: "--line" }).flatMap(([key, css]) => colors[key] ? [[css, colors[key]]] : []));
}
