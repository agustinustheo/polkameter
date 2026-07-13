export type ArrivalModel =
  | { kind: "burst"; windowMs: number }
  | { kind: "ramp"; durationMs: number }
  | { kind: "poisson"; ratePerSecond: number };

export interface Scenario {
  name: string;
	endpoint: string;
	prometheusEndpoint: string;
  pallet: string;
  call: string;
  argumentsJson: string;
	signerProfile: string;
  signerSource: string;
	  fundDerivedUsers: boolean;
	  fundingAmount: string;
	  fundingFinalityTimeoutMs: number;
	  fundingBatchSize: number;
	virtualUsers: number;
	concurrency: number;
	iterations: number;
  arrival: ArrivalModel;
  completion: "submitted" | "in_block" | "finalized";
  mortalityPeriod: number;
  finalityTimeoutMs: number;
  maxElapsedMs: number;
  wholeRunTimeoutMs: number;
  shutdownDrainTimeoutMs: number;
	maxConcurrentSamples: number;
}

export interface ValidationIssue {
  field: string;
  message: string;
}

export interface ScenarioValidation {
  valid: boolean;
  issues: ValidationIssue[];
  estimatedSamples: number;
}

export interface SchedulePreview {
  offsetsMs: number[];
  durationMs: number;
  batchSize: number;
}

export interface NativeScenarioDocument {
  version: number;
  testPlan: {
    name: string;
    description: string;
    seed: number;
	  limits: { wholeRunTimeoutMs: number; shutdownDrainTimeoutMs: number; maxConcurrentSamples: number };
  };
	chain: { endpoint: string; prometheusEndpoint?: string; transactionProfile: "polkadot" };
	  signerSource: {
	    profile: string;
	    derivationPath: string;
	    funding?: { amount: string; finalityTimeoutMs: number; batchSize: number };
	  };
  threadGroups: Array<{
    name: string;
	    users: number;
	    concurrency: number;
	    iterations: number;
    arrival: ArrivalModel;
    samplers: Array<{
      phase: "setup" | "transaction" | "teardown";
      label: string;
      pallet: string;
      call: string;
      arguments: unknown;
      completion: Scenario["completion"];
      mortalityPeriod: number;
      finalityTimeoutMs: number;
      assertions: Array<{ kind: "success" } | { kind: "max_elapsed"; milliseconds: number }>;
    }>;
  }>;
  collectors: Collector[];
}

export type Collector = "jtl" | "events_jsonl" | "telemetry_jsonl" | "summary" | "svg_plots";

export interface PreflightReport {
	  runId: string;
	  signerDerivationRoot: string;
	  endpoint: string;
  genesisHash: string;
  specVersion: number;
  transactionVersion: number;
  metadataHash: string;
  pallets: Array<{ name: string; calls: string[] }>;
  selectedCalls: Array<{ pallet: string; call: string; encodable: boolean; error?: string }>;
  derivedAccounts: Array<{ index: number; address: string }>;
  readiness: { signerSource: string; balanceAndNonce: string; transactionProfile: string };
  resolvedSampleCount: number;
}

export interface RunStatus {
  state: string;
  runId?: string;
  artifactDir?: string;
  completedSamples: number;
  successfulSamples: number;
  failedSamples: number;
  timedOutSamples: number;
  message?: string;
}

export interface RemoteRunnerTarget {
  endpoint: string;
  bearerToken: string;
}

export interface JmxImportReport {
  threadGroups: Array<{
    name: string;
    users: number;
    rampSeconds: number;
    loops?: number;
  }>;
  collectors: string[];
  diagnostics: string[];
}

export interface DashboardReport {
  summary: string;
  plots: Array<{ name: string; svg: string }>;
}

export interface SampleBatch {
  label: string;
  success: boolean;
  elapsedMs: number;
  responseCode: string;
  completedSamples: number;
}
