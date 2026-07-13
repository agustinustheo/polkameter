import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import {
  Activity,
  Boxes,
  Braces,
  Cable,
  CheckCircle2,
  ChevronDown,
  CircleDot,
  ClipboardList,
  Cpu,
  createIcons,
  Flag,
  FolderOpen,
  Gauge,
  GitBranch,
  Play,
  RotateCcw,
  Save,
  ShieldCheck,
  Square,
  Timer,
  Users,
  Wrench,
  XCircle
} from "lucide";
import type { ArrivalModel, Collector, DashboardReport, NativeScenarioDocument, PreflightReport, RunStatus, SampleBatch, Scenario, ScenarioValidation, SchedulePreview } from "./types";
import { buildNativeScenario, removeSampler, removeThreadGroup, type EditablePhase, type EditableSampler, type EditableThreadGroup } from "./scenario-plan";
import { decideRunIntent, preflightView } from "./run-state";
import { appendLiveSample, liveMetrics, type LiveSample } from "./live-results";
import "./styles.css";

const initialScenario: Scenario = {
  name: "1000 user transfer burst",
  endpoint: "ws://127.0.0.1:9944",
  pallet: "balances",
  call: "transfer_keep_alive",
  argumentsJson: '{\n  "dest": { "$variant": "Id", "value": { "$bytes": "0xd43593c715fdd31c61141abd04a99fd6822c8558854ccde39a5684e7a56da27d" } },\n  "value": "1000000000000"\n}',
  signerSource: "//Alice",
  virtualUsers: 1000,
  concurrency: 1000,
  arrival: { kind: "burst", windowMs: 1000 },
  completion: "finalized",
  mortalityPeriod: 4096,
  finalityTimeoutMs: 300000,
  maxElapsedMs: 0,
  wholeRunTimeoutMs: 900000,
  shutdownDrainTimeoutMs: 300000
};

let scenario: Scenario = structuredClone(initialScenario);
let lastValidation: ScenarioValidation | undefined;
let lastPreview: SchedulePreview | undefined;
let lastPreflight: PreflightReport | undefined;
let runStatus: RunStatus = { state: "draft", completedSamples: 0, successfulSamples: 0, failedSamples: 0, timedOutSamples: 0 };
let threadGroups: EditableThreadGroup[] = [newThreadGroup("group-1", "Virtual users")];
let activeThreadGroupId = threadGroups[0].id;
let activeSamplerIndex = 0;
let runReport: DashboardReport | undefined;
let preflightError: string | undefined;
let preflightRunId: string | undefined;
let collectors: Collector[] = ["jtl", "events_jsonl", "telemetry_jsonl", "summary", "svg_plots"];
let liveSamples: LiveSample[] = [];
let liveRenderQueued = false;

function newThreadGroup(id: string, name: string): EditableThreadGroup {
  return {
    id,
    name,
    virtualUsers: scenario.virtualUsers,
    concurrency: scenario.concurrency,
    arrival: structuredClone(scenario.arrival),
    samplers: [newSampler(`${id}-sampler-1`, "transaction")]
  };
}

function newSampler(id: string, phase: EditablePhase): EditableSampler {
  return { id, phase, label: `${phase}.${scenario.pallet}.${scenario.call}`, pallet: scenario.pallet, call: scenario.call, argumentsJson: scenario.argumentsJson, completion: scenario.completion, mortalityPeriod: scenario.mortalityPeriod, finalityTimeoutMs: scenario.finalityTimeoutMs, maxElapsedMs: scenario.maxElapsedMs };
}

function activeThreadGroup(): EditableThreadGroup {
  return threadGroups.find((group) => group.id === activeThreadGroupId) ?? threadGroups[0];
}

function syncActiveThreadGroup(): void {
  const group = activeThreadGroup();
  group.virtualUsers = scenario.virtualUsers;
  group.concurrency = scenario.concurrency;
  group.arrival = structuredClone(scenario.arrival);
  const sampler = group.samplers[activeSamplerIndex];
  if (sampler) Object.assign(sampler, { pallet: scenario.pallet, call: scenario.call, argumentsJson: scenario.argumentsJson, completion: scenario.completion, mortalityPeriod: scenario.mortalityPeriod, finalityTimeoutMs: scenario.finalityTimeoutMs, maxElapsedMs: scenario.maxElapsedMs, label: `${sampler.phase}.${scenario.pallet}.${scenario.call}` });
}

function selectThreadGroup(id: string): void {
  syncActiveThreadGroup();
  const group = threadGroups.find((candidate) => candidate.id === id);
  if (!group) return;
  activeThreadGroupId = group.id;
  activeSamplerIndex = 0;
  scenario.virtualUsers = group.virtualUsers;
  scenario.concurrency = group.concurrency;
  scenario.arrival = structuredClone(group.arrival);
  loadActiveSampler();
}

