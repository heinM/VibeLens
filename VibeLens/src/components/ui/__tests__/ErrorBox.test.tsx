import { isValidElement, type ReactElement, type ReactNode } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it, vi } from "vitest";
import { ErrorBox } from "..";

/** Depth-first search of a rendered element tree for the first element of this type. */
function find(node: ReactNode, type: string): ReactElement<{ onClick?: () => void }> | null {
  if (!isValidElement(node)) return null;
  const el = node as ReactElement<{ children?: ReactNode; onClick?: () => void }>;
  if (el.type === type) return el;
  const kids = el.props.children;
  for (const k of Array.isArray(kids) ? kids : [kids]) {
    const hit = find(k, type);
    if (hit) return hit;
  }
  return null;
}

describe("ErrorBox", () => {
  const error = { code: "database", message: "relation \"t\" does not exist" };

  it("renders nothing without an error", () => {
    expect(renderToStaticMarkup(<ErrorBox error={null} onDismiss={() => undefined} />)).toBe("");
  });

  it("has no dismiss control unless onDismiss is given", () => {
    const html = renderToStaticMarkup(<ErrorBox error={error} />);
    expect(html).toContain("does not exist");
    expect(html).toContain("[database]");
    expect(html).not.toContain("<button");
    expect(html).not.toContain("Dismiss");
  });

  it("renders a labelled dismiss button that calls onDismiss", () => {
    const onDismiss = vi.fn();
    const html = renderToStaticMarkup(<ErrorBox error={error} onDismiss={onDismiss} />);
    expect(html).toContain('aria-label="Dismiss"');
    expect(html).toContain("does not exist");
    const button = find(ErrorBox({ error, onDismiss }), "button");
    expect(button).not.toBeNull();
    button?.props.onClick?.();
    expect(onDismiss).toHaveBeenCalledTimes(1);
  });
});
