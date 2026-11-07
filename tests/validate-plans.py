#!/usr/bin/env python3
"""Check examples, full docs plans, and validator/schema boundaries. Run at the repo root.

Requires python3, xmllint (CI installs libxml2-utils), and a built CLI:
    cargo +1.93.0 build -p polkameter --bin polkameter
    python3 tests/validate-plans.py
Set POLKAMETER to use another CLI binary. No plugins or chain are needed.
"""

from html.parser import HTMLParser
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import tempfile
import unittest
import xml.etree.ElementTree as ET

ROOT = Path(__file__).resolve().parents[1]
NAMESPACE = "https://polkameter.dev/schema/plan"
ET.register_namespace("", NAMESPACE)
SCHEMA = ROOT / "schemas/polkameter-plan.xsd"
CLI = Path(os.environ.get("POLKAMETER", ROOT / "target/debug/polkameter")).resolve()
SKILL = ROOT / "skills/polkameter"


def plan(children="", attrs='version="1" name="test"'):
    return f'<polkameter-plan xmlns="{NAMESPACE}" {attrs}>{children}</polkameter-plan>'


FULL = plan('''
  <plugins><plugin id="example" version="0.1.0" protocol="1"/></plugins>
  <targets><target id="chain" endpoint="ws://127.0.0.1:9944"/></targets>
  <credentials><credential id="admin" profile="sudo"/></credentials>
  <setup><step id="prepare" use="example.prepare">
    <input name="value" value="21"/>
  </step></setup>
  <workflow id="users"><step id="echo" use="core.echo"><input name="value" ref="user.index"/></step></workflow>
  <load target="chain" source-ref="steps.prepare.flood" probes-ref="steps.prepare.probes">
    <rate tx-per-second="0" seconds="1"/>
  </load>
  <monitors topology="local" relay-target="chain" para-id="0">
    <metric role="collator" name="substrate_block_height"/>
  </monitors>
  <thresholds/>
''')


def mutate(xml, path, attribute, value):
    root = ET.fromstring(xml)
    node = root if path == "." else root.find(path, {"p": NAMESPACE})
    assert node is not None, path
    if value is None:
        del node.attrib[attribute]
    else:
        node.set(attribute, value)
    return ET.tostring(root, encoding="unicode")


class CodeBlocks(HTMLParser):
    """Read rendered XML in the slides, including text split across syntax spans."""

    def __init__(self):
        super().__init__(convert_charrefs=True)
        self.blocks = []
        self.current = None

    def handle_starttag(self, tag, attrs):
        if tag == "pre":
            self.current = []

    def handle_data(self, data):
        if self.current is not None:
            self.current.append(data)

    def handle_endtag(self, tag):
        if tag == "pre" and self.current is not None:
            self.blocks.append("".join(self.current))
            self.current = None


def published_plans():
    for path in sorted((ROOT / "examples").rglob("*.polkameter.xml")):
        yield str(path.relative_to(ROOT)), path.read_text()
    for path in sorted((SKILL / "templates").glob("*.polkameter.xml")):
        yield str(path.relative_to(ROOT)), path.read_text()
    for path in sorted((ROOT / "docs").rglob("*")):
        if path.suffix == ".md":
            # Accept both Markdown fence styles, including indented list fences.
            blocks = re.findall(
                r"^\s*(`{3,}|~{3,})[^\n]*\n(.*?)^\s*\1\s*$",
                path.read_text(), re.MULTILINE | re.DOTALL,
            )
            blocks = [body for _, body in blocks]
        elif path.suffix == ".html":
            parser = CodeBlocks()
            parser.feed(path.read_text())
            blocks = parser.blocks
        else:
            continue
        for index, body in enumerate(blocks, 1):
            # A plan starts with markup; CLI output that quotes an element name is not one.
            if body.lstrip().startswith("<") and re.search(r"<polkameter-plan(?:\s|/?>)", body):
                yield f"{path.relative_to(ROOT)} block {index}", body.strip()


