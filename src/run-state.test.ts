import { describe, expect, it } from "vitest";
import { decideRunIntent, preflightView } from "./run-state";
import type { PreflightReport } from "./types";

const ready: PreflightReport = {
  runId: "run-1", signerDerivationRoot: "//run-run-1",
  endpoint: "ws://127.0.0.1:9944", genesisHash: "0x1", specVersion: 1, transactionVersion: 1,
  metadataHash: "0x2", pallets: [], selectedCalls: [{ pallet: "Balances", call: "transfer_keep_alive", encodable: true }],
  derivedAccounts: [], readiness: { signerSource: "memory", balanceAndNonce: "pre-arm", transactionProfile: "Polkadot" }, resolvedSampleCount: 1
};

describe("run state", () => {
  it("blocks arming until a metadata-valid preflight exists and flips running runs to stop", () => {
    expect(decideRunIntent("draft", undefined)).toBe("blocked");
    expect(decideRunIntent("draft", ready)).toBe("arm");
    expect(decideRunIntent("running", ready)).toBe("stop");
  });

  it("keeps live preflight failures visible over structural success", () => {
    expect(preflightView(true, undefined, "connection refused")).toBe("live_error");
    expect(preflightView(false, undefined, undefined)).toBe("structural_error");
    expect(preflightView(true, ready, undefined)).toBe("live_ready");
  });
});
