import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import DOMPurify from "dompurify";
import {
  Activity,
  Boxes,
  Braces,
  Cable,
  CheckCircle2,
  ChevronDown,
  CircleDot,
  CircleHelp,
  ClipboardList,
  Cpu,
  createIcons,
  Flag,
  FolderOpen,
  Gauge,
  GitBranch,
  PanelLeftClose,
  PanelLeftOpen,
  PanelRightClose,
  PanelRightOpen,
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
import type { ArrivalModel, Collector, DashboardReport, JmxImportReport, NativeScenarioDocument, PreflightReport, RemoteRunnerTarget, RunStatus, RuntimeArgumentField, RuntimeCall, RuntimeMetadata, SampleBatch, Scenario, ScenarioValidation, SchedulePreview } from "./types";
import { buildNativeScenario, removeSampler, removeThreadGroup, type EditablePhase, type EditableSampler, type EditableThreadGroup } from "./scenario-plan";
import { decideRunIntent, preflightView } from "./run-state";
import { appendLiveSample, liveMetrics, type LiveSample } from "./live-results";
import { maybeStartTour, refreshActiveTour, startTour } from "./tour";
import { svgImageDataUrl } from "./svg-data";
import polkameterMark from "./assets/polkameter-mark.png";
import "./styles.css";

const initialScenario: Scenario = {
  name: "1000 user transfer burst",
  endpoint: "ws://127.0.0.1:9944",
	prometheusEndpoint: "",
  pallet: "balances",
  call: "transfer_keep_alive",
  argumentsJson: '{\n  "dest": { "$variant": "Id", "value": { "$bytes": "0xd43593c715fdd31c61141abd04a99fd6822c8558854ccde39a5684e7a56da27d" } },\n  "value": "1000000000000"\n}',
	signerProfile: "local-dev",
  signerSource: "",
	  fundDerivedUsers: false,
	  fundingAmount: "10000000000000",
	  fundingFinalityTimeoutMs: 60000,
	  fundingBatchSize: 50,
	virtualUsers: 1000,
	concurrency: 1000,
	iterations: 1,
  arrival: { kind: "burst", windowMs: 1000 },
  completion: "finalized",
  mortalityPeriod: 4096,
  finalityTimeoutMs: 300000,
  maxElapsedMs: 0,
  wholeRunTimeoutMs: 900000,
	shutdownDrainTimeoutMs: 300000,
	maxConcurrentSamples: 1000
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
let remoteRunnerEndpoint = "";
let remoteRunnerToken = "";
let activeRemoteTarget: RemoteRunnerTarget | undefined;
type PlanNode = "plan" | "connection" | "assertions" | "collector";
let activePlanNode: PlanNode | undefined = "plan";
let planMenuOpen = false;
let planPanelCollapsed = false;
let monitorPanelCollapsed = false;
let planPanelWidth = 240;
let monitorPanelWidth = 320;
let toastMessage = "";
let toastVisible = false;
let toastTimer: number | undefined;
let remotePollFailures = 0;
let runtimeMetadata: RuntimeMetadata | undefined;
let metadataLoading = false;
let currentScenarioPath = "";

const PANEL_WIDTHS = {
  collapsed: 52,
  plan: { min: 180, max: 420, initial: 240 },
  monitor: { min: 220, max: 480, initial: 320 }
} as const;

function newThreadGroup(id: string, name: string): EditableThreadGroup {
  return {
    id,
    name,
	virtualUsers: scenario.virtualUsers,
	concurrency: scenario.concurrency,
	iterations: scenario.iterations,
    arrival: structuredClone(scenario.arrival),
    samplers: [newSampler(`${id}-sampler-1`, "transaction")]
  };
}

function newSampler(id: string, phase: EditablePhase): EditableSampler {
  return { id, phase, label: `${samplerLabelPhase(phase)}.${scenario.pallet}.${scenario.call}`, pallet: scenario.pallet, call: scenario.call, argumentsJson: scenario.argumentsJson, completion: scenario.completion, mortalityPeriod: scenario.mortalityPeriod, finalityTimeoutMs: scenario.finalityTimeoutMs, maxElapsedMs: scenario.maxElapsedMs };
}

function activeThreadGroup(): EditableThreadGroup {
  return threadGroups.find((group) => group.id === activeThreadGroupId) ?? threadGroups[0];
}

function syncActiveThreadGroup(): void {
  const group = activeThreadGroup();
	group.virtualUsers = scenario.virtualUsers;
	group.concurrency = scenario.concurrency;
	group.iterations = scenario.iterations;
  group.arrival = structuredClone(scenario.arrival);
  const sampler = group.samplers[activeSamplerIndex];
  if (sampler) Object.assign(sampler, { pallet: scenario.pallet, call: scenario.call, argumentsJson: scenario.argumentsJson, completion: scenario.completion, mortalityPeriod: scenario.mortalityPeriod, finalityTimeoutMs: scenario.finalityTimeoutMs, maxElapsedMs: scenario.maxElapsedMs, label: `${samplerLabelPhase(sampler.phase)}.${scenario.pallet}.${scenario.call}` });
}

function selectThreadGroup(id: string): void {
  syncActiveThreadGroup();
  const group = threadGroups.find((candidate) => candidate.id === id);
  if (!group) return;
  activeThreadGroupId = group.id;
  activeSamplerIndex = 0;
	scenario.virtualUsers = group.virtualUsers;
	scenario.concurrency = group.concurrency;
	scenario.iterations = group.iterations;
  scenario.arrival = structuredClone(group.arrival);
  loadActiveSampler();
}

function loadActiveSampler(): void {
  const sampler = activeThreadGroup().samplers[activeSamplerIndex];
  if (!sampler) return;
  scenario.pallet = sampler.pallet; scenario.call = sampler.call; scenario.argumentsJson = sampler.argumentsJson; scenario.completion = sampler.completion; scenario.mortalityPeriod = sampler.mortalityPeriod; scenario.finalityTimeoutMs = sampler.finalityTimeoutMs; scenario.maxElapsedMs = sampler.maxElapsedMs;
}

function plannedSamples(): number {
	return threadGroups.reduce((total, group) => total + group.samplers.reduce((groupTotal, sampler) => groupTotal + (sampler.phase === "transaction" ? group.virtualUsers * group.iterations : 1), 0), 0);
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
  const savedScrollTop = document.querySelector<HTMLElement>(".editor-scroll")?.scrollTop ?? 0;
  const focused = document.activeElement;
  const focusedId = focused instanceof HTMLElement ? focused.id : "";
  const selection = focused instanceof HTMLInputElement || focused instanceof HTMLTextAreaElement
    ? { start: focused.selectionStart, end: focused.selectionEnd }
    : undefined;
  const previewDuration = lastPreview ? formatDuration(lastPreview.durationMs) : "Not calculated";
  const resultState = lastValidation ? (lastValidation.valid ? "Ready" : "Needs attention") : "Draft";
  const resultClass = lastValidation?.valid ? "success" : lastValidation ? "warning" : "neutral";
  const runActive = ["running", "arming", "stopping"].includes(runStatus.state);
  const runFinished = ["completed", "completed_with_failures", "stopped", "failed"].includes(runStatus.state);
  const monitorState = runStatus.state === "draft" ? (lastPreflight ? "Armed" : "Preview") : runStatus.state.replaceAll("_", " ");
  const stepClass = (done: boolean, active: boolean) => (active ? "active" : done ? "done" : "");
  const connectStep = stepClass(Boolean(lastPreflight) || runActive || runFinished, false);
  const prepareStep = stepClass(runActive || runFinished, runStatus.state === "arming");
  const sampleStep = stepClass(runFinished, runStatus.state === "running");
  const collectStep = stepClass(runFinished, runStatus.state === "stopping");

  const appMarkup = `
    <main class="shell">
      <header class="topbar" data-tauri-drag-region>
        <div class="product-lockup" data-tauri-drag-region>
          <div class="product-mark"><img src="${polkameterMark}" alt="Polkameter"/></div>
          <div>
            <div class="product-name">Polkameter</div>
            <div class="product-subtitle">Polkadot SDK load workbench</div>
          </div>
        </div>
        <div class="topbar-actions">
          <span class="state-pill ${resultClass}"><span></span>${resultState}</span>
          <button class="icon-button" id="tour-button" title="Guided tour"><i data-lucide="circle-help"></i></button>
          <button class="icon-button" id="reset-button" title="Reset scenario"><i data-lucide="rotate-ccw"></i></button>
		  <button class="command-button quiet" id="load-button"><i data-lucide="folder-open"></i> Open test</button>
          <button class="command-button quiet" id="save-button"><i data-lucide="save"></i> Save</button>
		  <button class="command-button quiet" id="save-as-button"><i data-lucide="save"></i> Save as</button>
		  <button class="command-button quiet" id="export-jmx-button"><i data-lucide="save"></i> Export JMX</button>
		  <button class="command-button quiet" id="import-jmx-button"><i data-lucide="folder-open"></i> Inspect JMX</button>
		  <input id="import-jmx-file" type="file" accept=".jmx,application/xml,text/xml" hidden/>
          <button class="command-button quiet" id="preflight-button"><i data-lucide="cable"></i> Preflight chain</button>
          <button class="command-button primary" id="run-button"><i data-lucide="${runActive ? "square" : "play"}"></i> ${runActive ? "Stop run" : "Arm and run"}</button>
        </div>
      </header>

      <div class="workspace" style="--plan-panel-width:${planPanelCollapsed ? PANEL_WIDTHS.collapsed : planPanelWidth}px; --monitor-panel-width:${monitorPanelCollapsed ? PANEL_WIDTHS.collapsed : monitorPanelWidth}px;">
        <aside class="plan-panel ${planPanelCollapsed ? "collapsed" : ""}">
          <div class="panel-heading"><span>Test plan</span><div class="panel-controls"><button class="icon-button" id="plan-menu-button" title="Plan options"><i data-lucide="chevron-down"></i></button><button class="icon-button" id="toggle-plan-panel" title="${planPanelCollapsed ? "Expand test plan" : "Collapse test plan"}"><i data-lucide="${planPanelCollapsed ? "panel-left-open" : "panel-left-close"}"></i></button></div>${planMenuOpen ? `<menu class="panel-menu"><button data-layout-action="toggle-plan">${planPanelCollapsed ? "Expand test plan" : "Collapse test plan"}</button><button data-layout-action="toggle-monitor">${monitorPanelCollapsed ? "Expand run monitor" : "Collapse run monitor"}</button><button data-layout-action="reset">Reset panel layout</button></menu>` : ""}</div>
          <nav class="plan-tree" aria-label="Test plan">
            <button class="tree-row root ${activePlanNode === "plan" ? "active" : ""}" data-plan-node="plan"><i data-lucide="clipboard-list"></i><span>${escapeHtml(scenario.name)}</span></button>
            <button class="tree-row indent ${activePlanNode === "connection" ? "active" : ""}" data-plan-node="connection"><i data-lucide="cable"></i><span>Chain connection</span><em>1</em></button>
            ${threadGroups.map((group) => `<button class="tree-row indent ${group.id === activeThreadGroupId ? "context" : ""}" data-thread-group="${group.id}"><i data-lucide="users"></i><span>${escapeHtml(group.name)}</span><em>${group.virtualUsers}</em></button>${group.samplers.map((sampler, index) => `<button class="tree-row indent-two phase-row ${group.id === activeThreadGroupId && index === activeSamplerIndex ? "active" : ""}" data-sampler-group="${group.id}" data-sampler-index="${index}"><i data-lucide="${sampler.phase === "setup" ? "wrench" : sampler.phase === "teardown" ? "flag" : "braces"}"></i><span>${escapeHtml(sampler.label)}</span><em>${sampler.phase === "transaction" ? group.concurrency : "1"}</em></button>`).join("")}`).join("")}
            <button class="tree-row indent ${activePlanNode === "assertions" ? "active" : ""}" data-plan-node="assertions"><i data-lucide="shield-check"></i><span>Assertions</span><em>1</em></button>
            <button class="tree-row indent ${activePlanNode === "collector" ? "active" : ""}" data-plan-node="collector"><i data-lucide="activity"></i><span>Collectors</span><em>5</em></button>
          </nav>
          <div class="plan-footer">
            <div><i data-lucide="shield-check"></i> Local scenario only</div>
            <small>Preview never submits an extrinsic.</small>
          </div>
        </aside>
        <div class="panel-resizer plan-resizer ${planPanelCollapsed ? "disabled" : ""}" data-resize-panel="plan" title="Resize test plan"></div>

        <section class="editor-panel">
          <div class="section-bar">
            <div><span class="eyebrow">${currentScenarioPath ? `Open XML plan · ${escapeHtml(fileName(currentScenarioPath))}` : "New XML plan"}</span><h1>${escapeHtml(scenario.name)}</h1></div>
            <div class="run-boundary"><span>Completion boundary</span><strong>${scenario.completion.replace("_", " ")}</strong></div>
          </div>

          <div class="editor-scroll">
            <section class="form-section" id="plan-section">
              <div class="section-title"><i data-lucide="clipboard-list"></i><div><h2>Plan structure</h2><p>Setup runs once, workflow steps run in order for every virtual user, and teardown runs after the load drains.</p></div></div>
              <div class="plan-actions">
                <button class="command-button quiet" id="add-thread-group-button"><i data-lucide="users"></i> Add thread group</button>
                <button class="command-button quiet" id="remove-thread-group-button" ${threadGroups.length === 1 ? "disabled" : ""}><i data-lucide="x-circle"></i> Remove group</button>
                <button class="command-button quiet" id="add-setup-button"><i data-lucide="wrench"></i> Add setup sampler</button>
                <button class="command-button quiet" id="add-transaction-button"><i data-lucide="braces"></i> Add workflow step</button>
                <button class="command-button quiet" id="add-teardown-button"><i data-lucide="flag"></i> Add teardown sampler</button>
              </div>
              <div class="form-grid two group-name-field">${textField("Thread group name", "threadGroupName", activeThreadGroup().name)}<div class="group-summary"><strong>${threadGroups.length} thread groups</strong><span>${plannedSamples()} total scheduled samples</span></div></div>
              <div class="phase-list">${activeThreadGroup().samplers.map((sampler, index) => `<span class="phase-chip ${index === activeSamplerIndex ? "selected" : ""}" data-sampler-index="${index}"><strong>${index + 1}</strong>${phaseLabel(sampler.phase)}<button title="Remove ${phaseLabel(sampler.phase)} sampler" data-remove-phase="${index}"><i data-lucide="x-circle"></i></button></span>`).join("")}</div>
            </section>

            <section class="form-section" id="connection-section">
              <div class="section-title"><i data-lucide="git-branch"></i><div><h2>Chain connection</h2><p>Choose a chain, then let its runtime describe the transaction fields.</p></div></div>
              <div class="form-grid two">
                ${textField("Scenario name", "name", scenario.name)}
				<label class="field rpc-field"><span>WebSocket RPC</span><div class="input-action"><input id="endpoint" value="${escapeHtml(scenario.endpoint)}"/><button class="command-button" id="load-metadata-button" type="button" ${metadataLoading ? "disabled" : ""}>${metadataLoading ? "Loading…" : "Load call fields"}</button></div><small>Read-only: this never submits a transaction.</small></label>
					${textField("Node Prometheus", "prometheusEndpoint", scenario.prometheusEndpoint)}
					${textField("Remote runner URL", "remoteRunnerEndpoint", remoteRunnerEndpoint)}
					<label class="field"><span>Remote runner token</span><input id="remoteRunnerToken" type="password" autocomplete="off" value="${escapeHtml(remoteRunnerToken)}"/></label>
              </div>
			  ${transactionForm()}
            </section>

            <section class="form-section split-section" id="users-section">
              <div>
                <div class="section-title"><i data-lucide="users"></i><div><h2>Virtual users</h2><p>Deterministic signers and bounded submission pressure.</p></div></div>
                <div class="form-grid two">
				${numberField("Virtual users", "virtualUsers", scenario.virtualUsers, 1)}
				${numberField("Concurrency", "concurrency", scenario.concurrency, 1)}
				${numberField("Iterations per virtual user", "iterations", scenario.iterations, 1)}
                ${textField("Signer profile", "signerProfile", scenario.signerProfile)}
                <label class="field"><span>Signer SURI</span><input id="signerSource" type="password" autocomplete="off" placeholder="Paste to save or replace this profile" value="${escapeHtml(scenario.signerSource)}"/><div class="plan-actions"><button class="command-button quiet" id="store-signer-button" type="button">Store signer</button><button class="command-button quiet" id="remove-signer-button" type="button">Forget signer</button></div></label>
                ${numberField("Mortality period", "mortalityPeriod", scenario.mortalityPeriod, 4)}
				<label class="field"><span>Fund derived users</span><input id="fundDerivedUsers" type="checkbox" ${scenario.fundDerivedUsers ? "checked" : ""}/></label>
				${scenario.fundDerivedUsers ? textField("Funding amount", "fundingAmount", scenario.fundingAmount) : ""}
				${scenario.fundDerivedUsers ? numberField("Funding finality deadline (ms)", "fundingFinalityTimeoutMs", scenario.fundingFinalityTimeoutMs, 1000) : ""}
				${scenario.fundDerivedUsers ? numberField("Funding batch size", "fundingBatchSize", scenario.fundingBatchSize, 1) : ""}
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
				${numberField("Plan concurrency limit", "maxConcurrentSamples", scenario.maxConcurrentSamples, 1)}
              </div>
            </section>

            <section class="form-section" id="assertions-section">
              <div class="section-title"><i data-lucide="shield-check"></i><div><h2>Assertions and collectors</h2><p>The selected sampler requires a successful transaction and can enforce a latency ceiling.</p></div></div>
              <div class="collector-list">${(["jtl", "events_jsonl", "telemetry_jsonl", "summary", "svg_plots"] as Collector[]).map((collector) => `<label><input type="checkbox" data-collector="${collector}" ${collectors.includes(collector) ? "checked" : ""}/><span>${collector.replaceAll("_", " ")}</span></label>`).join("")}</div>
            </section>

            <section class="form-section" id="arrival-section">
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

            <section class="form-section result-section" id="preflight-section">
              <div class="section-title"><i data-lucide="cpu"></i><div><h2>Preflight</h2><p>Validation and schedule preview run in the Rust backend.</p></div></div>
              ${resultPanel()}
            </section>
            <div id="live-slot">${liveResultsPanel()}</div>
            <div id="report-slot"></div>
          </div>
        </section>
        <div class="panel-resizer monitor-resizer ${monitorPanelCollapsed ? "disabled" : ""}" data-resize-panel="monitor" title="Resize run monitor"></div>

        <aside class="monitor-panel ${monitorPanelCollapsed ? "collapsed" : ""}">
          <div class="panel-heading"><span>Run monitor</span><div class="panel-controls"><span class="live-dot ${runActive ? "running" : ""}">${escapeHtml(monitorState)}</span><button class="icon-button" id="toggle-monitor-panel" title="${monitorPanelCollapsed ? "Expand run monitor" : "Collapse run monitor"}"><i data-lucide="${monitorPanelCollapsed ? "panel-right-open" : "panel-right-close"}"></i></button></div></div>
          <div class="metric-grid">
            ${metric("Planned samples", String(plannedSamples()), "all groups", "boxes")}
            ${metric("Parallel sends", String(parallelSends()), "max", "gauge")}
            ${metric("Target", `${scenario.pallet}.${scenario.call}`, "call", "braces")}
            ${metric("Schedule", previewDuration, "preview", "timer")}
          </div>
          <section class="monitor-section">
            <div class="monitor-title"><span>Execution path</span><i data-lucide="circle-dot"></i></div>
            <ol class="execution-path">
              <li class="${connectStep}"><span class="step-icon"><i data-lucide="cable"></i></span><div><strong>Connect</strong><small>${lastPreflight ? `Runtime ${lastPreflight.specVersion} preflighted` : "Preflight validates the RPC and encodes calls"}</small></div></li>
              <li class="${prepareStep}"><span class="step-icon"><i data-lucide="users"></i></span><div><strong>Prepare</strong><small>${scenario.virtualUsers} deterministic virtual users</small></div></li>
              <li class="${sampleStep}"><span class="step-icon"><i data-lucide="play"></i></span><div><strong>Sample</strong><small>${arrivalKind()} at up to ${scenario.concurrency} parallel submissions</small></div></li>
              <li class="${collectStep}"><span class="step-icon"><i data-lucide="activity"></i></span><div><strong>Collect</strong><small id="collect-progress">${runStatus.completedSamples} completed, ${runStatus.failedSamples} failed</small></div></li>
            </ol>
          </section>
          <section class="monitor-section note">
            <div class="monitor-title"><span>Status</span><i data-lucide="shield-check"></i></div>
            <p>${escapeHtml(runStatus.message ?? (lastPreflight ? "Preflight succeeded. Arm and run submits the plan." : "Arm and run preflights the chain first, then submits the plan."))}</p>
          </section>
          ${runStatus.artifactDir ? `<button class="command-button quiet report-button" id="open-report-button"><i data-lucide="activity"></i> Open run report</button>` : ""}
        </aside>
      </div>
      <div class="toast ${toastVisible ? "visible" : ""}" id="toast" role="status">${escapeHtml(toastMessage)}</div>
    </main>
  `;
  document.querySelector<HTMLDivElement>("#app")!.innerHTML = DOMPurify.sanitize(appMarkup, {
    USE_PROFILES: { html: true }
  });

  bindEvents();
  renderReportPanel();
  refreshIcons();
  restoreEditorState(savedScrollTop, focusedId, selection);
  refreshActiveTour();
}

function restoreEditorState(scrollTop: number, focusedId: string, selection: { start: number | null; end: number | null } | undefined): void {
  const editorScroll = document.querySelector<HTMLElement>(".editor-scroll");
  if (editorScroll) {
    editorScroll.style.scrollBehavior = "auto";
    editorScroll.scrollTop = scrollTop;
    editorScroll.style.scrollBehavior = "";
  }
  if (!focusedId) return;
  const element = document.getElementById(focusedId);
  if (!element) return;
  element.focus({ preventScroll: true });
  if (selection && selection.start !== null && selection.end !== null && (element instanceof HTMLInputElement || element instanceof HTMLTextAreaElement)) {
    try {
      element.setSelectionRange(selection.start, selection.end);
    } catch {
      // Number inputs reject setSelectionRange.
    }
  }
}

function textField(label: string, field: string, value: string): string {
  return `<label class="field"><span>${label}</span><input id="${field}" value="${escapeHtml(value)}" /></label>`;
}

function numberField(label: string, field: string, value: number, min: number): string {
  return `<label class="field"><span>${label}</span><input id="${field}" type="number" min="${min}" value="${value}" /></label>`;
}

function selectedRuntimeCall(): RuntimeCall | undefined {
  const pallet = runtimeMetadata?.pallets.find((item) => item.name.toLowerCase() === scenario.pallet.toLowerCase());
  return pallet?.calls.find((item) => item.name.toLowerCase() === scenario.call.toLowerCase());
}

function argumentObject(): Record<string, unknown> {
  try {
    const value = JSON.parse(scenario.argumentsJson) as unknown;
    return value !== null && typeof value === "object" && !Array.isArray(value) ? value as Record<string, unknown> : {};
  } catch {
    return {};
  }
}

function argumentKind(field: RuntimeArgumentField): "address" | "amount" | "boolean" | "json" {
  const type = `${field.name ?? ""} ${field.typeName ?? ""}`.toLowerCase();
  if (field.name?.toLowerCase() === "dest" || type.includes("accountid") || type.includes("multiaddress")) return "address";
  if (type.includes("bool")) return "boolean";
  if (field.name?.toLowerCase().includes("amount") || field.name?.toLowerCase() === "value" || /(?:^|[^a-z])u(?:8|16|32|64|128|256)(?:$|[^a-z])/.test(type) || type.includes("balance") || type.includes("compact")) return "amount";
  return "json";
}

function fieldValue(field: RuntimeArgumentField, index: number): string {
  const value = argumentObject()[field.name ?? String(index)];
  if (argumentKind(field) === "address" && value && typeof value === "object") {
    const encoded = value as { value?: { $bytes?: unknown } };
    return typeof encoded.value?.$bytes === "string" ? encoded.value.$bytes : "";
  }
  if (argumentKind(field) === "boolean") return value === true ? "true" : "false";
  if (argumentKind(field) === "amount") return value === undefined ? "" : String(value);
  return value === undefined ? "" : JSON.stringify(value, null, 2);
}

function friendlyArgumentField(field: RuntimeArgumentField, index: number): string {
  const key = field.name ?? String(index);
  const kind = argumentKind(field);
  const label = field.name ? humanize(field.name) : `Argument ${index + 1}`;
  const detail = [field.typeName, field.docs[0]].filter(Boolean).join(" · ");
  if (kind === "boolean") {
    return `<label class="friendly-field toggle"><span>${escapeHtml(label)}</span><select data-friendly-argument="${escapeHtml(key)}" data-argument-kind="${kind}"><option value="false" ${fieldValue(field, index) === "false" ? "selected" : ""}>No</option><option value="true" ${fieldValue(field, index) === "true" ? "selected" : ""}>Yes</option></select>${detail ? `<small>${escapeHtml(detail)}</small>` : ""}</label>`;
  }
  if (kind === "json") {
    return `<label class="friendly-field"><span>${escapeHtml(label)}</span><textarea data-friendly-argument="${escapeHtml(key)}" data-argument-kind="${kind}" rows="3" placeholder='Enter a JSON value'>${escapeHtml(fieldValue(field, index))}</textarea>${detail ? `<small>${escapeHtml(detail)}</small>` : ""}</label>`;
  }
  const addressHelp = kind === "address" ? "Paste an SS58 address or a 0x account ID." : "Use whole units; decimal values are not supported.";
  return `<label class="friendly-field"><span>${escapeHtml(label)}</span><input data-friendly-argument="${escapeHtml(key)}" data-argument-kind="${kind}" inputmode="${kind === "amount" ? "numeric" : "text"}" placeholder="${kind === "address" ? "e.g. 5GrwvaEF…" : "e.g. 1000000000000"}" value="${escapeHtml(fieldValue(field, index))}"/>${detail || addressHelp ? `<small>${escapeHtml(detail || addressHelp)}</small>` : ""}</label>`;
}

function transactionForm(): string {
  const pallets = runtimeMetadata?.pallets ?? [];
  const selectedPallet = pallets.find((item) => item.name.toLowerCase() === scenario.pallet.toLowerCase());
  const selectedCall = selectedRuntimeCall();
  const metadataNote = runtimeMetadata
    ? `Runtime ${runtimeMetadata.specVersion} · metadata ${runtimeMetadata.metadataHash.slice(0, 14)}…`
    : "Load metadata to replace SCALE-shaped JSON with normal fields.";
  const palletControl = pallets.length
    ? `<select id="pallet">${pallets.map((item) => `<option value="${escapeHtml(item.name)}" ${item.name.toLowerCase() === scenario.pallet.toLowerCase() ? "selected" : ""}>${escapeHtml(item.name)}</option>`).join("")}</select>`
    : `<input id="pallet" value="${escapeHtml(scenario.pallet)}" placeholder="e.g. Balances"/>`;
  const callControl = selectedPallet
    ? `<select id="call">${selectedPallet.calls.map((item) => `<option value="${escapeHtml(item.name)}" ${item.name.toLowerCase() === scenario.call.toLowerCase() ? "selected" : ""}>${escapeHtml(item.name)}</option>`).join("")}</select>`
    : `<input id="call" value="${escapeHtml(scenario.call)}" placeholder="e.g. transfer_keep_alive"/>`;
  return `<div class="transaction-builder">
    <div class="transaction-heading"><div><span class="field-label">Transaction</span><strong>What should each virtual user do?</strong></div><small>${escapeHtml(metadataNote)}</small></div>
    <div class="form-grid two"><label class="field"><span>Pallet</span>${palletControl}</label><label class="field"><span>Call</span>${callControl}</label></div>
    ${selectedCall ? `<div class="friendly-call"><div class="friendly-call-heading"><div><strong>${escapeHtml(humanize(selectedCall.name))}</strong><p>${escapeHtml(selectedCall.docs[0] || "Fields are generated from this chain’s current runtime metadata.")}</p></div><span>${selectedCall.fields.length} ${selectedCall.fields.length === 1 ? "field" : "fields"}</span></div><div class="friendly-fields">${selectedCall.fields.map(friendlyArgumentField).join("") || "<p>This call has no arguments.</p>"}</div></div>` : `<div class="metadata-empty"><i data-lucide="braces"></i><div><strong>Load the chain’s call fields</strong><p>Connect to the RPC above to browse available pallets and get labelled inputs for this transaction.</p></div></div>`}
    <details class="advanced-json"><summary>Advanced: edit call arguments as JSON</summary><label class="field full"><span>Raw runtime value</span><textarea id="argumentsJson" spellcheck="false">${escapeHtml(scenario.argumentsJson)}</textarea></label></details>
  </div>`;
}

function humanize(value: string): string {
  return value.replace(/_/g, " ").replace(/([a-z])([A-Z])/g, "$1 $2").replace(/\b\w/g, (character) => character.toUpperCase());
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
  if (lastPreview && lastPreview.offsetsMs.length > 0) {
    const span = Math.max(1, lastPreview.durationMs);
    const bins = new Array<number>(count).fill(0);
    for (const offset of lastPreview.offsetsMs) bins[Math.min(count - 1, Math.floor((offset / span) * count))] += 1;
    const peak = Math.max(...bins, 1);
    return bins.map((bin) => `<span style="height:${bin === 0 ? 2 : Math.max(6, Math.round((bin / peak) * 100))}%"></span>`).join("");
  }
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

function renderReportPanel(): void {
  const slot = document.querySelector<HTMLElement>("#report-slot");
  if (!slot || !runReport) return;
  const section = document.createElement("section");
  section.className = "form-section report-section";
  const title = document.createElement("div");
  title.className = "section-title";
  title.innerHTML = '<i data-lucide="activity"></i><div><h2>Run results</h2><p>Historical artifacts generated by the Rust collector.</p></div>';
  section.append(title);
  const summary = document.createElement("pre");
  summary.className = "run-summary";
  summary.textContent = runReport.summary;
  section.append(summary);
  const plots = document.createElement("div");
  plots.className = "report-plots";
  for (const plot of runReport.plots) {
    const name = plot.name.replace(/-/g, " ");
    const article = document.createElement("article");
    article.className = "report-plot";
    const heading = document.createElement("h3");
    heading.textContent = name;
    const frame = document.createElement("div");
    frame.className = "plot-frame";
    const image = document.createElement("img");
    image.className = "report-svg";
    image.alt = name;
    image.src = svgImageDataUrl(plot.svg);
    frame.append(image);
    article.append(heading, frame);
    plots.append(article);
  }
  section.append(plots);
  slot.replaceChildren(section);
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

function renderLiveSections(): void {
  const slot = document.querySelector<HTMLElement>("#live-slot");
  if (!slot) {
    render();
    return;
  }
  slot.innerHTML = liveResultsPanel();
  const collectProgress = document.querySelector<HTMLElement>("#collect-progress");
  if (collectProgress) collectProgress.textContent = `${runStatus.completedSamples} completed, ${runStatus.failedSamples} failed`;
  refreshIcons();
}

function metric(label: string, value: string, detail: string, icon: string): string {
  return `<div class="metric"><i data-lucide="${icon}"></i><span>${label}</span><strong>${escapeHtml(value)}</strong><small>${detail}</small></div>`;
}

function bindEvents(): void {
	const strings: (keyof Pick<Scenario, "name" | "endpoint" | "prometheusEndpoint" | "pallet" | "call" | "argumentsJson" | "signerProfile" | "signerSource" | "fundingAmount">)[] = ["name", "endpoint", "prometheusEndpoint", "pallet", "call", "argumentsJson", "signerProfile", "signerSource", "fundingAmount"];
  for (const field of strings) {
    const input = document.querySelector<HTMLInputElement | HTMLTextAreaElement | HTMLSelectElement>(`#${field}`);
    input?.addEventListener("input", () => {
      scenario[field] = input.value;
      if (field === "pallet" || field === "call" || field === "argumentsJson") syncActiveThreadGroup();
      markDraftChanged();
    });
		if (field === "pallet" || field === "call") {
			input?.addEventListener("change", () => {
				scenario[field] = input.value;
				if (field === "pallet") {
					const pallet = runtimeMetadata?.pallets.find((item) => item.name === scenario.pallet);
					if (pallet?.calls.length) scenario.call = pallet.calls[0].name;
				}
				syncActiveThreadGroup();
				markDraftChanged();
				render();
			});
		}
  }
	document.querySelector<HTMLButtonElement>("#load-metadata-button")?.addEventListener("click", () => void loadRuntimeMetadata());
	document.querySelectorAll<HTMLInputElement | HTMLTextAreaElement | HTMLSelectElement>("[data-friendly-argument]").forEach((input) => {
		const update = () => {
			const key = input.dataset.friendlyArgument!;
			const kind = input.dataset.argumentKind as "address" | "amount" | "boolean" | "json";
			const result = updateFriendlyArgument(key, kind, input.value);
			input.toggleAttribute("aria-invalid", !result.valid);
			if (!result.valid && document.activeElement !== input) showToast(result.error!);
		};
		input.addEventListener("input", update);
		input.addEventListener("change", update);
	});
	for (const [id, assign] of [
		["remoteRunnerEndpoint", (value: string) => remoteRunnerEndpoint = value],
		["remoteRunnerToken", (value: string) => remoteRunnerToken = value]
	] as const) {
		document.querySelector<HTMLInputElement>(`#${id}`)?.addEventListener("input", (event) => {
			assign((event.target as HTMLInputElement).value);
		});
	}

	const numbers: (keyof Pick<Scenario, "virtualUsers" | "concurrency" | "iterations" | "mortalityPeriod" | "finalityTimeoutMs" | "maxElapsedMs" | "wholeRunTimeoutMs" | "shutdownDrainTimeoutMs" | "maxConcurrentSamples" | "fundingFinalityTimeoutMs" | "fundingBatchSize">)[] = ["virtualUsers", "concurrency", "iterations", "mortalityPeriod", "finalityTimeoutMs", "maxElapsedMs", "wholeRunTimeoutMs", "shutdownDrainTimeoutMs", "maxConcurrentSamples", "fundingFinalityTimeoutMs", "fundingBatchSize"];
  for (const field of numbers) {
    const input = document.querySelector<HTMLInputElement>(`#${field}`);
    input?.addEventListener("input", () => {
      scenario[field] = numericValue(input.value);
      syncActiveThreadGroup();
      markDraftChanged();
    });
  }
	const fundDerivedUsers = document.querySelector<HTMLInputElement>("#fundDerivedUsers");
	fundDerivedUsers?.addEventListener("change", () => {
	  scenario.fundDerivedUsers = fundDerivedUsers.checked;
	  markDraftChanged();
	  render();
	});

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
  document.querySelector<HTMLButtonElement>("#tour-button")?.addEventListener("click", () => startTour());
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
	document.querySelector<HTMLButtonElement>("#export-jmx-button")?.addEventListener("click", () => void saveJmxFile());
	document.querySelector<HTMLButtonElement>("#import-jmx-button")?.addEventListener("click", () => {
		document.querySelector<HTMLInputElement>("#import-jmx-file")?.click();
	});
	document.querySelector<HTMLInputElement>("#import-jmx-file")?.addEventListener("change", (event) => {
		const file = (event.target as HTMLInputElement).files?.[0];
		if (file) void inspectJmxFile(file);
	});
	document.querySelector<HTMLButtonElement>("#store-signer-button")?.addEventListener("click", () => void storeSignerProfile());
	document.querySelector<HTMLButtonElement>("#remove-signer-button")?.addEventListener("click", () => void removeSignerProfile());
  document.querySelector<HTMLButtonElement>("#load-button")?.addEventListener("click", () => void openScenarioFile());
	document.querySelector<HTMLButtonElement>("#save-as-button")?.addEventListener("click", () => void saveScenarioAsFile());
  document.querySelector<HTMLButtonElement>("#open-report-button")?.addEventListener("click", () => void loadRunReport());
  document.querySelector<HTMLButtonElement>("#plan-menu-button")?.addEventListener("click", () => {
    planMenuOpen = !planMenuOpen;
    render();
  });
  document.querySelector<HTMLButtonElement>("#toggle-plan-panel")?.addEventListener("click", () => {
    planPanelCollapsed = !planPanelCollapsed;
    planMenuOpen = false;
    render();
  });
  document.querySelector<HTMLButtonElement>("#toggle-monitor-panel")?.addEventListener("click", () => {
    monitorPanelCollapsed = !monitorPanelCollapsed;
    render();
  });
  document.querySelectorAll<HTMLButtonElement>("[data-layout-action]").forEach((button) => {
    button.addEventListener("click", () => {
      switch (button.dataset.layoutAction) {
        case "toggle-plan": planPanelCollapsed = !planPanelCollapsed; break;
        case "toggle-monitor": monitorPanelCollapsed = !monitorPanelCollapsed; break;
        case "reset":
          planPanelCollapsed = false;
          monitorPanelCollapsed = false;
          planPanelWidth = PANEL_WIDTHS.plan.initial;
          monitorPanelWidth = PANEL_WIDTHS.monitor.initial;
          break;
      }
      planMenuOpen = false;
      render();
    });
  });
  document.querySelectorAll<HTMLButtonElement>("[data-plan-node]").forEach((button) => {
    button.addEventListener("click", () => {
      const selectedNode = button.dataset.planNode as PlanNode;
      activePlanNode = selectedNode;
      render();
      window.requestAnimationFrame(() => scrollToPlanSection(selectedNode));
    });
  });
  document.querySelectorAll<HTMLButtonElement>("[data-thread-group]").forEach((button) => {
    button.addEventListener("click", () => {
      activePlanNode = undefined;
      selectThreadGroup(button.dataset.threadGroup!);
      render();
      window.requestAnimationFrame(() => scrollToEditorSection("users-section"));
    });
  });
  document.querySelectorAll<HTMLElement>("[data-sampler-index]").forEach((element) => {
    element.addEventListener("click", (event) => {
      if ((event.target as HTMLElement).closest("[data-remove-phase]")) return;
      activePlanNode = undefined;
      const samplerGroup = element.dataset.samplerGroup;
      if (samplerGroup && samplerGroup !== activeThreadGroupId) selectThreadGroup(samplerGroup);
      else syncActiveThreadGroup();
      activeSamplerIndex = Number(element.dataset.samplerIndex); loadActiveSampler(); render();
      window.requestAnimationFrame(() => scrollToEditorSection("users-section"));
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
  document.removeEventListener("pointerdown", dismissPlanMenu, true);
  if (planMenuOpen) document.addEventListener("pointerdown", dismissPlanMenu, true);
  bindPanelResizers();
}

function dismissPlanMenu(event: PointerEvent): void {
  const target = event.target as HTMLElement | null;
  if (target?.closest(".panel-menu") || target?.closest("#plan-menu-button")) return;
  planMenuOpen = false;
  render();
}

function scrollToPlanSection(node: PlanNode): void {
  const section = node === "plan" ? "plan-section" : node === "connection" ? "connection-section" : "assertions-section";
  scrollToEditorSection(section);
}

function scrollToEditorSection(id: string): void {
  const editorScroll = document.querySelector<HTMLElement>(".editor-scroll");
  const section = document.getElementById(id);
  if (!editorScroll || !section) return;
  const top = section.getBoundingClientRect().top - editorScroll.getBoundingClientRect().top + editorScroll.scrollTop;
  editorScroll.scrollTo({ top: Math.max(0, top - 8), behavior: "smooth" });
}

function bindPanelResizers(): void {
  document.querySelectorAll<HTMLElement>("[data-resize-panel]").forEach((resizer) => {
    resizer.addEventListener("pointerdown", (event) => {
      if (resizer.classList.contains("disabled")) return;
      event.preventDefault();
      const panel = resizer.dataset.resizePanel;
      const pointerId = event.pointerId;
      resizer.setPointerCapture(pointerId);
      const resize = (move: PointerEvent) => {
        if (panel === "plan") {
          planPanelWidth = Math.max(PANEL_WIDTHS.plan.min, Math.min(PANEL_WIDTHS.plan.max, move.clientX));
        } else {
          monitorPanelWidth = Math.max(PANEL_WIDTHS.monitor.min, Math.min(PANEL_WIDTHS.monitor.max, window.innerWidth - move.clientX));
        }
        applyPanelLayout();
      };
      const finish = () => {
        window.removeEventListener("pointermove", resize);
        window.removeEventListener("pointerup", finish);
        window.removeEventListener("pointercancel", finish);
      };
      window.addEventListener("pointermove", resize);
      window.addEventListener("pointerup", finish, { once: true });
      window.addEventListener("pointercancel", finish, { once: true });
    });
  });
}

function applyPanelLayout(): void {
  const workspace = document.querySelector<HTMLElement>(".workspace");
  if (!workspace) return;
  workspace.style.setProperty("--plan-panel-width", `${planPanelCollapsed ? PANEL_WIDTHS.collapsed : planPanelWidth}px`);
  workspace.style.setProperty("--monitor-panel-width", `${monitorPanelCollapsed ? PANEL_WIDTHS.collapsed : monitorPanelWidth}px`);
}

async function loadRuntimeMetadata(): Promise<void> {
	metadataLoading = true;
	render();
	try {
		runtimeMetadata = await invoke<RuntimeMetadata>("fetch_runtime_metadata", { endpoint: scenario.endpoint.trim() });
		const selectedPallet = runtimeMetadata.pallets.find((item) => item.name.toLowerCase() === scenario.pallet.toLowerCase());
		if (!selectedPallet) scenario.pallet = runtimeMetadata.pallets[0]?.name ?? scenario.pallet;
		const calls = runtimeMetadata.pallets.find((item) => item.name === scenario.pallet)?.calls ?? [];
		if (!calls.some((item) => item.name.toLowerCase() === scenario.call.toLowerCase())) scenario.call = calls[0]?.name ?? scenario.call;
		syncActiveThreadGroup();
		markDraftChanged();
		showToast(`Loaded ${runtimeMetadata.pallets.length} pallets from the live runtime`);
	} catch (error) {
		showToast(`Could not load call fields: ${String(error)}`);
	} finally {
		metadataLoading = false;
		render();
	}
}

function updateFriendlyArgument(key: string, kind: "address" | "amount" | "boolean" | "json", raw: string): { valid: true } | { valid: false; error: string } {
	const argumentsValue = argumentObject();
	if (kind === "address") {
		if (!raw.trim()) return { valid: false, error: "Enter a recipient address." };
		const accountId = accountIdHex(raw);
		if (!accountId) return { valid: false, error: "Use an SS58 address or a 32-byte 0x account ID." };
		argumentsValue[key] = { $variant: "Id", value: { $bytes: accountId } };
	} else if (kind === "amount") {
		if (!/^\d+$/.test(raw.trim())) return { valid: false, error: "Enter a whole-number amount." };
		argumentsValue[key] = raw.trim();
	} else if (kind === "boolean") {
		argumentsValue[key] = raw === "true";
	} else {
		try {
			argumentsValue[key] = JSON.parse(raw);
		} catch {
			return { valid: false, error: `Enter valid JSON for ${humanize(key)}.` };
		}
	}
	scenario.argumentsJson = JSON.stringify(argumentsValue, null, 2);
	syncActiveThreadGroup();
	markDraftChanged();
	const rawEditor = document.querySelector<HTMLTextAreaElement>("#argumentsJson");
	if (rawEditor) rawEditor.value = scenario.argumentsJson;
	return { valid: true };
}

function accountIdHex(value: string): string | undefined {
	const input = value.trim();
	if (/^0x[\da-fA-F]{64}$/.test(input)) return `0x${input.slice(2).toLowerCase()}`;
	const alphabet = "123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";
	let decoded = 0n;
	for (const character of input) {
		const position = alphabet.indexOf(character);
		if (position < 0) return undefined;
		decoded = decoded * 58n + BigInt(position);
	}
	let hex = decoded.toString(16);
	if (hex.length % 2) hex = `0${hex}`;
	const leadingZeroes = input.match(/^1*/)?.[0].length ?? 0;
	hex = "00".repeat(leadingZeroes) + hex;
	const bytes = hex.match(/.{2}/g) ?? [];
	// The common one-byte SS58 prefix and two-byte checksum surround AccountId32.
	if (bytes.length !== 35 || Number.parseInt(bytes[0], 16) >= 64) return undefined;
	return `0x${bytes.slice(1, 33).join("")}`;
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
  if (!lastValidation?.valid) {
    scrollToEditorSection("preflight-section");
    return;
  }
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
  scrollToEditorSection("preflight-section");
}

async function armOrStopRun(): Promise<void> {
  try {
	if (remoteRunnerEndpoint.trim() || (activeRemoteTarget && ["running", "arming", "stopping"].includes(runStatus.state))) {
		if (runStatus.state === "running" || runStatus.state === "arming") {
			if (!activeRemoteTarget || !runStatus.runId) throw new Error("remote run target is unavailable");
			runStatus = await invoke<RunStatus>("stop_remote_run", { target: activeRemoteTarget, runId: runStatus.runId });
		} else {
			await previewScenario();
			if (!lastValidation?.valid) return;
			activeRemoteTarget = { endpoint: remoteRunnerEndpoint.trim(), bearerToken: remoteRunnerToken };
			runStatus = await invoke<RunStatus>("start_remote_run", {
				target: activeRemoteTarget,
				document: nativeScenario(),
				runId: `remote-${Date.now()}`
			});
			runReport = undefined;
			liveSamples = [];
			remotePollFailures = 0;
			void pollRemoteRun();
		}
		render();
		return;
	}
    let intent = decideRunIntent(runStatus.state, lastPreflight);
    if (intent === "stop") {
      runStatus = await invoke<RunStatus>("stop_run");
    } else {
      if (intent === "blocked") {
        showToast("Preflighting the chain before arming");
        await runPreflight();
        intent = decideRunIntent(runStatus.state, lastPreflight);
        if (intent !== "arm") return;
      }
      if (!preflightRunId) throw new Error("preflight did not return an arming run ID");
      activeRemoteTarget = undefined;
      runStatus = await invoke<RunStatus>("start_run", { document: nativeScenario(), outputRoot: "target/polkameter-runs", runId: preflightRunId });
      runReport = undefined;
      liveSamples = [];
      showToast("Run armed");
    }
    render();
  } catch (error) {
    showToast(`Run failed to start: ${String(error)}`);
  }
}

async function loadRunReport(): Promise<void> {
  if (!runStatus.artifactDir) return;
  try {
		runReport = activeRemoteTarget && runStatus.runId
			? await invoke<DashboardReport>("read_remote_run_report", { target: activeRemoteTarget, runId: runStatus.runId })
			: await invoke<DashboardReport>("read_run_report", { artifactDir: runStatus.artifactDir });
    render();
    showToast("Run report loaded");
  } catch (error) {
    showToast(`Could not load run report: ${String(error)}`);
  }
}

async function pollRemoteRun(): Promise<void> {
	if (!activeRemoteTarget || !runStatus.runId) return;
	try {
		runStatus = await invoke<RunStatus>("get_remote_run_status", {
			target: activeRemoteTarget,
			runId: runStatus.runId
		});
		remotePollFailures = 0;
		render();
		if (["completed", "completed_with_failures", "stopped", "failed"].includes(runStatus.state)) {
			void loadRunReport();
			return;
		}
		window.setTimeout(() => void pollRemoteRun(), 1000);
	} catch (error) {
		remotePollFailures += 1;
		if (remotePollFailures >= 5) {
			showToast(`Remote run status unavailable: ${String(error)}`);
			return;
		}
		window.setTimeout(() => void pollRemoteRun(), 2000);
	}
}

function scenarioFilePath(): string {
  return `target/polkameter-scenarios/${scenario.name.toLowerCase().replace(/[^a-z0-9]+/g, "-").replace(/(^-|-$)/g, "") || "scenario"}.polkameter.xml`;
}

function jmxFilePath(): string {
	return scenarioFilePath().replace(/\.polkameter\.xml$/, ".jmx");
}

async function saveScenarioFile(): Promise<void> {
	if (!currentScenarioPath) {
		await saveScenarioAsFile();
		return;
	}
  try {
		const path = currentScenarioPath;
    await invoke("save_scenario", { document: nativeScenario(), path });
    const { signerSource: _signerSource, ...persisted } = scenario;
    localStorage.setItem("polkameter-scenario", JSON.stringify(persisted));
    showToast(`Saved redacted scenario to ${path}`);
  } catch (error) {
    showToast(`Could not save scenario: ${String(error)}`);
  }
}

async function saveScenarioAsFile(): Promise<void> {
	try {
		const path = await invoke<string | null>("save_scenario_as", { document: nativeScenario(), suggestedName: fileName(scenarioFilePath()) });
		if (!path) return;
		currentScenarioPath = path;
		const { signerSource: _signerSource, ...persisted } = scenario;
		localStorage.setItem("polkameter-scenario", JSON.stringify(persisted));
		render();
		showToast(`Saved XML test plan to ${path}`);
	} catch (error) {
		showToast(`Could not save test plan: ${String(error)}`);
	}
}

async function saveJmxFile(): Promise<void> {
	try {
		const path = jmxFilePath();
		await invoke("save_jmx", { document: nativeScenario(), path });
		showToast(`Saved structural JMX companion to ${path}`);
	} catch (error) {
		showToast(`Could not export JMX: ${String(error)}`);
	}
}

async function inspectJmxFile(file: File): Promise<void> {
	try {
		const report = await invoke<JmxImportReport>("import_jmx", { xml: await file.text() });
		const groups = report.threadGroups.length;
		const collectors = report.collectors.length;
		const note = report.diagnostics[0];
		showToast(`JMX: ${groups} thread group${groups === 1 ? "" : "s"}, ${collectors} collector${collectors === 1 ? "" : "s"}${note ? `. ${note}` : ""}`);
	} catch (error) {
		showToast(`Could not inspect JMX: ${String(error)}`);
	}
}

async function openScenarioFile(): Promise<void> {
  try {
    const opened = await invoke<{ path: string; document: NativeScenarioDocument } | null>("open_scenario_dialog");
    if (!opened) return;
    currentScenarioPath = opened.path;
    applyScenarioDocument(opened.document);
    showToast(`Opened ${fileName(currentScenarioPath)}. Signer material stays in the credential vault.`);
  } catch (error) {
    showToast(`Could not open test plan: ${String(error)}`);
  }
}

function applyScenarioDocument(document: NativeScenarioDocument): void {
    const group = document.threadGroups[0];
    const primary = group?.samplers.find((sampler) => sampler.phase === "transaction") ?? group?.samplers[0];
    if (!group || !primary) throw new Error("scenario has no editable thread group sampler");
    scenario = {
      name: document.testPlan.name,
	  endpoint: document.chain.endpoint,
	  prometheusEndpoint: document.chain.prometheusEndpoint ?? "",
      pallet: primary.pallet,
      call: primary.call,
      argumentsJson: JSON.stringify(primary.arguments, null, 2),
	  signerProfile: document.signerSource.profile,
      signerSource: "",
		fundDerivedUsers: Boolean(document.signerSource.funding),
		fundingAmount: document.signerSource.funding?.amount ?? initialScenario.fundingAmount,
		fundingFinalityTimeoutMs: document.signerSource.funding?.finalityTimeoutMs ?? initialScenario.fundingFinalityTimeoutMs,
		fundingBatchSize: document.signerSource.funding?.batchSize ?? initialScenario.fundingBatchSize,
		virtualUsers: group.users,
		concurrency: group.concurrency,
		iterations: group.iterations,
      arrival: group.arrival,
      completion: primary.completion,
      mortalityPeriod: primary.mortalityPeriod,
      finalityTimeoutMs: primary.finalityTimeoutMs,
      maxElapsedMs: primary.assertions.find((assertion) => assertion.kind === "max_elapsed")?.milliseconds ?? 0,
	  wholeRunTimeoutMs: document.testPlan.limits.wholeRunTimeoutMs,
	  shutdownDrainTimeoutMs: document.testPlan.limits.shutdownDrainTimeoutMs,
	  maxConcurrentSamples: document.testPlan.limits.maxConcurrentSamples ?? initialScenario.maxConcurrentSamples
    };
    threadGroups = document.threadGroups.map((loaded, index) => ({
      id: `group-${index + 1}`,
      name: loaded.name,
		virtualUsers: loaded.users,
		concurrency: loaded.concurrency,
		iterations: loaded.iterations,
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
}

function fileName(path: string): string {
	return path.split(/[\\/]/).at(-1) || path;
}

async function storeSignerProfile(): Promise<void> {
	if (!scenario.signerSource.trim()) {
		showToast("Enter a SURI to store it in the operating system credential vault");
		return;
	}
	try {
		await invoke("store_signer_profile", { profile: scenario.signerProfile, suri: scenario.signerSource });
		scenario.signerSource = "";
		markDraftChanged();
		render();
		showToast("Signer profile stored in the operating system credential vault");
	} catch (error) {
		showToast(`Could not store signer profile: ${String(error)}`);
	}
}

async function removeSignerProfile(): Promise<void> {
	try {
		await invoke("remove_signer_profile", { profile: scenario.signerProfile });
		scenario.signerSource = "";
		markDraftChanged();
		render();
		showToast("Signer profile removed from the operating system credential vault");
	} catch (error) {
		showToast(`Could not remove signer profile: ${String(error)}`);
	}
}

function formatDuration(value: number): string {
  if (value < 1000) return `${value} ms`;
  return `${(value / 1000).toFixed(value < 10000 ? 1 : 0)} s`;
}

function phaseLabel(phase: "setup" | "transaction" | "teardown"): string {
  return phase === "transaction" ? "Workflow" : phase.charAt(0).toUpperCase() + phase.slice(1);
}

function samplerLabelPhase(phase: EditablePhase): string {
	return phase === "transaction" ? "workflow" : phase;
}

function escapeHtml(value: string): string {
  return value.replace(/[&<>'"]/g, (character) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", "'": "&#039;", '"': "&quot;" })[character]!);
}

function showToast(message: string): void {
  toastMessage = message;
  toastVisible = true;
  if (toastTimer !== undefined) window.clearTimeout(toastTimer);
  toastTimer = window.setTimeout(() => {
    toastVisible = false;
    toastTimer = undefined;
    document.querySelector<HTMLDivElement>("#toast")?.classList.remove("visible");
  }, 2400);
  const toast = document.querySelector<HTMLDivElement>("#toast");
  if (toast) {
    toast.textContent = message;
    toast.classList.add("visible");
  }
}

function refreshIcons(): void {
  const iconMap = {
    Activity, Boxes, Braces, Cable, CheckCircle2, ChevronDown, CircleDot, CircleHelp, ClipboardList, Cpu, Flag, FolderOpen, Gauge, GitBranch, PanelLeftClose, PanelLeftOpen, PanelRightClose, PanelRightOpen, Play, RotateCcw, Save, ShieldCheck, Square, Timer, Users, Wrench, XCircle
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
maybeStartTour();

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
      renderLiveSections();
    }, 150);
  }
});