function loadActiveSampler(): void {
  const sampler = activeThreadGroup().samplers[activeSamplerIndex];
  if (!sampler) return;
  scenario.pallet = sampler.pallet; scenario.call = sampler.call; scenario.argumentsJson = sampler.argumentsJson; scenario.completion = sampler.completion; scenario.mortalityPeriod = sampler.mortalityPeriod; scenario.finalityTimeoutMs = sampler.finalityTimeoutMs; scenario.maxElapsedMs = sampler.maxElapsedMs;
}

function plannedSamples(): number {
  return threadGroups.reduce((total, group) => total + group.virtualUsers * group.samplers.length, 0);
}

function parallelSends(): number {
  return threadGroups.reduce((total, group) => total + group.concurrency, 0);
}

function markDraftChanged(): void {
  lastPreflight = undefined;
  preflightRunId = undefined;
  preflightError = undefined;
}

function arrivalKind(): ArrivalModel["kind"] {
  return scenario.arrival.kind;
}

function numericValue(value: string): number {
  return Math.max(0, Number.parseInt(value, 10) || 0);
}

function render(): void {
  const previewDuration = lastPreview ? formatDuration(lastPreview.durationMs) : "Not calculated";
  const resultState = lastValidation ? (lastValidation.valid ? "Ready" : "Needs attention") : "Draft";
  const resultClass = lastValidation?.valid ? "success" : lastValidation ? "warning" : "neutral";

  document.querySelector<HTMLDivElement>("#app")!.innerHTML = `
    <main class="shell">
      <header class="topbar" data-tauri-drag-region="deep">
        <div class="product-lockup">
          <div class="product-mark"><i data-lucide="gauge"></i></div>
          <div>
            <div class="product-name">Polkameter</div>
            <div class="product-subtitle">Polkadot SDK load workbench</div>
          </div>
        </div>
        <div class="topbar-actions">
          <span class="state-pill ${resultClass}"><span></span>${resultState}</span>
          <button class="icon-button" id="reset-button" title="Reset scenario"><i data-lucide="rotate-ccw"></i></button>
          <button class="command-button quiet" id="load-button"><i data-lucide="folder-open"></i> Load scenario</button>
          <button class="command-button quiet" id="save-button"><i data-lucide="save"></i> Save scenario</button>
          <button class="command-button quiet" id="preflight-button"><i data-lucide="cable"></i> Preflight chain</button>
          <button class="command-button primary" id="run-button"><i data-lucide="${runStatus.state === "running" ? "square" : "play"}"></i> ${runStatus.state === "running" ? "Stop run" : "Arm and run"}</button>
        </div>
      </header>

      <div class="workspace">
        <aside class="plan-panel">
          <div class="panel-heading"><span>Test plan</span><button class="icon-button" title="Plan options"><i data-lucide="chevron-down"></i></button></div>
          <nav class="plan-tree" aria-label="Test plan">
            <button class="tree-row root active" data-plan-node="plan"><i data-lucide="clipboard-list"></i><span>${escapeHtml(scenario.name)}</span></button>
            <button class="tree-row indent" data-plan-node="connection"><i data-lucide="cable"></i><span>Chain connection</span><em>1</em></button>
            ${threadGroups.map((group) => `<button class="tree-row indent ${group.id === activeThreadGroupId ? "active" : ""}" data-thread-group="${group.id}"><i data-lucide="users"></i><span>${escapeHtml(group.name)}</span><em>${group.virtualUsers}</em></button>${group.samplers.map((sampler, index) => `<button class="tree-row indent-two phase-row ${group.id === activeThreadGroupId && index === activeSamplerIndex ? "active" : ""}" data-sampler-index="${index}"><i data-lucide="${sampler.phase === "setup" ? "wrench" : sampler.phase === "teardown" ? "flag" : "braces"}"></i><span>${escapeHtml(sampler.label)}</span><em>${sampler.phase === "transaction" ? group.concurrency : "1"}</em></button>`).join("")}`).join("")}
            <button class="tree-row indent" data-plan-node="assertions"><i data-lucide="shield-check"></i><span>Assertions</span><em>1</em></button>
            <button class="tree-row indent" data-plan-node="collector"><i data-lucide="activity"></i><span>Collectors</span><em>5</em></button>
          </nav>
          <div class="plan-footer">
            <div><i data-lucide="shield-check"></i> Local scenario only</div>
            <small>Preview never submits an extrinsic.</small>
          </div>
        </aside>

        <section class="editor-panel">
          <div class="section-bar">
            <div><span class="eyebrow">Scenario editor</span><h1>${escapeHtml(scenario.name)}</h1></div>
            <div class="run-boundary"><span>Completion boundary</span><strong>${scenario.completion.replace("_", " ")}</strong></div>
          </div>

          <div class="editor-scroll">
            <section class="form-section">
              <div class="section-title"><i data-lucide="clipboard-list"></i><div><h2>Plan structure</h2><p>Setup runs once, transactions follow the arrival schedule and teardown runs after the load drains.</p></div></div>
              <div class="plan-actions">
                <button class="command-button quiet" id="add-thread-group-button"><i data-lucide="users"></i> Add thread group</button>
                <button class="command-button quiet" id="remove-thread-group-button" ${threadGroups.length === 1 ? "disabled" : ""}><i data-lucide="x-circle"></i> Remove group</button>
                <button class="command-button quiet" id="add-setup-button"><i data-lucide="wrench"></i> Add setup sampler</button>
                <button class="command-button quiet" id="add-transaction-button"><i data-lucide="braces"></i> Add transaction sampler</button>
                <button class="command-button quiet" id="add-teardown-button"><i data-lucide="flag"></i> Add teardown sampler</button>
              </div>
              <div class="form-grid two group-name-field">${textField("Thread group name", "threadGroupName", activeThreadGroup().name)}<div class="group-summary"><strong>${threadGroups.length} thread groups</strong><span>${plannedSamples()} total scheduled samples</span></div></div>
              <div class="phase-list">${activeThreadGroup().samplers.map((sampler, index) => `<span class="phase-chip ${index === activeSamplerIndex ? "selected" : ""}" data-sampler-index="${index}"><strong>${index + 1}</strong>${phaseLabel(sampler.phase)}<button title="Remove ${phaseLabel(sampler.phase)} sampler" data-remove-phase="${index}"><i data-lucide="x-circle"></i></button></span>`).join("")}</div>
            </section>

            <section class="form-section">
              <div class="section-title"><i data-lucide="git-branch"></i><div><h2>Chain connection</h2><p>The target RPC and dynamic call identity.</p></div></div>
              <div class="form-grid two">
                ${textField("Scenario name", "name", scenario.name)}
                ${textField("WebSocket RPC", "endpoint", scenario.endpoint)}
                ${textField("Pallet", "pallet", scenario.pallet)}
                ${textField("Call", "call", scenario.call)}
              </div>
              <label class="field full"><span>Call arguments JSON</span><textarea id="argumentsJson" spellcheck="false">${escapeHtml(scenario.argumentsJson)}</textarea></label>
            </section>

            <section class="form-section split-section">
              <div>
                <div class="section-title"><i data-lucide="users"></i><div><h2>Virtual users</h2><p>Deterministic signers and bounded submission pressure.</p></div></div>
                <div class="form-grid two">
                  ${numberField("Virtual users", "virtualUsers", scenario.virtualUsers, 1)}
                  ${numberField("Concurrency", "concurrency", scenario.concurrency, 1)}
                  ${textField("Signer derivation", "signerSource", scenario.signerSource)}
                  ${numberField("Mortality period", "mortalityPeriod", scenario.mortalityPeriod, 4)}
                </div>
              </div>
              <div class="boundary-box">
                <span class="field-label">Wait for</span>
                <div class="segmented" id="completion-control">
                  ${segmentedButton("submitted", "Submitted")}
                  ${segmentedButton("in_block", "In block")}
                  ${segmentedButton("finalized", "Finalized")}
                </div>
                ${numberField("Finality deadline (ms)", "finalityTimeoutMs", scenario.finalityTimeoutMs, 1000)}
                ${numberField("Max sample elapsed (ms)", "maxElapsedMs", scenario.maxElapsedMs, 0)}
                ${numberField("Whole-run deadline (ms)", "wholeRunTimeoutMs", scenario.wholeRunTimeoutMs, 1000)}
				${numberField("Shutdown drain deadline (ms)", "shutdownDrainTimeoutMs", scenario.shutdownDrainTimeoutMs, 1000)}
              </div>
            </section>

            <section class="form-section">
              <div class="section-title"><i data-lucide="shield-check"></i><div><h2>Assertions and collectors</h2><p>The selected sampler requires a successful transaction and can enforce a latency ceiling.</p></div></div>
              <div class="collector-list">${(["jtl", "events_jsonl", "telemetry_jsonl", "summary", "svg_plots"] as Collector[]).map((collector) => `<label><input type="checkbox" data-collector="${collector}" ${collectors.includes(collector) ? "checked" : ""}/><span>${collector.replaceAll("_", " ")}</span></label>`).join("")}</div>
            </section>

            <section class="form-section">
              <div class="section-title"><i data-lucide="timer"></i><div><h2>Arrival model</h2><p>Shape when virtual users reach the sampler.</p></div></div>
              <div class="arrival-layout">
                <div class="segmented vertical" id="arrival-control">
                  ${arrivalButton("burst", "Burst", "Release every user inside one window")}
                  ${arrivalButton("ramp", "Ramp", "Spread users evenly over a duration")}
                  ${arrivalButton("poisson", "Poisson", "Use deterministic exponential spacing")}
                </div>
                <div class="arrival-config">
                  <div class="arrival-header"><strong>${arrivalKind() === "burst" ? "Burst window" : arrivalKind() === "ramp" ? "Ramp duration" : "Poisson rate"}</strong><span>All offsets are deterministic for a reproducible preview.</span></div>
                  ${arrivalFields()}
                  <div class="mini-plot" aria-label="Arrival preview">
                    ${arrivalBars()}
                  </div>
                  <div class="plot-axis"><span>0 ms</span><span>${previewDuration}</span></div>
                </div>
              </div>
            </section>

            <section class="form-section result-section">
              <div class="section-title"><i data-lucide="cpu"></i><div><h2>Preflight</h2><p>Validation and schedule preview run in the Rust backend.</p></div></div>
              ${resultPanel()}
            </section>
            ${liveResultsPanel()}
            ${reportPanel()}
          </div>
        </section>

        <aside class="monitor-panel">
          <div class="panel-heading"><span>Run monitor</span><span class="live-dot">Preview</span></div>
          <div class="metric-grid">
            ${metric("Planned samples", String(plannedSamples()), "all groups", "boxes")}
            ${metric("Parallel sends", String(parallelSends()), "max", "gauge")}
            ${metric("Target", `${scenario.pallet}.${scenario.call}`, "call", "braces")}
            ${metric("Schedule", previewDuration, "preview", "timer")}
          </div>
          <section class="monitor-section">
            <div class="monitor-title"><span>Execution path</span><i data-lucide="circle-dot"></i></div>
            <ol class="execution-path">
              <li><span class="step-icon"><i data-lucide="cable"></i></span><div><strong>Connect</strong><small>RPC endpoint is structurally validated</small></div></li>
              <li><span class="step-icon"><i data-lucide="users"></i></span><div><strong>Prepare</strong><small>${scenario.virtualUsers} deterministic virtual users</small></div></li>
              <li><span class="step-icon"><i data-lucide="play"></i></span><div><strong>Sample</strong><small>${arrivalKind()} at up to ${scenario.concurrency} parallel submissions</small></div></li>
              <li><span class="step-icon"><i data-lucide="activity"></i></span><div><strong>Collect</strong><small>${runStatus.completedSamples} completed, ${runStatus.failedSamples} failed</small></div></li>
            </ol>
          </section>
          <section class="monitor-section note">
            <div class="monitor-title"><span>Current boundary</span><i data-lucide="shield-check"></i></div>
            <p>${escapeHtml(runStatus.message ?? "Preflight the chain, then explicitly arm the scenario.")}</p>
          </section>
          ${runStatus.artifactDir ? `<button class="command-button quiet report-button" id="open-report-button"><i data-lucide="activity"></i> Open run report</button>` : ""}
        </aside>
      </div>
      <div class="toast" id="toast" role="status"></div>
    </main>
  `;

  bindEvents();
  refreshIcons();
}

