import { describe, expect, it } from "vitest";
import { buildNativeScenario, removeSampler, removeThreadGroup, type EditablePhase, type EditableThreadGroup } from "./scenario-plan";
import type { Scenario } from "./types";

const scenario: Scenario = {
  name: "Burst",
  endpoint: "ws://127.0.0.1:9944",
  pallet: "Balances",
  call: "transfer_keep_alive",
  argumentsJson: '{"value":"1"}',
  signerSource: "//Alice",
  virtualUsers: 4,
  concurrency: 2,
  arrival: { kind: "burst", windowMs: 1 },
  completion: "finalized",
  mortalityPeriod: 4,
  finalityTimeoutMs: 1_000,
  maxElapsedMs: 0,
  wholeRunTimeoutMs: 10_000,
  shutdownDrainTimeoutMs: 1_000
};

describe("scenario plan", () => {
  it("preserves the ordered structural sampler phases", () => {
    const document = buildNativeScenario(scenario, [group("A", ["setup", "transaction", "teardown"])]);
    expect(document.threadGroups[0].samplers.map((sampler) => sampler.phase)).toEqual(["setup", "transaction", "teardown"]);
    expect(document.threadGroups[0].samplers[0].arguments).toEqual({ value: "1" });
    expect(document.threadGroups[0].samplers[1].arguments).toEqual({ remark: { $bytes: "0x01" } });
    expect(document.threadGroups[0].samplers[1].assertions).toContainEqual({ kind: "max_elapsed", milliseconds: 50 });
  });

  it("never removes the last sampler", () => {
    const samplers = group("A", ["setup", "transaction"]).samplers;
    expect(removeSampler([samplers[0]], 0)).toEqual([samplers[0]]);
    expect(removeSampler(samplers, 0)).toEqual([samplers[1]]);
  });

  it("serializes multiple independently scheduled thread groups", () => {
    const document = buildNativeScenario(scenario, [
      group("Signups", ["setup", "transaction"]),
      { ...group("Reports", ["transaction", "teardown"]), virtualUsers: 8, concurrency: 4, arrival: { kind: "ramp", durationMs: 1200 } }
    ]);
    expect(document.threadGroups.map((threadGroup) => threadGroup.name)).toEqual(["Signups", "Reports"]);
    expect(document.threadGroups[1].arrival).toEqual({ kind: "ramp", durationMs: 1200 });
    expect(removeThreadGroup([group("A", ["transaction"]), group("B", ["transaction"])], "a")).toHaveLength(1);
  });
});

function group(name: string, phases: EditablePhase[]): EditableThreadGroup {
  return {
    id: name.toLowerCase(), name, virtualUsers: 4, concurrency: 2, arrival: { kind: "burst", windowMs: 1 },
    samplers: phases.map((phase, index) => ({ id: `${name}-${index}`, phase, label: `${phase}.${name}`, pallet: index === 0 ? "Balances" : "System", call: index === 0 ? "transfer_keep_alive" : "remark", argumentsJson: index === 0 ? '{"value":"1"}' : '{"remark":{"$bytes":"0x01"}}', completion: index === 0 ? "finalized" : "submitted", mortalityPeriod: 4, finalityTimeoutMs: 1_000, maxElapsedMs: index === 1 ? 50 : 0 }))
  };
}
