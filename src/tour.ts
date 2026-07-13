import { driver, type Driver, type DriveStep } from "driver.js";
import "driver.js/dist/driver.css";

const TOUR_SEEN_KEY = "polkameter-tour-seen";

let activeTour: Driver | undefined;

const steps: DriveStep[] = [
  {
    popover: {
      title: "Welcome to Polkameter",
      description: "A JMeter-style workbench for stress-testing Polkadot SDK chains. This tour walks through every control; replay it anytime with the ? button in the top bar."
    }
  },
  {
    element: ".state-pill",
    popover: {
      title: "Scenario state",
      description: "Draft until the backend validates the plan, Ready when it is structurally valid and Needs attention when validation reports issues."
    }
  },
  {
    element: "#reset-button",
    popover: {
      title: "Reset scenario",
      description: "Discards the current plan and restores the default 1000-user transfer burst. Saved files and stored signer profiles are untouched."
    }
  },
  {
    element: "#load-button",
    popover: {
      title: "Load scenario",
      description: "Reopens a saved .polkameter.json plan. Signer secrets are never in the file; the plan only names a profile that stays in the operating-system credential vault."
    }
  },
  {
    element: "#save-button",
    popover: {
      title: "Save scenario",
      description: "Writes the plan as redacted .polkameter.json: test-plan limits, thread groups, samplers and collectors, with the signer reduced to a profile alias."
    }
  },
  {
    element: "#export-jmx-button",
    popover: {
      title: "Export JMX",
      description: "Writes a structural JMeter companion file with thread groups and collectors. The .polkameter.json stays authoritative because JMX carries no pallet, call or SCALE contract."
    }
  },
  {
    element: "#import-jmx-button",
    popover: {
      title: "Inspect JMX",
      description: "Reads a JMeter .jmx and reports its thread groups, collectors and any unsupported sampler types. It never executes non-Substrate samplers."
    }
  },
  {
    element: "#preflight-button",
    popover: {
      title: "Preflight chain",
      description: "Connects to the WebSocket RPC, reads live runtime metadata, SCALE encodes every planned call and derives the exact signer accounts a run would use. Nothing is submitted."
    }
  },
  {
    element: "#run-button",
    popover: {
      title: "Arm and run",
      description: "Runs a preflight first if needed, then submits the plan against the chain. While a run is active this becomes Stop, which halts scheduling and drains active watches within the shutdown deadline."
    }
  },
  {
    element: ".plan-tree",
    popover: {
      title: "Test plan tree",
      description: "The whole plan at a glance: chain connection, thread groups with their user counts, each group's samplers, assertions and collectors. Click any row to jump to its editor section.",
      side: "right"
    }
  },
  {
    element: "[data-thread-group]",
    popover: {
      title: "Thread groups",
      description: "Each group owns its virtual users, concurrency, iterations and arrival model. Groups run concurrently under a shared plan-wide concurrency ceiling, and each gets a disjoint signer range.",
      side: "right"
    }
  },
  {
    element: ".plan-footer",
    popover: {
      title: "Local guarantee",
      description: "Editing and previewing never submit an extrinsic. Transactions only leave this machine after an explicit arm.",
      side: "right"
    }
  },
  {
    element: "#plan-section",
    popover: {
      title: "Plan structure",
      description: "Samplers run in three phases: setup once before load, transaction samplers under the arrival schedule (looped per virtual user) and teardown once after the load drains."
    }
  },
  {
    element: "#connection-section",
    popover: {
      title: "Chain connection",
      description: "The target RPC and the call identity. Pallet, call and arguments depend on the connected chain's runtime, so treat these fields as chain-specific; preflight verifies against live metadata that the call exists and encodes. The remote runner fields are session-only and optional."
    }
  },
  {
    element: "#users-section",
    popover: {
      title: "Virtual users and signers",
      description: "Users derive deterministically from the signer profile and run ID. Every derived account must already exist on chain; the optional funding helper only works against loopback dev chains."
    }
  },
  {
    element: ".boundary-box",
    popover: {
      title: "Completion boundary",
      description: "How long each sample waits: Submitted returns at broadcast, In block when the transaction lands in a block and Finalized when that block finalizes. The deadlines below bound samples, the whole run and the shutdown drain.",
      side: "left"
    }
  },
  {
    element: "#assertions-section",
    popover: {
      title: "Assertions and collectors",
      description: "Every sampler asserts transaction success; a max-elapsed assertion additionally fails samples that exceed a latency ceiling. Collectors choose which artifacts a run writes: JTL samples, event and telemetry logs, a summary and SVG plots."
    }
  },
  {
    element: "#arrival-section",
    popover: {
      title: "Arrival model",
      description: "When virtual users hit the chain: Burst releases everyone inside one window, Ramp spreads arrivals evenly over a duration and Poisson spaces them with deterministic exponential gaps. All schedules are seeded, so a preview matches the real run."
    }
  },
  {
    element: "#preflight-section",
    popover: {
      title: "Preflight result",
      description: "Validation and encoding results from the Rust backend land here: runtime version, metadata hash, per-call encodability and the derived accounts a run would use. A run cannot arm until every call encodes."
    }
  },
  {
    element: ".metric-grid",
    popover: {
      title: "Plan metrics",
      description: "Live totals for the current plan: scheduled samples across all groups, maximum parallel submissions, the target call and the previewed schedule duration.",
      side: "left"
    }
  },
  {
    element: ".execution-path",
    popover: {
      title: "Execution path",
      description: "The run lifecycle. Steps fill in as they complete: Connect after a successful preflight, Prepare while arming, Sample during the run and Collect once results are written.",
      side: "left"
    }
  },
  {
    element: ".monitor-section.note",
    popover: {
      title: "Status",
      description: "The backend's latest word on the run, including why an arm was refused. After a run finishes, an Open run report button appears below with the summary and plots.",
      side: "left"
    }
  },
  {
    popover: {
      title: "That's the workbench",
      description: "Each run writes a portable artifact directory: samples.jtl, events and telemetry logs, summary.md and SVG plots. Replay this tour anytime with the ? button."
    }
  }
];

export function startTour(): void {
  activeTour?.destroy();
  activeTour = driver({
    steps,
    showProgress: true,
    popoverClass: "polkameter-tour",
    nextBtnText: "Next",
    prevBtnText: "Back",
    doneBtnText: "Done",
    onDestroyed: () => {
      activeTour = undefined;
    }
  });
  localStorage.setItem(TOUR_SEEN_KEY, "1");
  activeTour.drive();
}

export function maybeStartTour(): void {
  if (!localStorage.getItem(TOUR_SEEN_KEY)) startTour();
}

export function refreshActiveTour(): void {
  if (activeTour?.isActive()) activeTour.refresh();
}