class PlanValidation(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        if not shutil.which("xmllint"):
            raise RuntimeError("xmllint is required; install libxml2-utils in CI")
        if not CLI.is_file():
            raise RuntimeError(f"build the CLI first: {CLI}")
        cls.work = tempfile.TemporaryDirectory(prefix="polkameter-schema-")
        cls.addClassCleanup(cls.work.cleanup)

    def check_plan(self, xml, schema_valid=True, cli_valid=True, *,
                   cli_error=None, schema_error=None):
        path = Path(self.work.name) / "plan.polkameter.xml"
        path.write_text(xml)
        schema = subprocess.run(
            ["xmllint", "--nonet", "--noout", "--schema", str(SCHEMA), str(path)],
            capture_output=True, text=True, timeout=30,
        )
        cli = subprocess.run(
            [str(CLI), "validate", str(path), "--format", "json"],
            capture_output=True, text=True, timeout=30,
        )
        self.assertEqual(schema.returncode, 0 if schema_valid else 3, schema.stderr)
        self.assertEqual(cli.returncode, 0 if cli_valid else 2, cli.stdout + cli.stderr)
        result = json.loads(cli.stdout)
        self.assertEqual(result["schemaVersion"], 1)
        if cli_valid:
            self.assertEqual(result["event"], "validation")
            self.assertIs(result["valid"], True)
        else:
            # The diagnostic goes to stderr and, with --format json, as an error line to stdout.
            self.assertIn(cli_error, cli.stderr)
            self.assertEqual(result["event"], "error")
            self.assertIs(result["valid"], False)
            self.assertEqual(result["exitCode"], 2)
            self.assertEqual(result["file"], str(path))
            self.assertIn(cli_error, result["message"])
            self.assertTrue(result["message"].startswith(f"{path}: "), result["message"])
        if not schema_valid:
            for fragment in schema_error:
                self.assertIn(fragment, schema.stderr)


class SkillTemplates(unittest.TestCase):
    """The skill's plan templates are copies of the example plans, so the plans CI runs are the
    ones an assistant copies."""

    def test_plan_templates_equal_their_examples(self):
        templates = sorted((SKILL / "templates").glob("*.polkameter.xml"))
        self.assertTrue(templates, "no plan templates in the skill")
        for template in templates:
            with self.subTest(template.name):
                example = ROOT / "examples" / template.name
                self.assertTrue(example.is_file(), f"examples/{template.name} is missing")
                self.assertEqual(template.read_text(), example.read_text(),
                                 f"copy examples/{template.name} over {template.relative_to(ROOT)}")

    def test_dev_chain_page_shows_the_ci_templates(self):
        page = (ROOT / "docs/dev-chain.md").read_text()
        for name in ["github-workflow.yml", "gitlab-ci.yml"]:
            lines = (SKILL / "templates" / name).read_text().splitlines()
            body = "\n".join(lines[next(i for i, l in enumerate(lines) if not l.startswith("#")):])
            with self.subTest(name):
                self.assertIn(body, page, f"docs/dev-chain.md must show templates/{name} without its header")

    def test_claude_code_plugin_follows_the_workspace_version(self):
        manifest = json.loads((ROOT / ".claude-plugin/plugin.json").read_text())
        marketplace = json.loads((ROOT / ".claude-plugin/marketplace.json").read_text())
        version = re.search(r'^version = "([^"]+)"', (ROOT / "Cargo.toml").read_text(), re.MULTILINE)[1]
        self.assertEqual(manifest["version"], version, "bump .claude-plugin/plugin.json with the workspace")
        entry = next(p for p in marketplace["plugins"] if p["name"] == manifest["name"])
        self.assertEqual(entry["source"], "./")
        self.assertTrue((ROOT / "skills" / manifest["name"] / "SKILL.md").is_file())

    def test_skill_links_resolve(self):
        for document in [SKILL / "SKILL.md", SKILL / "checklist.md"]:
            for target in re.findall(r"\]\(((?:templates/)?[\w.-]+\.(?:md|xml|yml|rs))\)",
                                     document.read_text()):
                with self.subTest(f"{document.name} -> {target}"):
                    self.assertTrue((SKILL / target).is_file(), target)


COUNTS = {"published": 0, "agreement": 0, "cli_only": 0}


def case(name, xml, schema_valid=True, cli_valid=None, category="agreement", *,
         cli_error=None, schema_error=None):
    if cli_valid is None:
        cli_valid = schema_valid
    assert cli_valid == (cli_error is None), f"{name}: specify the CLI rejection reason"
    assert schema_valid == (schema_error is None), f"{name}: specify the XSD rejection reason"
    COUNTS[category] += 1

    def test(self):
        self.check_plan(xml, schema_valid, cli_valid,
                        cli_error=cli_error, schema_error=schema_error)

    test.__doc__ = name
    if category == "cli_only":
        test.__doc__ += (
            ". Pins intentional XSD 1.0 permissiveness; tightening the schema "
            "later is an expected change."
        )
    # unittest accepts punctuation and spaces; preserve the complete, stable label.
    test_name = f"test_{category}: {name}"
    assert not hasattr(PlanValidation, test_name), f"duplicate case label: {name}"
    setattr(PlanValidation, test_name, test)


def schema_attribute(path, attribute):
    element = "polkameter-plan" if path == "." else path.rsplit("p:", 1)[-1]
    return (f"Element '{{{NAMESPACE}}}{element}'", f"attribute '{attribute}'")


def scalar_case(label, xml, path, attribute, value, valid, reason):
    case(label, xml, valid, cli_error=None if valid else reason,
         schema_error=None if valid else schema_attribute(path, attribute))


for label, xml in published_plans():
    case(label, xml, category="published")
assert COUNTS["published"], "no published plans discovered"

case("minimal plan and defaults", plan())
case("full plan, zero para ID, zero rate, and defaults", FULL)
case("smoke mode", plan(attrs='version="1" name="test" mode="smoke"'))
reversed_plan = ET.fromstring(FULL)
reversed_plan[:] = reversed(list(reversed_plan))
case("sections in reverse order", ET.tostring(reversed_plan, encoding="unicode"))
case("workflows interleaved with sections", plan('''
  <workflow id="a"/><targets/><workflow id="b"/><setup/><workflow id="c"/>
'''))

# Every required attribute is checked with both validators, including nested elements.
for path, attributes in [
    (".", ["version", "name"]),
    ("p:plugins/p:plugin", ["id", "version", "protocol"]),
    ("p:targets/p:target", ["id", "endpoint"]),
    ("p:credentials/p:credential", ["id", "profile"]),
    ("p:setup/p:step", ["id", "use"]),
    ("p:setup/p:step/p:input", ["name"]),
    ("p:workflow", ["id"]),
    ("p:load", ["target", "source-ref", "probes-ref"]),
    ("p:load/p:rate", ["tx-per-second", "seconds"]),
    ("p:monitors", ["topology", "relay-target", "para-id"]),
    ("p:monitors/p:metric", ["role", "name"]),
]:
    for attribute in attributes:
        case(f"required {path}/@{attribute}", mutate(FULL, path, attribute, None), False,
             cli_error=f"missing `{attribute}` on <{'polkameter-plan' if path == '.' else path.rsplit('p:', 1)[-1]}>",
             schema_error=schema_attribute(path, attribute) + ("required but missing",))

# A good and a bad value for each bounded attribute, and each scalar enum/pattern.
for path, attribute, good, bad, reason in [
    (".", "version", ["1", " 1 "], ["2"],
     'unsupported plan version'),
    (".", "mode", ["stress", "smoke"], ["", "other", " smoke"],
     '`mode` on <polkameter-plan> must be smoke or stress'),
    (".", "name", [" "], [""],
     '`name` on <polkameter-plan> must not be empty'),
    (".", "timeout-ms", ["1"], ["0"],
     '`timeout-ms` on <polkameter-plan> must be at least 1'),
    ("p:plugins/p:plugin", "id", ["a", "cor", "Core", "corex", "-0_"], ["", "core", "bad.id", "é", "x y"],
     {"": "plugin ID `` is invalid", "core": "plugin ID `core` is reserved for built-in operations",
      "bad.id": "plugin ID `bad.id` is invalid", "é": "plugin ID `é` is invalid", "x y": "plugin ID `x y` is invalid"}),
    ("p:plugins/p:plugin", "version", [" "], [""],
     'plugin `example` needs a `version`'),
    ("p:plugins/p:plugin", "protocol", ["1"], ["0", "2"],
     'plugin `example` uses protocol'),
    ("p:targets/p:target", "endpoint", ["ws://", "wss://node", "ws://x y"], ["https://node", "WS://node", " ws://node"],
     'a target must be a WebSocket RPC URL'),
    ("p:credentials/p:credential", "profile", [" "], [""],
     'credential `admin` needs a `profile`'),
    ("p:setup/p:step", "use", ["example.any-name_0"], ["core", "core.a.b", "core."],
     {'core': 'step `prepare`: use="core" must name an operation as <plugin>.<operation>',
      'core.a.b': 'step `prepare`: use="core.a.b" has an invalid operation name `a.b`',
      'core.': 'step `prepare`: use="core." has an invalid operation name ``'}),
    ("p:setup/p:step", "timeout-ms", ["1"], ["0"],
     'step `prepare`: `timeout-ms` must be at least 1'),
    ("p:workflow", "users", ["100000"], ["100001"],
     'workflow `users` has users='),
    ("p:workflow", "iterations", ["1"], ["0"],
     'workflow `users` has iterations="0"; use at least 1'),
    ("p:workflow", "concurrency", ["1000"], ["1001"],
     'workflow `users` has concurrency='),
    ("p:load", "connections", ["64"], ["65"],
     '<load> has connections='),
    ("p:load", "baseline-probes", ["1000"], ["1001"],
     '<load> has baseline-probes='),
    ("p:load", "recovery-seconds", ["86400"], ["86401"],
     '<load> has recovery-seconds='),
    ("p:load", "block-interval-seconds", ["0.1"], ["0.09", "NaN"],
     '<load> has block-interval-seconds='),
    ("p:load/p:rate", "tx-per-second", ["0"], ["-1", "NaN"],
     'step 0: 1 s at'),
    ("p:load/p:rate", "seconds", ["1"], ["0"],
     'step 0: 0 s at'),
    ("p:monitors", "para-id", ["0", "2000"], ["-1"],
     '`para-id` on <monitors> must be a whole number from 0 to'),
]:
    for valid, values in [(True, good), (False, bad)]:
        for value in values:
            xml = mutate(FULL, path, attribute, value)
            if path == "p:plugins/p:plugin" and attribute == "id" and valid:
                xml = xml.replace('use="example.prepare"', f'use="{value}.prepare"')
            scalar_case(f"{path}/@{attribute}={value!r}", xml, path, attribute, value, valid,
                        reason[value] if isinstance(reason, dict) and not valid else reason)

for attribute in ["stall-ms", "finality-stall-ms", "max-p95-latency-ms", "max-submit-reply-ms", "finality-wait-ms", "probes-in-a-row"]:
    for value, valid in [("1", True), ("0", False)]:
        scalar_case(f"threshold {attribute}={value}",
                    mutate(FULL, "p:thresholds", attribute, value),
                    "p:thresholds", attribute, value, valid, f"<thresholds> has {attribute}=")
for attribute in ["min-included-ratio", "max-refused-ratio", "pool-refuses-ratio", "min-send-ratio", "max-block-gap-factor", "recovered-block-gap-factor"]:
    values = [("1", True), ("0", False), ("NaN", False)]
    values += [("1.1", attribute.endswith("factor"))]
    for value, valid in values:
        scalar_case(f"threshold {attribute}={value}",
                    mutate(FULL, "p:thresholds", attribute, value),
                    "p:thresholds", attribute, value, valid, f"<thresholds> has {attribute}=")

RAMP = FULL.replace('<rate tx-per-second="0" seconds="1"/>', '<ramp start="2" step="-1" steps="3" seconds="1"/>')
case("descending ramp reaches zero", RAMP)
case("geometric ramp", RAMP.replace('step="-1"', 'growth="2"'))
for attribute in ["start", "steps", "seconds"]:
    case(f"ramp requires {attribute}", mutate(RAMP, "p:load/p:ramp", attribute, None), False,
         cli_error=f"missing `{attribute}` on <ramp>",
         schema_error=schema_attribute("p:load/p:ramp", attribute) + ("required but missing",))
for attribute, values in [
    ("start", ["0", "-1", "INF", "NaN"]),
    ("step", ["INF", "NaN"]),
    ("growth", ["0", "-1", "INF", "NaN"]),
    ("steps", ["0", "10001"]),
    ("seconds", ["0"]),
]:
    for value in values:
        base = RAMP.replace('step="-1"', 'growth="2"') if attribute == "growth" else RAMP
        reason = {
            "start": "<ramp> start must be", "step": "step 0: 1 s at NaN tx/s",
            "growth": "<ramp> growth must be", "steps": "<ramp> steps must be at least 1" if value == "0" else "a ramp has at most 10000 steps",
            "seconds": "step 0: 0 s at",
        }[attribute]
        scalar_case(f"ramp {attribute}={value}", mutate(base, "p:load/p:ramp", attribute, value),
                    "p:load/p:ramp", attribute, value, False, reason)
case("10000 ramp steps", RAMP.replace('step="-1" steps="3"', 'step="0" steps="10000"'))
for count, valid in [(0, False), (10000, True), (10001, False)]:
    case(f"{count} rate elements", FULL.replace('<rate tx-per-second="0" seconds="1"/>', '<rate tx-per-second="0" seconds="1"/>' * count), valid,
         cli_error=None if valid else "<load> needs at least one <rate> or a <ramp>" if count == 0 else "<load> has 10001 <rate> steps; use at most 10000",
         schema_error=None if valid else (f"Element '{{{NAMESPACE}}}load'", "Missing child element") if count == 0
         else (f"Element '{{{NAMESPACE}}}rate'", "not expected"))
case("rates and ramp are exclusive elements", FULL.replace('</load>', '<ramp start="2" step="1" steps="1" seconds="1"/></load>'), False,
     cli_error="use rates or a ramp, not both", schema_error=(f"Element '{{{NAMESPACE}}}ramp'", "not expected"))
case("unknown attribute", mutate(FULL, ".", "unknown", "x"), False,
     cli_error="unknown attribute `unknown` on <polkameter-plan>",
     schema_error=schema_attribute(".", "unknown") + ("not allowed",))
case("unknown child", plan('<unknown/>'), False,
     cli_error="unknown element <unknown> in <polkameter-plan>",
     schema_error=(f"Element '{{{NAMESPACE}}}unknown'", "not expected"))
for tag in ["plugin", "target", "credential", "workflow", "step"]:
    root = ET.fromstring(FULL)
    node = root.find(f".//{{{NAMESPACE}}}{tag}")
    parent = next(parent for parent in root.iter() if node in list(parent))
    parent.append(ET.fromstring(ET.tostring(node)))
    reason = {"plugin": "plugin ID `example` is declared twice", "target": "target ID `chain` is declared twice",
              "credential": "credential ID `admin` is declared twice", "workflow": "workflow ID `users` is declared twice",
              "step": "step ID `prepare` is declared twice"}[tag]
    case(f"duplicate {tag} ID", ET.tostring(root, encoding="unicode"), False,
         cli_error=reason, schema_error=(f"Element '{{{NAMESPACE}}}{tag}'", "Duplicate key-sequence"))
case("step IDs are global", FULL.replace('<workflow id="users">', '<workflow id="users"><step id="prepare" use="core.echo"/>'), False,
     cli_error="step ID `prepare` is declared twice; step IDs must be unique in the whole plan",
     schema_error=(f"Element '{{{NAMESPACE}}}step'", "Duplicate key-sequence"))
case("duplicate input name", FULL.replace('<input name="value" value="21"/>', '<input name="value" value="21"/>' * 2), False,
     cli_error="step `prepare` (example.prepare): input `value` is given twice", schema_error=(f"Element '{{{NAMESPACE}}}input'", "Duplicate key-sequence"))

# These gaps are intentional XSD 1.0 limitations, documented in the schema/reference.
for label, xml, reason in [
    ("ramp has neither step nor growth", RAMP.replace(' step="-1"', ''), "ramp needs exactly one of step or growth"),
    ("ramp has both step and growth", RAMP.replace('step="-1"', 'step="-1" growth="2"'), "ramp needs exactly one of step or growth"),
    ("input has neither ref nor value", FULL.replace(' value="21"', ''), "<input> needs exactly one of ref or value"),
    ("input has both ref and value", FULL.replace('value="21"', 'value="21" ref="run.id"'), "<input> needs exactly one of ref or value"),
    ("state-check without state-ref", mutate(FULL, "p:load", "state-check", "example.check"), "state-check and state-ref must be given together"),
    ("state-ref without state-check", mutate(FULL, "p:load", "state-ref", "steps.prepare.state"), "state-check and state-ref must be given together"),
    ("expanded ramp goes negative", RAMP.replace('step="-1"', 'step="-2"'), "step 2: 1 s at -2 tx/s"),
    ("expanded ramp overflows", RAMP.replace('step="-1"', 'growth="1e308"'), "step 1: 1 s at inf tx/s"),
    ("unknown operation provider", mutate(FULL, "p:setup/p:step", "use", "missing.prepare"), 'use="missing.prepare" names plugin `missing`, which <plugins> does not declare'),
    ("unknown reference", mutate(FULL, "p:load", "source-ref", "steps.missing.flood"), "<load> source-ref is `steps.missing.flood`, but nothing in the plan defines it"),
    ("forward reference", FULL.replace('value="21"', 'ref="steps.echo.value"'), "step `prepare` (example.prepare): input `value` refers to `steps.echo.value`, but step `echo` does not run before this step"),
    ("monitors without load", re.sub(r'<load\b.*?</load>', '', FULL, flags=re.DOTALL), "<monitors> needs a <load> in the same plan"),
]:
    case(label, xml, True, False, "cli_only", cli_error=reason)
for section in ["plugins", "targets", "credentials", "preflight", "setup", "load", "monitors", "thresholds", "evaluate", "teardown"]:
    root = ET.fromstring(FULL)
    node = root.find(f"{{{NAMESPACE}}}{section}")
    if node is None:
        node = ET.SubElement(root, f"{{{NAMESPACE}}}{section}")
        step = ET.SubElement(node, f"{{{NAMESPACE}}}step", id=f"{section}-step", use="core.echo")
        ET.SubElement(step, f"{{{NAMESPACE}}}input", name="value", value="1")
    case(f"single {section} before duplication", ET.tostring(root, encoding="unicode"))
    root.append(ET.fromstring(ET.tostring(node)))
    # Avoid duplicate-ID errors, so this exercises the singleton check specifically.
    for descendant in root[-1].iter():
        if "id" in descendant.attrib:
            descendant.set("id", "duplicate-section-" + descendant.get("id"))
    case(f"singleton {section}", ET.tostring(root, encoding="unicode"), True, False, "cli_only",
         cli_error=f"<{section}> may appear once in <polkameter-plan>")
case("state-check and state-ref together", mutate(mutate(FULL, "p:load", "state-check", "example.check"), "p:load", "state-ref", "steps.prepare.state"))
case("metric role checked only at preflight", mutate(FULL, "p:monitors/p:metric", "role", "other"))
case("undeclared load target", mutate(FULL, "p:load", "target", "missing"), False,
     cli_error="<load> target `missing` is not a declared <target>", schema_error=(f"Element '{{{NAMESPACE}}}load'", "No match found for key-sequence"))
case("undeclared relay target", mutate(FULL, "p:monitors", "relay-target", "missing"), False,
     cli_error="<monitors> relay-target `missing` is not a declared <target>", schema_error=(f"Element '{{{NAMESPACE}}}monitors'", "No match found for key-sequence"))

if __name__ == "__main__":
    print(f"Plan cases: {COUNTS}", flush=True)
    unittest.main(verbosity=2)
