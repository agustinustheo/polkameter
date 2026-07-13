import type { Collector, NativeScenarioDocument, Scenario } from "./types";

export type EditablePhase = "setup" | "transaction" | "teardown";

export interface EditableSampler {
  id: string;
  phase: EditablePhase;
  label: string;
  pallet: string;
  call: string;
  argumentsJson: string;
  completion: Scenario["completion"];
  mortalityPeriod: number;
  finalityTimeoutMs: number;
  maxElapsedMs: number;
}

export interface EditableThreadGroup {
  id: string;
  name: string;
  virtualUsers: number;
  concurrency: number;
  arrival: Scenario["arrival"];
  samplers: EditableSampler[];
}

export function buildNativeScenario(scenario: Scenario, groups: EditableThreadGroup[], collectors: Collector[] = ["jtl", "events_jsonl", "telemetry_jsonl", "summary", "svg_plots"]): NativeScenarioDocument {
  return {
    version: 1,
    testPlan: {
      name: scenario.name,
      description: "Created in Polkameter",
      seed: 1,
      limits: { wholeRunTimeoutMs: scenario.wholeRunTimeoutMs, shutdownDrainTimeoutMs: scenario.shutdownDrainTimeoutMs }
    },
    chain: { endpoint: scenario.endpoint, transactionProfile: "polkadot" },
    signerSource: { baseSuri: scenario.signerSource, derivationPath: "" },
    threadGroups: groups.map((group) => ({
      name: group.name,
      users: group.virtualUsers,
      concurrency: group.concurrency,
      arrival: group.arrival,
      samplers: group.samplers.map((sampler) => ({
        phase: sampler.phase,
        label: sampler.label,
        pallet: sampler.pallet,
        call: sampler.call,
        arguments: parseArguments(sampler.argumentsJson),
        completion: sampler.completion,
        mortalityPeriod: sampler.mortalityPeriod,
        finalityTimeoutMs: sampler.finalityTimeoutMs,
        assertions: [{ kind: "success" as const }, ...(sampler.maxElapsedMs > 0 ? [{ kind: "max_elapsed" as const, milliseconds: sampler.maxElapsedMs }] : [])]
      }))
    })),
    collectors
  };
}

export function removeSampler(samplers: EditableSampler[], index: number): EditableSampler[] {
  if (samplers.length <= 1 || index < 0 || index >= samplers.length) return samplers;
  return samplers.filter((_, candidate) => candidate !== index);
}

function parseArguments(argumentsJson: string): unknown {
  try { return JSON.parse(argumentsJson); } catch { return argumentsJson; }
}

export function removeThreadGroup(groups: EditableThreadGroup[], id: string): EditableThreadGroup[] {
  if (groups.length <= 1) return groups;
  return groups.filter((group) => group.id !== id);
}
