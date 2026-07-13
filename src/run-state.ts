import type { PreflightReport } from "./types";

export type RunIntent = "arm" | "stop" | "blocked";
export type PreflightView = "idle" | "structural_error" | "live_error" | "live_ready" | "structurally_ready";

export function decideRunIntent(state: string, preflight: PreflightReport | undefined): RunIntent {
  if (state === "running" || state === "arming" || state === "stopping") return "stop";
  if (!preflight || preflight.selectedCalls.some((call) => !call.encodable)) return "blocked";
  return "arm";
}

export function preflightView(
  structuralValid: boolean | undefined,
  preflight: PreflightReport | undefined,
  error: string | undefined
): PreflightView {
  if (error) return "live_error";
  if (preflight) return "live_ready";
  if (structuralValid === false) return "structural_error";
  if (structuralValid) return "structurally_ready";
  return "idle";
}
