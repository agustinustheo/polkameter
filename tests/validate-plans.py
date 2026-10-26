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


def plan(children="", attrs='version="1" name="test"'):
    return f'<polkameter-plan xmlns="{NAMESPACE}" {attrs}>{children}</polkameter-plan>'


FULL = plan('''
  <plugins><plugin id="example" version="0.1.0" protocol="1"/></plugins>
  <targets><target id="chain" endpoint="ws://127.0.0.1:9944"/></targets>
  <credentials><credential id="admin" profile="sudo"/></credentials>
  <setup><step id="prepare" use="example.prepare">
    <input name="value" value="21"/>
  </step></setup>
  <workflow id="users"><step id="echo" use="core.echo"/></workflow>
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
            if re.search(r"<polkameter-plan(?:\s|/?>)", body):
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
        if cli_valid:
            self.assertIs(json.loads(cli.stdout)["valid"], True)
        else:
            # --format json currently formats successes only; errors have no JSON
            # code/path fields. Pin the diagnostic until the CLI provides them.
            self.assertIn(cli_error, cli.stderr)
        if not schema_valid:
            for fragment in schema_error:
                self.assertIn(fragment, schema.stderr)


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


def scalar_case(label, xml, path, attribute, value, valid, reason, integer_bits=None):
    """Distinguish integer parse failures from in-range values violating a rule."""
    if not valid and integer_bits is not None:
        if value.startswith("-") or int(value) >= 2 ** integer_bits:
            reason = f"invalid `{attribute}`:"
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

# Values at and beyond all bounded integer ranges, and each scalar enum/pattern.
for path, attribute, good, bad, reason, integer_bits in [
    (".", "version", ["1", "+1", " 1 "], ["0", "2", "4294967296"],
     'expected a version 1 Polkameter plan in its namespace', 32),
    (".", "mode", ["stress", "smoke"], ["", "other", " smoke"],
     'invalid `mode`: must be smoke or stress', None),
    (".", "name", [" "], [""],
     'name and run deadline required', None),
    (".", "timeout-ms", ["1", "+1", "18446744073709551615"], ["0", "-1", "18446744073709551616"],
     'name and run deadline required', 64),
    ("p:plugins/p:plugin", "id", ["a", "cor", "Core", "corex", "-0_"], ["", "core", "bad.id", "é", "x y"],
     'duplicate or invalid plugin ID', None),
    ("p:plugins/p:plugin", "version", [" "], [""],
     'unsupported plugin protocol or empty version', None),
    ("p:plugins/p:plugin", "protocol", ["1"], ["0", "2"],
     'unsupported plugin protocol or empty version', 32),
    ("p:targets/p:target", "endpoint", ["ws://", "wss://node", "ws://x y"], ["https://node", "WS://node", " ws://node"],
     'target must be WebSocket RPC', None),
    ("p:credentials/p:credential", "profile", [" "], [""],
     'duplicate or invalid credential', None),
    ("p:setup/p:step", "use", ["core.any-name_0"], ["core", "core.a.b", "core."],
     {'core': 'operation must be plugin.name', 'core.a.b': 'unknown operation provider core', 'core.': 'unknown operation provider core'}, None),
    ("p:setup/p:step", "timeout-ms", ["1", "18446744073709551615"], ["0", "18446744073709551616"],
     'invalid or duplicate step prepare', 64),
    ("p:workflow", "users", ["1", "100000"], ["0", "100001"],
     'invalid workflow limits', 32),
    ("p:workflow", "iterations", ["1", "4294967295"], ["0", "4294967296"],
     'invalid workflow limits', 32),
    ("p:workflow", "concurrency", ["1", "1000"], ["0", "1001"],
     'invalid workflow limits', 32),
    ("p:load", "connections", ["1", "64"], ["0", "65"],
     'invalid load limits', 32),
    ("p:load", "baseline-probes", ["1", "1000"], ["0", "1001"],
     'invalid load limits', 32),
    ("p:load", "recovery-seconds", ["0", "+0", "86400"], ["-0", "-1", "86401"],
     'invalid load limits', 32),
    ("p:load", "block-interval-seconds", ["0.1", "1.7976931348623157E308"], ["0.09", "INF", "NaN"],
     'invalid expected block interval', None),
    ("p:load/p:rate", "tx-per-second", ["0", "1.7976931348623157E308"], ["-1", "INF", "-INF", "NaN"],
     'step 0: 1 s at', None),
    ("p:load/p:rate", "seconds", ["1", "4294967295"], ["0", "4294967296"],
     'step 0: 0 s at', 32),
    ("p:monitors", "para-id", ["0", "+0", "1", "999", "1000", "1999", "2000", "4294967295"], ["-0", "-1", "4294967296"],
     'invalid `para-id`:', 32),
]:
    for valid, values in [(True, good), (False, bad)]:
        for value in values:
            xml = mutate(FULL, path, attribute, value)
            if path == "p:plugins/p:plugin" and attribute == "id" and valid:
                xml = xml.replace('use="example.prepare"', f'use="{value}.prepare"')
            scalar_case(f"{path}/@{attribute}={value!r}", xml, path, attribute, value, valid,
                        reason[value] if isinstance(reason, dict) and not valid else reason,
                        integer_bits)

for attribute in ["stall-ms", "finality-stall-ms", "max-p95-latency-ms", "max-submit-reply-ms", "finality-wait-ms", "probes-in-a-row"]:
    for value, valid in [("1", True), ("18446744073709551615", True), ("0", False), ("18446744073709551616", False)]:
        scalar_case(f"threshold {attribute}={value}",
                    mutate(FULL, "p:thresholds", attribute, value),
                    "p:thresholds", attribute, value, valid, f"invalid {attribute}",
                    64)
for attribute in ["min-included-ratio", "max-refused-ratio", "pool-refuses-ratio", "min-send-ratio", "max-block-gap-factor", "recovered-block-gap-factor"]:
    values = [("0.1", True), ("1", True), ("0", False), ("-1", False), ("NaN", False), ("INF", False)]
    values += [("1.1", attribute.endswith("factor"))]
    for value, valid in values:
        scalar_case(f"threshold {attribute}={value}",
                    mutate(FULL, "p:thresholds", attribute, value),
                    "p:thresholds", attribute, value, valid, f"invalid {attribute}")

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
    ("seconds", ["0", "4294967296"]),
]:
    for value in values:
        base = RAMP.replace('step="-1"', 'growth="2"') if attribute == "growth" else RAMP
        reason = {
            "start": "invalid ramp start", "step": "step 0: 1 s at [NaN] tx/s",
            "growth": "invalid growth", "steps": "the plan has no step" if value == "0" else "ramp requires at most 10000 steps",
            "seconds": "step 0: 0 s at",
        }[attribute]
        scalar_case(f"ramp {attribute}={value}", mutate(base, "p:load/p:ramp", attribute, value),
                    "p:load/p:ramp", attribute, value, False, reason,
                    32 if attribute in {"steps", "seconds"} else None)
case("10000 ramp steps", RAMP.replace('step="-1" steps="3"', 'step="0" steps="10000"'))
for count, valid in [(0, False), (10000, True), (10001, False)]:
    case(f"{count} rate elements", FULL.replace('<rate tx-per-second="0" seconds="1"/>', '<rate tx-per-second="0" seconds="1"/>' * count), valid,
         cli_error=None if valid else "the plan has no step" if count == 0 else "too many rate steps",
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
    reason = {"plugin": "duplicate or invalid plugin ID", "target": "duplicate or invalid target",
              "credential": "duplicate or invalid credential", "workflow": "duplicate workflow",
              "step": "invalid or duplicate step prepare"}[tag]
    case(f"duplicate {tag} ID", ET.tostring(root, encoding="unicode"), False,
         cli_error=reason, schema_error=(f"Element '{{{NAMESPACE}}}{tag}'", "Duplicate key-sequence"))
case("step IDs are global", FULL.replace('<workflow id="users">', '<workflow id="users"><step id="prepare" use="core.echo"/>'), False,
     cli_error="invalid or duplicate step prepare",
     schema_error=(f"Element '{{{NAMESPACE}}}step'", "Duplicate key-sequence"))
case("duplicate input name", FULL.replace('<input name="value" value="21"/>', '<input name="value" value="21"/>' * 2), False,
     cli_error="duplicate input value", schema_error=(f"Element '{{{NAMESPACE}}}input'", "Duplicate key-sequence"))

# These gaps are intentional XSD 1.0 limitations, documented in the schema/reference.
for label, xml, reason in [
    ("ramp has neither step nor growth", RAMP.replace(' step="-1"', ''), "ramp needs exactly one of step or growth"),
    ("ramp has both step and growth", RAMP.replace('step="-1"', 'step="-1" growth="2"'), "ramp needs exactly one of step or growth"),
    ("input has neither ref nor value", FULL.replace(' value="21"', ''), "input needs exactly one of ref or value"),
    ("input has both ref and value", FULL.replace('value="21"', 'value="21" ref="run.id"'), "input needs exactly one of ref or value"),
    ("state-check without state-ref", mutate(FULL, "p:load", "state-check", "example.check"), "state-check and state-ref must be given together"),
    ("state-ref without state-check", mutate(FULL, "p:load", "state-ref", "steps.prepare.state"), "state-check and state-ref must be given together"),
    ("expanded ramp goes negative", RAMP.replace('step="-1"', 'step="-2"'), "step 2: 1 s at [-2.0] tx/s"),
    ("expanded ramp overflows", RAMP.replace('step="-1"', 'growth="1e308"'), "step 1: 1 s at [inf] tx/s"),
    ("unknown operation provider", mutate(FULL, "p:setup/p:step", "use", "missing.prepare"), "unknown operation provider missing"),
    ("unknown reference", mutate(FULL, "p:load", "source-ref", "steps.missing.flood"), "unknown workload reference"),
    ("forward reference", FULL.replace('value="21"', 'ref="steps.echo.value"'), "unknown or forward reference steps.echo.value"),
    ("monitors without load", re.sub(r'<load\b.*?</load>', '', FULL, flags=re.DOTALL), "monitors require load and relay target"),
]:
    case(label, xml, True, False, "cli_only", cli_error=reason)
for section in ["plugins", "targets", "credentials", "preflight", "setup", "load", "monitors", "thresholds", "evaluate", "teardown"]:
    root = ET.fromstring(FULL)
    node = root.find(f"{{{NAMESPACE}}}{section}")
    if node is None:
        node = ET.SubElement(root, f"{{{NAMESPACE}}}{section}")
        ET.SubElement(node, f"{{{NAMESPACE}}}step", id=f"{section}-step", use="core.echo")
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
     cli_error="unknown load target", schema_error=(f"Element '{{{NAMESPACE}}}load'", "No match found for key-sequence"))
case("undeclared relay target", mutate(FULL, "p:monitors", "relay-target", "missing"), False,
     cli_error="monitors require load and relay target", schema_error=(f"Element '{{{NAMESPACE}}}monitors'", "No match found for key-sequence"))

if __name__ == "__main__":
    print(f"Plan cases: {COUNTS}", flush=True)
    unittest.main(verbosity=2)
