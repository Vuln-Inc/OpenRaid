import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import { VirtualList } from "./VirtualList";

describe("virtualized DOM", () => {
  it("renders fewer than twenty of five hundred roster rows", () => {
    const markup = renderToStaticMarkup(<VirtualList
      items={Array.from({ length: 500 }, (_, id) => ({ id }))}
      rowHeight={84} label="Agent roster" getKey={item => item.id}
      render={item => <button data-agent={item.id}>agent-{item.id}</button>}
    />);
    expect((markup.match(/data-agent=/g) ?? []).length).toBeLessThan(20);
    expect(markup).toContain("42000px");
    expect(markup).toContain('aria-label="Agent roster"');
  });
  it("shows an accessible empty state", () => {
    const markup = renderToStaticMarkup(<VirtualList items={[]} rowHeight={100} label="Board" getKey={() => 0} render={() => null} />);
    expect(markup).toContain("Nothing here yet.");
    expect(markup).toContain("virtual-list-empty");
  });
  it("uses the view-specific empty state without a duplicate fallback", () => {
    const markup = renderToStaticMarkup(<VirtualList items={[]} rowHeight={100} label="Board" getKey={() => 0} render={() => null} emptyState={<h2>A shared space for progress</h2>} />);
    expect(markup).toContain("A shared space for progress");
    expect(markup).not.toContain("Nothing here yet.");
    expect(markup).not.toContain("height:0");
  });
  it("hides the empty state when rows are available", () => {
    const markup = renderToStaticMarkup(<VirtualList items={["Update"]} rowHeight={100} label="Board" getKey={item => item} render={item => <p>{item}</p>} emptyState={<h2>A shared space for progress</h2>} />);
    expect(markup).toContain("Update");
    expect(markup).not.toContain("A shared space for progress");
    expect(markup).not.toContain("virtual-list-empty");
  });
  it("provides a keyboard-focusable named viewport without rendering offscreen activity", () => {
    const markup = renderToStaticMarkup(<VirtualList
      items={Array.from({ length: 500 }, (_, id) => ({ id }))}
      rowHeight={100} label="Agent activity" getKey={item => item.id}
      render={item => <span>activity-{item.id}-end</span>}
    />);
    expect(markup).toContain('role="region"');
    expect(markup).toContain('tabindex="0"');
    expect(markup).toContain('aria-label="Agent activity"');
    expect(markup).toContain("activity-0-end");
    expect(markup).not.toContain("activity-499-end");
  });
  it("escapes long and hostile activity text rather than treating it as HTML", () => {
    const content = '<script>alert("secret")</script>' + "界".repeat(1000);
    const markup = renderToStaticMarkup(<VirtualList
      items={[content]} rowHeight={100} label="Board" getKey={() => "message"}
      render={item => <span>{item}</span>}
    />);
    expect(markup).not.toContain("<script>");
    expect(markup).toContain("&lt;script&gt;");
    expect(markup).toContain("界".repeat(1000));
  });
});