function textField(label: string, field: string, value: string): string {
  return `<label class="field"><span>${label}</span><input id="${field}" value="${escapeHtml(value)}" /></label>`;
}

function numberField(label: string, field: string, value: number, min: number): string {
  return `<label class="field"><span>${label}</span><input id="${field}" type="number" min="${min}" value="${value}" /></label>`;
}

function segmentedButton(value: Scenario["completion"], label: string): string {
  return `<button class="segment ${scenario.completion === value ? "selected" : ""}" data-completion="${value}">${label}</button>`;
}

function arrivalButton(value: ArrivalModel["kind"], label: string, description: string): string {
  return `<button class="arrival-choice ${arrivalKind() === value ? "selected" : ""}" data-arrival="${value}"><strong>${label}</strong><small>${description}</small></button>`;
}

function arrivalFields(): string {
  if (scenario.arrival.kind === "poisson") {
    return numberField("Average users per second", "ratePerSecond", scenario.arrival.ratePerSecond, 1);
  }

  const label = scenario.arrival.kind === "burst" ? "Window (ms)" : "Duration (ms)";
  return numberField(label, "arrivalDuration", scenario.arrival.kind === "burst" ? scenario.arrival.windowMs : scenario.arrival.durationMs, 1);
}

function arrivalBars(): string {
  const count = 24;
  const values = Array.from({ length: count }, (_, index) => {
    if (arrivalKind() === "burst") return 72 + (index % 3) * 8;
    if (arrivalKind() === "ramp") return 18 + index * 3;
    return 24 + ((index * 17) % 53);
  });
  return values.map((height) => `<span style="height:${height}%"></span>`).join("");
}

