export function visibleRange(count: number, scrollTop: number, height: number, rowHeight: number, overscan = 5) {
  const first = Math.max(0, Math.floor(scrollTop / rowHeight) - overscan);
  const end = Math.min(count, Math.ceil((scrollTop + height) / rowHeight) + overscan);
  return { first: Math.min(first, count), end, totalHeight: count * rowHeight };
}
