// @vitest-environment jsdom
import {describe,expect,it,vi} from "vitest";
vi.mock("@tauri-apps/api/core",()=>({invoke:vi.fn()}));
vi.mock("@tauri-apps/api/event",()=>({listen:vi.fn(async()=>()=>{})}));
import {invoke} from "@tauri-apps/api/core";
import {openPluginWorkbench,parsePluginXml} from "./plugin-plans";
import xml from "../examples/plugin-workflow.polkameter.xml?raw";

describe("plugin XML editor",()=>{
  it("preserves nested steps and output references when XML is serialized",()=>{
    const tree=parsePluginXml(xml.replace("<setup>", '<!-- keep this note --><setup data-review="preserve">'));
    const roundTrip=parsePluginXml(new XMLSerializer().serializeToString(tree));
    expect(roundTrip.querySelectorAll("workflow > step").length).toBe(tree.querySelectorAll("workflow > step").length);
    expect(roundTrip.querySelector('input[ref="steps.seed.value"]')).not.toBeNull();
    expect(new XMLSerializer().serializeToString(roundTrip)).toContain("<!-- keep this note -->");
    expect(roundTrip.querySelector("setup")?.getAttribute("data-review")).toBe("preserve");
    // Host contract validation still rejects unsupported attributes; editing must not erase them.
  });
  it("rejects a foreign root, namespace, version and malformed document",()=>{
    for(const value of [xml.replaceAll("polkameter-plan","unknown"),xml.replace("schema/plan/v2","schema/plan/v9"),xml.replace('version="2"','version="1"'),xml.slice(0,-20)]) expect(()=>parsePluginXml(value)).toThrow();
  });
  it("opens the manifest editor and invalidates stale fields after XML edits",async()=>{
    HTMLDialogElement.prototype.showModal=function(){this.open=true;};
    HTMLDialogElement.prototype.close=function(){this.open=false;this.dispatchEvent(new Event("close"));};
    vi.mocked(invoke).mockResolvedValue({plugins:{example:{operations:{double:{description:"Double",inputs:{value:{schema:{type:"integer"}}}}}}}});
    await openPluginWorkbench();
    const dialog=document.querySelector("dialog")!;
    (dialog.querySelector('[data-action="inspect"]') as HTMLButtonElement).click();
    await vi.waitFor(()=>expect(dialog.querySelectorAll("fieldset").length).toBeGreaterThan(0));
    const editor=dialog.querySelector("textarea")!;
    editor.value=xml.replace('value="21"','value="22"');
    editor.dispatchEvent(new Event("input"));
    expect(dialog.querySelectorAll("fieldset").length).toBe(0);
    expect(editor.value).toContain('value="22"');
    dialog.close();
  });
});