function resultPanel(): string {
  if (preflightView(lastValidation?.valid, lastPreflight, preflightError) === "live_error") {
    return `<div class="preflight-error"><i data-lucide="x-circle"></i><div><strong>Live metadata preflight failed</strong><p>${escapeHtml(preflightError!)}</p></div></div>`;
  }
  if (!lastValidation) {
    return `<div class="preflight-empty"><i data-lucide="check-circle-2"></i><div><strong>Ready to validate</strong><p>Validate the scenario to receive a backend schedule preview.</p></div></div>`;
  }

  if (lastPreflight) {
    const calls = lastPreflight.selectedCalls.map((call) => `${call.pallet}.${call.call}: ${call.encodable ? "encodable" : call.error ?? "not encodable"}`).join("; ");
    const accounts = lastPreflight.derivedAccounts.slice(0, 2).map((account) => account.address.slice(0, 16)).join(", ");
    return `<div class="preflight-success"><i data-lucide="check-circle-2"></i><div><strong>Live metadata preflight completed</strong><p>Runtime ${lastPreflight.specVersion}, metadata ${escapeHtml(lastPreflight.metadataHash.slice(0, 18))}..., ${lastPreflight.pallets.length} pallets. ${escapeHtml(calls)}</p><p>Resolved samples: ${lastPreflight.resolvedSampleCount}. Derived accounts: ${escapeHtml(accounts || "none")}.</p></div></div>`;
  }

  if (lastValidation.valid) {
    return `<div class="preflight-success"><i data-lucide="check-circle-2"></i><div><strong>Scenario is structurally valid</strong><p>${lastValidation.estimatedSamples} samples planned. ${lastPreview?.offsetsMs.length ?? 0} seeded schedule points returned by Rust.</p></div></div>`;
  }

  return `<div class="preflight-error"><i data-lucide="x-circle"></i><div><strong>Scenario needs attention</strong><ul>${lastValidation.issues.map((issue) => `<li><code>${escapeHtml(issue.field)}</code> ${escapeHtml(issue.message)}</li>`).join("")}</ul></div></div>`;
}

