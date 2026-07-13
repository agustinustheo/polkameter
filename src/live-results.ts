export interface LiveSample {
  label: string;
  success: boolean;
  elapsedMs: number;
  responseCode: string;
  receivedAt: number;
}

export function appendLiveSample(samples: LiveSample[], sample: LiveSample, limit = 240): LiveSample[] {
  return [...samples, sample].slice(-limit);
}

export function liveMetrics(samples: LiveSample[]): { throughput: number; p95: number; failures: number } {
  if (samples.length === 0) return { throughput: 0, p95: 0, failures: 0 };
  const elapsed = Math.max(1, samples.at(-1)!.receivedAt - samples[0].receivedAt);
  const latencies = samples.map((sample) => sample.elapsedMs).sort((left, right) => left - right);
  return {
    throughput: samples.length / (elapsed / 1000),
    p95: latencies[Math.floor((latencies.length - 1) * 0.95)],
    failures: samples.filter((sample) => !sample.success).length
  };
}
