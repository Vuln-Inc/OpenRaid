import { useEffect, useRef, useState, type ReactNode } from "react";
import { visibleRange } from "./virtual";
import { keyboardRow } from "./virtual-keyboard";

export function VirtualList<T>({ items, rowHeight, render, label, getKey, emptyState }: {
  items: readonly T[];
  rowHeight: number;
  render: (item: T, index: number) => ReactNode;
  label: string;
  getKey: (item: T, index: number) => string | number;
  emptyState?: ReactNode;
}) {
  const viewport = useRef<HTMLDivElement>(null);
  const [height, setHeight] = useState(400);
  const [top, setTop] = useState(0);
  const [focusRow, setFocusRow] = useState<number | null>(null);
  useEffect(() => {
    const node = viewport.current;
    if (!node) return;
    const observer = new ResizeObserver(() => setHeight(node.clientHeight));
    observer.observe(node);
    return () => observer.disconnect();
  }, []);
  useEffect(() => {
    const node = viewport.current;
    if (node && node.scrollTop > Math.max(0, items.length * rowHeight - height)) {
      node.scrollTop = Math.max(0, items.length * rowHeight - height);
      setTop(node.scrollTop);
    }
  }, [items.length, rowHeight, height]);
  useEffect(() => {
    if (focusRow === null) return;
    const row = viewport.current?.querySelector<HTMLElement>(`[data-virtual-row="${focusRow}"]`);
    const target = row?.querySelector<HTMLElement>('button:not(:disabled), a[href], input:not(:disabled), select:not(:disabled), textarea:not(:disabled), [tabindex="0"]') ?? row;
    target?.focus({ preventScroll: true });
    setFocusRow(null);
  }, [focusRow, top]);
  const range = visibleRange(items.length, top, height, rowHeight);
  return <div ref={viewport} className={`virtual-list${items.length === 0 ? " virtual-list-empty" : ""}`} role="region" aria-label={label} aria-keyshortcuts="ArrowDown ArrowUp Home End PageDown PageUp" tabIndex={0}
    onScroll={event => setTop(event.currentTarget.scrollTop)}
    onKeyDown={event => {
      const element = event.target as HTMLElement;
      // Preserve native text editing and selection controls inside list rows.
      if (element.matches("input, textarea, select, [contenteditable='true']") || event.altKey || event.ctrlKey || event.metaKey || event.shiftKey) return;
      const currentRow = element.closest<HTMLElement>("[data-virtual-row]");
      const current = currentRow ? Number(currentRow.dataset.virtualRow) : Math.floor(top / rowHeight) - (event.key === "ArrowDown" ? 1 : 0);
      const next = keyboardRow(event.key, current, items.length, height / rowHeight);
      if (next === null) return;
      event.preventDefault();
      const node = event.currentTarget;
      const rowTop = next * rowHeight;
      const newTop = rowTop < node.scrollTop ? rowTop : rowTop + rowHeight > node.scrollTop + height ? Math.max(0, rowTop + rowHeight - height) : node.scrollTop;
      node.scrollTop = newTop;
      setTop(newTop);
      setFocusRow(next);
    }}>
    {items.length > 0 ? <div style={{ height: range.totalHeight, position: "relative" }}>
      {items.slice(range.first, range.end).map((item, offset) => {
        const index = range.first + offset;
        return <div key={getKey(item, index)} data-virtual-row={index} tabIndex={-1} style={{ position: "absolute", top: index * rowHeight, height: rowHeight, width: "100%" }}>{render(item, index)}</div>;
      })}
    </div> : emptyState ?? <p className="empty">Nothing here yet.</p>}
  </div>;
}