function reportPanel(): string {
  if (!runReport) return "";
  const summary = escapeHtml(runReport.summary);
  return `<section class="form-section report-section">
    <div class="section-title"><i data-lucide="activity"></i><div><h2>Run results</h2><p>Historical artifacts generated by the Rust collector.</p></div></div>
    <pre class="run-summary">${summary}</pre>
    <div class="report-plots">${runReport.plots.map((plot) => `<article class="report-plot"><h3>${escapeHtml(plot.name.replace(/-/g, " "))}</h3><div class="plot-frame">${plot.svg}</div></article>`).join("")}</div>
  </section>`;
}

function liveResultsPanel(): string {
  if (liveSamples.length === 0) return "";
  const metrics = liveMetrics(liveSamples);
  const bars = liveSamples.slice(-24).map((sample) => Math.max(8, Math.min(100, sample.elapsedMs / Math.max(...liveSamples.map((item) => item.elapsedMs), 1) * 100)));
  const latest = liveSamples.at(-1)!;
  return `<section class="form-section live-results-section">
    <div class="section-title"><i data-lucide="activity"></i><div><h2>Live results</h2><p>Bounded streamed samples from the active run.</p></div></div>
    <div class="live-metrics"><div><span>Throughput</span><strong>${metrics.throughput.toFixed(1)} / s</strong></div><div><span>Latency p95</span><strong>${metrics.p95} ms</strong></div><div><span>Failures</span><strong>${metrics.failures}</strong></div><div><span>Latest</span><strong>${escapeHtml(latest.responseCode)}</strong></div></div>
    <div class="mini-plot live-plot">${bars.map((height) => `<span style="height:${height}%"></span>`).join("")}</div>
  </section>`;
}

function metric(label: string, value: string, detail: string, icon: string): string {
  return `<div class="metric"><i data-lucide="${icon}"></i><span>${label}</span><strong>${escapeHtml(value)}</strong><small>${detail}</small></div>`;
}

