import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import initialXml from "../examples/plugin-workflow.polkameter.xml?raw";

type Field = { schema: { type: string }; optional: boolean };
type Manifest = { operations: Record<string, { description: string; inputs: Record<string, Field> }> };
type Inspection = { plugins: Record<string, Manifest> };
type Status = { phase?:string; id:string; state: string; error?: string; outcome?: { artifact_dir: string; exit_code: number; error?: string } };

export function parsePluginXml(xml: string): Document {
  const document = new DOMParser().parseFromString(xml, "application/xml");
  if (document.querySelector("parsererror")) throw new Error("XML is not well formed");
  if (document.doctype || document.documentElement.tagName !== "polkameter-plan" || document.documentElement.namespaceURI !== "https://polkameter.dev/schema/plan") throw new Error("Expected a Polkameter plan");
  if (document.documentElement.getAttribute("version") !== "1") throw new Error("Expected plan format version 1");
  return document;
}

/** Mounts the plan editor and runner into `root`. */
export async function mountPluginWorkbench(root: HTMLElement): Promise<void> {
  const dialog = document.createElement("main");
  dialog.className = "plugin-workbench";
  dialog.innerHTML = `<header><h2>Polkameter</h2></header>
    <p>Compose preparation, custom operations and load in one plan. Installed plugins supply the operations.</p>
    <nav><button data-action="open">Open</button><button data-action="save">Save as</button><button data-action="inspect">Inspect steps</button><button data-action="preflight">Preflight</button><button data-action="run">Run</button><button data-action="stop">Stop</button></nav>
    <details><summary>Remote runner</summary><label>Agent endpoint<input data-field="endpoint" placeholder="https://runner.example"/></label><label>Agent token<input data-field="token" type="password" autocomplete="off"/></label></details>
    <div class="plugin-plan-columns"><section><label>Scenario XML<textarea data-field="xml" spellcheck="false"></textarea></label></section><section data-field="steps"><p>Inspect steps to load operation descriptions and edit inputs.</p></section></div>
    <p role="status" data-field="status">Ready</p><pre data-field="events" aria-label="Run events"></pre>`;
  root.append(dialog);
  const xml = dialog.querySelector<HTMLTextAreaElement>("[data-field=xml]")!;
  const status = dialog.querySelector<HTMLElement>("[data-field=status]")!;
  const events = dialog.querySelector<HTMLElement>("[data-field=events]")!;
  const steps = dialog.querySelector<HTMLElement>("[data-field=steps]")!;
  xml.value = initialXml;
  let running = false;
  let runId="";
  let runTarget: {endpoint:string;bearerToken:string} | null = null;
  const target = () => {
    const endpoint = dialog.querySelector<HTMLInputElement>("[data-field=endpoint]")!.value.trim();
    return endpoint ? { endpoint, bearerToken: dialog.querySelector<HTMLInputElement>("[data-field=token]")!.value } : null;
  };
  const append = (value: unknown) => { events.textContent = `${events.textContent}\n${JSON.stringify(value)}`.slice(-20000); };
  await listen("plugin-run-event", event => append(event.payload));
  window.setInterval(() => {
    if (!running) return;
    void invoke<Status>("get_plugin_status", {target:runTarget,runId}).then(result => {
      status.textContent = result.outcome ? `${result.state}. Artifacts: ${result.outcome.artifact_dir}${result.outcome.error ? `. ${result.outcome.error}` : ""}` : result.error ?? (result.phase ? `${result.state}: ${result.phase}` : result.state);
      running = result.state === "running";
      xml.disabled = running;
    }).catch(error => {status.textContent=String(error);});
  }, 500);
  function renderFields(inspection:Inspection):void {
    const tree = parsePluginXml(xml.value);
    steps.replaceChildren();
    for (const step of tree.querySelectorAll("step")) {
      const operation = step.getAttribute("use") ?? "";
      const split = operation.indexOf(".");
      const descriptor = inspection.plugins[operation.slice(0,split)]?.operations[operation.slice(split+1)];
      const section=document.createElement("fieldset"); const legend=document.createElement("legend");
      legend.textContent=`${step.getAttribute("id")} · ${operation}`;section.append(legend);
      if(descriptor){const description=document.createElement("p");description.textContent=descriptor.description;section.append(description);}
      for(const input of step.querySelectorAll(":scope > input")){
        const name=input.getAttribute("name")??"";
        const label=document.createElement("label");label.textContent=`${name} (${descriptor?.inputs[name]?.schema.type ?? "value"})`;
        const kind=document.createElement("select");
        for(const value of ["value","ref"]){const option=document.createElement("option");option.value=value;option.textContent=value==="ref"?"Output reference":"Literal value";kind.append(option);}
        kind.value=input.hasAttribute("ref")?"ref":"value";
        const field=document.createElement("input");field.value=input.getAttribute(kind.value)??"";
        const update=()=>{if(running)return;input.removeAttribute("ref");input.removeAttribute("value");input.setAttribute(kind.value,field.value);xml.value=new XMLSerializer().serializeToString(tree);};
        kind.addEventListener("change",update);field.addEventListener("change",update);
        label.append(kind,field);section.append(label);
      }
      steps.append(section);
    }
  }
  xml.addEventListener("input",()=>{steps.textContent="Inspect steps again to refresh the input editor.";});
  dialog.addEventListener("click", event => {
    const action=(event.target as HTMLElement).closest<HTMLButtonElement>("[data-action]")?.dataset.action;
    if(!action)return;
    void (async()=>{
      if(running && action!=="stop"){status.textContent="Stop the running plan before editing it.";return;}
      if(action==="open"){const loaded=await invoke<string|null>("open_plugin_plan");if(loaded){xml.value=loaded;steps.textContent="Inspect steps to edit inputs.";}return;}
      if(action==="save"){const path=await invoke<string|null>("save_plugin_plan",{xml:xml.value});if(path)status.textContent=`Saved ${path}`;return;}
      parsePluginXml(xml.value);
      if(action==="inspect"){const inspection=await invoke<Inspection>("inspect_plugin_plan",{xml:xml.value,target:target()});renderFields(inspection);status.textContent="Installed operations and input contracts validated.";}
      if(action==="preflight"){await invoke("preflight_plugin_plan",{xml:xml.value,target:target()});status.textContent="Preflight passed.";}
      if(action==="run"){runTarget=target();const result=await invoke<Status>("start_plugin_plan",{xml:xml.value,target:runTarget});runId=result.id;running=result.state==="running";xml.disabled=running;status.textContent=result.phase ? `${result.state}: ${result.phase}` : result.state;}
      if(action==="stop"){await invoke("stop_plugin_plan",{target:runTarget,runId});status.textContent="Stopping and cleaning up…";}
    })().catch(error=>{status.textContent=String(error);});
  });
}
