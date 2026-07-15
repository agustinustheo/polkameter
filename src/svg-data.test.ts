import { describe, expect, it } from "vitest";
import { svgImageDataUrl } from "./svg-data";

describe("svgImageDataUrl", () => {
  it("encodes report markup as image data instead of inline HTML", () => {
    const svg = '<svg><script>alert("not executed")</script><text>Latency</text></svg>';
    const url = svgImageDataUrl(svg);

    expect(url).toMatch(/^data:image\/svg\+xml;base64,[A-Za-z0-9+/=]+$/);
    expect(url).not.toContain("<script>");
    expect(atob(url.split(",")[1])).toBe(svg);
  });
});