function bindEvents(): void {
  const strings: (keyof Pick<Scenario, "name" | "endpoint" | "pallet" | "call" | "argumentsJson" | "signerSource">)[] = ["name", "endpoint", "pallet", "call", "argumentsJson", "signerSource"];
  for (const field of strings) {
    const input = document.querySelector<HTMLInputElement | HTMLTextAreaElement>(`#${field}`);
    input?.addEventListener("input", () => {
      scenario[field] = input.value;
      markDraftChanged();
    });
  }

  const numbers: (keyof Pick<Scenario, "virtualUsers" | "concurrency" | "mortalityPeriod" | "finalityTimeoutMs" | "maxElapsedMs" | "wholeRunTimeoutMs" | "shutdownDrainTimeoutMs">)[] = ["virtualUsers", "concurrency", "mortalityPeriod", "finalityTimeoutMs", "maxElapsedMs", "wholeRunTimeoutMs", "shutdownDrainTimeoutMs"];
  for (const field of numbers) {
    const input = document.querySelector<HTMLInputElement>(`#${field}`);
    input?.addEventListener("input", () => {
      scenario[field] = numericValue(input.value);
      syncActiveThreadGroup();
      markDraftChanged();
    });
  }

  const threadGroupName = document.querySelector<HTMLInputElement>("#threadGroupName");
  threadGroupName?.addEventListener("input", () => {
    activeThreadGroup().name = threadGroupName.value;
    markDraftChanged();
  });
  threadGroupName?.addEventListener("change", () => {
    render();
  });

  const arrivalDuration = document.querySelector<HTMLInputElement>("#arrivalDuration");
  arrivalDuration?.addEventListener("input", () => {
    const duration = numericValue(arrivalDuration.value);
    if (scenario.arrival.kind === "burst") scenario.arrival.windowMs = duration;
    if (scenario.arrival.kind === "ramp") scenario.arrival.durationMs = duration;
    syncActiveThreadGroup();
    markDraftChanged();
  });
  const ratePerSecond = document.querySelector<HTMLInputElement>("#ratePerSecond");
  ratePerSecond?.addEventListener("input", () => {
    if (scenario.arrival.kind === "poisson") scenario.arrival.ratePerSecond = numericValue(ratePerSecond.value);
    syncActiveThreadGroup();
    markDraftChanged();
  });

  document.querySelectorAll<HTMLButtonElement>("[data-completion]").forEach((button) => {
    button.addEventListener("click", () => {
      scenario.completion = button.dataset.completion as Scenario["completion"];
      markDraftChanged();
      render();
    });
  });
  document.querySelectorAll<HTMLButtonElement>("[data-arrival]").forEach((button) => {
    button.addEventListener("click", () => {
      const kind = button.dataset.arrival as ArrivalModel["kind"];
      scenario.arrival = kind === "burst" ? { kind, windowMs: 1000 } : kind === "ramp" ? { kind, durationMs: 60000 } : { kind, ratePerSecond: 100 };
      syncActiveThreadGroup();
      lastPreview = undefined;
      markDraftChanged();
      render();
    });
  });
  document.querySelectorAll<HTMLButtonElement>("[data-remove-phase]").forEach((button) => {
    button.addEventListener("click", () => {
      const index = Number(button.dataset.removePhase);
      if (activeThreadGroup().samplers.length === 1) {
        showToast("A thread group needs at least one sampler");
        return;
      }
      activeThreadGroup().samplers = removeSampler(activeThreadGroup().samplers, index);
      activeSamplerIndex = Math.min(activeSamplerIndex, activeThreadGroup().samplers.length - 1);
      loadActiveSampler();
      syncActiveThreadGroup();
      lastPreflight = undefined;
      render();
    });
  });
  for (const [id, phase] of [["#add-setup-button", "setup"], ["#add-transaction-button", "transaction"], ["#add-teardown-button", "teardown"]] as const) {
    document.querySelector<HTMLButtonElement>(id)?.addEventListener("click", () => {
      activeThreadGroup().samplers.push(newSampler(`${activeThreadGroup().id}-sampler-${Date.now()}`, phase));
      activeSamplerIndex = activeThreadGroup().samplers.length - 1;
      loadActiveSampler();
      syncActiveThreadGroup();
      lastPreflight = undefined;
      render();
    });
  }
  document.querySelector<HTMLButtonElement>("#preflight-button")?.addEventListener("click", () => void runPreflight());
  document.querySelector<HTMLButtonElement>("#run-button")?.addEventListener("click", () => void armOrStopRun());
  document.querySelector<HTMLButtonElement>("#reset-button")?.addEventListener("click", () => {
    scenario = structuredClone(initialScenario);
    lastValidation = undefined;
    lastPreview = undefined;
    lastPreflight = undefined;
    threadGroups = [newThreadGroup("group-1", "Virtual users")];
    activeThreadGroupId = threadGroups[0].id;
    activeSamplerIndex = 0;
    render();
    showToast("Scenario reset");
  });
  document.querySelector<HTMLButtonElement>("#save-button")?.addEventListener("click", () => void saveScenarioFile());
  document.querySelector<HTMLButtonElement>("#load-button")?.addEventListener("click", () => void loadScenarioFile());
  document.querySelector<HTMLButtonElement>("#open-report-button")?.addEventListener("click", () => void loadRunReport());
  document.querySelectorAll<HTMLButtonElement>("[data-thread-group]").forEach((button) => {
    button.addEventListener("click", () => {
      selectThreadGroup(button.dataset.threadGroup!);
      render();
    });
  });
  document.querySelectorAll<HTMLElement>("[data-sampler-index]").forEach((element) => {
    element.addEventListener("click", (event) => {
      if ((event.target as HTMLElement).closest("[data-remove-phase]")) return;
      syncActiveThreadGroup(); activeSamplerIndex = Number(element.dataset.samplerIndex); loadActiveSampler(); render();
    });
  });
  document.querySelectorAll<HTMLInputElement>("[data-collector]").forEach((input) => {
    input.addEventListener("change", () => { const collector = input.dataset.collector as Collector; collectors = input.checked ? [...collectors, collector] : collectors.filter((value) => value !== collector); markDraftChanged(); });
  });
  document.querySelector<HTMLButtonElement>("#add-thread-group-button")?.addEventListener("click", () => {
    syncActiveThreadGroup();
    const group = newThreadGroup(`group-${Date.now()}`, `Thread group ${threadGroups.length + 1}`);
    threadGroups.push(group);
    selectThreadGroup(group.id);
    lastPreflight = undefined;
    render();
  });
  document.querySelector<HTMLButtonElement>("#remove-thread-group-button")?.addEventListener("click", () => {
    if (threadGroups.length === 1) return;
    const removed = activeThreadGroupId;
    threadGroups = removeThreadGroup(threadGroups, removed);
    selectThreadGroup(threadGroups[0].id);
    lastPreflight = undefined;
    render();
  });
}

