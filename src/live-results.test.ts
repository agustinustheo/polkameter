import { describe, expect, it } from "vitest";
import { appendLiveSample, liveMetrics } from "./live-results";

describe("live results", () => {
  it("bounds streamed samples and derives current throughput and failures", () => {
    const samples = appendLiveSample([], { label: "transfer", success: true, elapsedMs: 10, responseCode: "OK", receivedAt: 1_000 }, 2);
    const next = appendLiveSample(samples, { label: "transfer", success: false, elapsedMs: 80, responseCode: "TX_ERROR", receivedAt: 2_000 }, 2);
    const bounded = appendLiveSample(next, { label: "transfer", success: true, elapsedMs: 20, responseCode: "OK", receivedAt: 3_000 }, 2);
    expect(bounded).toHaveLength(2);
    expect(liveMetrics(bounded)).toEqual({ throughput: 2, p95: 20, failures: 1 });
  });
});
