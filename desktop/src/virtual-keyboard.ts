/** Return the row reached by a navigation key, or null for an unrelated key. */
export function keyboardRow(key: string, current: number, count: number, pageSize: number): number | null {
  if (!count) return null;
  const page = Math.max(1, Math.floor(pageSize));
  const positions: Record<string, number> = {
    ArrowDown: current + 1,
    ArrowUp: current - 1,
    Home: 0,
    End: count - 1,
    PageDown: current + page,
    PageUp: current - page,
  };
  return Object.hasOwn(positions, key) ? Math.max(0, Math.min(count - 1, positions[key])) : null;
}