async function previewScenario(): Promise<void> {
  try {
    lastValidation = await invoke<ScenarioValidation>("validate_native_scenario", { document: nativeScenario() });
    lastPreview = lastValidation.valid
      ? await invoke<SchedulePreview>("preview_schedule", { virtualUsers: scenario.virtualUsers, arrival: scenario.arrival, seed: 1 })
      : undefined;
    render();
    showToast(lastValidation.valid ? "Rust preflight completed" : "Fix the reported fields before running");
  } catch (error) {
    showToast(`Backend error: ${String(error)}`);
  }
}

function nativeScenario(): NativeScenarioDocument {
  syncActiveThreadGroup();
  return buildNativeScenario(scenario, threadGroups, collectors);
}

async function runPreflight(): Promise<void> {
  preflightError = undefined;
  await previewScenario();
  if (!lastValidation?.valid) return;
  try {
    lastPreflight = await invoke<PreflightReport>("preflight_scenario", { document: nativeScenario() });
    preflightRunId = lastPreflight.runId;
    render();
    const failed = lastPreflight.selectedCalls.find((call) => !call.encodable);
    showToast(failed ? `Call cannot encode: ${failed.error}` : "Live chain preflight completed");
  } catch (error) {
    preflightError = String(error);
    render();
    showToast(`Preflight failed: ${preflightError}`);
  }
}

async function armOrStopRun(): Promise<void> {
  try {
    const intent = decideRunIntent(runStatus.state, lastPreflight);
    if (intent === "stop") {
      runStatus = await invoke<RunStatus>("stop_run");
    } else if (intent === "blocked") {
      showToast("Run a successful live chain preflight before arming");
      return;
    } else {
      if (!preflightRunId) throw new Error("preflight did not return an arming run ID");
      runStatus = await invoke<RunStatus>("start_run", { document: nativeScenario(), outputRoot: "target/polkameter-runs", runId: preflightRunId });
      runReport = undefined;
      liveSamples = [];
    }
    render();
  } catch (error) {
    showToast(`Run failed to start: ${String(error)}`);
  }
}

async function loadRunReport(): Promise<void> {
  if (!runStatus.artifactDir) return;
  try {
    runReport = await invoke<DashboardReport>("read_run_report", { artifactDir: runStatus.artifactDir });
    render();
    showToast("Run report loaded");
  } catch (error) {
    showToast(`Could not load run report: ${String(error)}`);
  }
}

function scenarioFilePath(): string {
  return `target/polkameter-scenarios/${scenario.name.toLowerCase().replace(/[^a-z0-9]+/g, "-").replace(/(^-|-$)/g, "") || "scenario"}.polkameter.json`;
}

async function saveScenarioFile(): Promise<void> {
  try {
    const path = scenarioFilePath();
    await invoke("save_scenario", { document: nativeScenario(), path });
    const { signerSource: _signerSource, ...persisted } = scenario;
    localStorage.setItem("polkameter-scenario", JSON.stringify(persisted));
    showToast(`Saved redacted scenario to ${path}`);
  } catch (error) {
    showToast(`Could not save scenario: ${String(error)}`);
  }
}

