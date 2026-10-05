import type { CSSProperties } from "react";

// One quiet 16px stroke family for desktop furniture, without brand glyphs.
const paths = {
  panel: "M9 3v18M3 3h18v18H3z",
  compose: "M12 5H5a2 2 0 0 0-2 2v12a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2v-7M15 3l6 6M9 15l3-1 9-9-3-3-9 9-1 4z",
  board: "M4 4h16v12H8l-4 4V4zM8 8h8M8 12h5",
  activity: "M3 12h4l3-7 4 14 3-7h4",
  prompts: "M6 3h9l4 4v14H6zM14 3v5h5M9 12h7M9 16h5",
  sessions: "M3 12a9 9 0 1 0 3-6M3 3v5h5M12 7v5l3 2",
  settings: "M12 8a4 4 0 1 0 0 8 4 4 0 0 0 0-8M9 3h6l1 3 3 1 2 5-2 5-3 1-1 3H9l-1-3-3-1-2-5 2-5 3-1 1-3z",
  folder: "M3 6h7l2 2h9v12H3V6z",
  chevron: "m9 5 7 7-7 7",
  down: "m6 9 6 6 6-6",
  arrow: "M12 19V5m-6 6 6-6 6 6",
  pause: "M8 5v14M16 5v14",
  play: "m8 5 11 7-11 7V5z",
  stop: "M6 6h12v12H6z",
  terminal: "m4 6 6 6-6 6M13 18h7",
  check: "m5 12 4 4L19 6",
} as const;

export type ShellIconName = keyof typeof paths;
export function ShellIcon({ name, className = "", style }: { name: ShellIconName; className?: string; style?: CSSProperties }) {
  return <svg className={`shell-icon ${className}`} style={style} width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.6" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true"><path d={paths[name]} /></svg>;
}