async function loadScenarioFile(): Promise<void> {
  try {
    const document = await invoke<NativeScenarioDocument>("load_scenario", { path: scenarioFilePath() });
    const group = document.threadGroups[0];
    const primary = group?.samplers.find((sampler) => sampler.phase === "transaction") ?? group?.samplers[0];
    if (!group || !primary) throw new Error("scenario has no editable thread group sampler");
    const preservedSuri = scenario.signerSource;
    scenario = {
      name: document.testPlan.name,
      endpoint: document.chain.endpoint,
      pallet: primary.pallet,
      call: primary.call,
      argumentsJson: JSON.stringify(primary.arguments, null, 2),
      signerSource: preservedSuri,
      virtualUsers: group.users,
      concurrency: group.concurrency,
      arrival: group.arrival,
      completion: primary.completion,
      mortalityPeriod: primary.mortalityPeriod,
      finalityTimeoutMs: primary.finalityTimeoutMs,
      maxElapsedMs: primary.assertions.find((assertion) => assertion.kind === "max_elapsed")?.milliseconds ?? 0,
      wholeRunTimeoutMs: document.testPlan.limits.wholeRunTimeoutMs,
      shutdownDrainTimeoutMs: document.testPlan.limits.shutdownDrainTimeoutMs
    };
    threadGroups = document.threadGroups.map((loaded, index) => ({
      id: `group-${index + 1}`,
      name: loaded.name,
      virtualUsers: loaded.users,
      concurrency: loaded.concurrency,
      arrival: loaded.arrival,
      samplers: loaded.samplers.map((sampler, samplerIndex) => ({ id: `group-${index + 1}-sampler-${samplerIndex + 1}`, phase: sampler.phase, label: sampler.label, pallet: sampler.pallet, call: sampler.call, argumentsJson: JSON.stringify(sampler.arguments, null, 2), completion: sampler.completion, mortalityPeriod: sampler.mortalityPeriod, finalityTimeoutMs: sampler.finalityTimeoutMs, maxElapsedMs: sampler.assertions.find((assertion) => assertion.kind === "max_elapsed")?.milliseconds ?? 0 }))
    }));
    activeThreadGroupId = threadGroups[0].id;
    activeSamplerIndex = 0;
    collectors = document.collectors;
    lastValidation = undefined;
    lastPreview = undefined;
    lastPreflight = undefined;
    preflightRunId = undefined;
    render();
    showToast("Scenario reopened. Enter signer material before arming if required.");
  } catch (error) {
    showToast(`Could not load scenario: ${String(error)}`);
  }
}

function formatDuration(value: number): string {
  if (value < 1000) return `${value} ms`;
  return `${(value / 1000).toFixed(value < 10000 ? 1 : 0)} s`;
}

function phaseLabel(phase: "setup" | "transaction" | "teardown"): string {
  return phase.charAt(0).toUpperCase() + phase.slice(1);
}

function escapeHtml(value: string): string {
  return value.replace(/[&<>'"]/g, (character) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", "'": "&#039;", '"': "&quot;" })[character]!);
}

function showToast(message: string): void {
  const toast = document.querySelector<HTMLDivElement>("#toast")!;
  toast.textContent = message;
  toast.classList.add("visible");
  window.setTimeout(() => toast.classList.remove("visible"), 2400);
}

function refreshIcons(): void {
  const iconMap = {
    Activity, Boxes, Braces, Cable, CheckCircle2, ChevronDown, CircleDot, ClipboardList, Cpu, Flag, FolderOpen, Gauge, GitBranch, Play, RotateCcw, Save, ShieldCheck, Square, Timer, Users, Wrench, XCircle
  };
  createIcons({
    icons: iconMap,
    attrs: { "aria-hidden": "true", height: "16", width: "16" }
  });
}

const savedScenario = localStorage.getItem("polkameter-scenario");
if (savedScenario) {
  try {
    const persisted = JSON.parse(savedScenario) as Partial<Scenario>;
    scenario = { ...initialScenario, ...persisted, signerSource: initialScenario.signerSource };
  } catch {
    localStorage.removeItem("polkameter-scenario");
  }
}

render();

void listen<RunStatus>("run-status", (event) => {
  runStatus = event.payload;
  render();
  if (["completed", "completed_with_failures", "stopped", "failed"].includes(runStatus.state) && runStatus.artifactDir) {
    void loadRunReport();
  }
});
void listen<SampleBatch>("sample-batch", (event) => {
  runStatus.completedSamples = event.payload.completedSamples;
  liveSamples = appendLiveSample(liveSamples, { ...event.payload, receivedAt: Date.now() });
  if (!liveRenderQueued) {
    liveRenderQueued = true;
    window.setTimeout(() => {
      liveRenderQueued = false;
      render();
    }, 150);
  }
});
