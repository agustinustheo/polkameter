#!/usr/bin/env python3
"""Check examples, full docs plans, and validator/schema boundaries. Run at the repo root.

Requires python3, xmllint (CI installs libxml2-utils), and a built CLI:
    cargo +1.93.0 build -p polkameter --bin polkameter
    python3 tests/validate-plans.py
Set POLKAMETER to use another CLI binary. No plugins or chain are needed.
"""

from html.parser import HTMLParser
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

    def check_plan(self, xml, schema_valid=True, cli_valid=True):
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
            self.assertIn('"valid":true', cli.stdout)


COUNTS = {"published": 0, "agreement": 0, "cli_only": 0}


def case(name, xml, schema_valid=True, cli_valid=None, category="agreement"):
    if cli_valid is None:
        cli_valid = schema_valid
    COUNTS[category] += 1

    def test(self):
        self.check_plan(xml, schema_valid, cli_valid)

    test.__doc__ = name
    # The counter keeps names unique even when file names contain punctuation.
    setattr(PlanValidation, f"test_{sum(COUNTS.values()):03d}_{category}", test)


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
        case(f"required {path}/@{attribute}", mutate(FULL, path, attribute, None), False)

# Values at and beyond all bounded integer ranges, and each scalar enum/pattern.
for path, attribute, good, bad in [
    (".", "version", ["1", "+1", " 1 "], ["0", "2", "4294967296"]),
    (".", "mode", ["stress", "smoke"], ["", "other", " smoke"]),
    (".", "name", [" "], [""]),
    (".", "timeout-ms", ["1", "+1", "18446744073709551615"], ["0", "-1", "18446744073709551616"]),
    ("p:plugins/p:plugin", "id", ["a", "cor", "Core", "corex", "-0_"], ["", "core", "bad.id", "é", "x y"]),
    ("p:plugins/p:plugin", "version", [" "], [""]),
    ("p:plugins/p:plugin", "protocol", ["1"], ["0", "2"]),
    ("p:targets/p:target", "endpoint", ["ws://", "wss://node", "ws://x y"], ["https://node", "WS://node", " ws://node"]),
    ("p:credentials/p:credential", "profile", [" "], [""]),
    ("p:setup/p:step", "use", ["core.any-name_0"], ["core", "core.a.b", "core."]),
    ("p:setup/p:step", "timeout-ms", ["1", "18446744073709551615"], ["0", "18446744073709551616"]),
    ("p:workflow", "users", ["1", "100000"], ["0", "100001"]),
    ("p:workflow", "iterations", ["1", "4294967295"], ["0", "4294967296"]),
    ("p:workflow", "concurrency", ["1", "1000"], ["0", "1001"]),
    ("p:load", "connections", ["1", "64"], ["0", "65"]),
    ("p:load", "baseline-probes", ["1", "1000"], ["0", "1001"]),
    ("p:load", "recovery-seconds", ["0", "+0", "86400"], ["-0", "-1", "86401"]),
    ("p:load", "block-interval-seconds", ["0.1", "1.7976931348623157E308"], ["0.09", "INF", "NaN"]),
    ("p:load/p:rate", "tx-per-second", ["0", "1.7976931348623157E308"], ["-1", "INF", "-INF", "NaN"]),
    ("p:load/p:rate", "seconds", ["1", "4294967295"], ["0", "4294967296"]),
    ("p:monitors", "para-id", ["0", "+0", "1", "999", "1000", "1999", "2000", "4294967295"], ["-0", "-1", "4294967296"]),
]:
    for valid, values in [(True, good), (False, bad)]:
        for value in values:
            xml = mutate(FULL, path, attribute, value)
            if path == "p:plugins/p:plugin" and attribute == "id" and valid:
                xml = xml.replace('use="example.prepare"', f'use="{value}.prepare"')
            case(f"{path}/@{attribute}={value!r}", xml, valid)

for attribute in ["stall-ms", "finality-stall-ms", "max-p95-latency-ms", "max-submit-reply-ms", "finality-wait-ms", "probes-in-a-row"]:
    for value, valid in [("1", True), ("18446744073709551615", True), ("0", False), ("18446744073709551616", False)]:
        case(f"threshold {attribute}={value}", mutate(FULL, "p:thresholds", attribute, value), valid)
for attribute in ["min-included-ratio", "max-refused-ratio", "pool-refuses-ratio", "min-send-ratio", "max-block-gap-factor", "recovered-block-gap-factor"]:
    values = [("0.1", True), ("1", True), ("0", False), ("-1", False), ("NaN", False), ("INF", False)]
    values += [("1.1", attribute.endswith("factor"))]
    for value, valid in values:
        case(f"threshold {attribute}={value}", mutate(FULL, "p:thresholds", attribute, value), valid)

RAMP = FULL.replace('<rate tx-per-second="0" seconds="1"/>', '<ramp start="2" step="-1" steps="3" seconds="1"/>')
case("descending ramp reaches zero", RAMP)
case("geometric ramp", RAMP.replace('step="-1"', 'growth="2"'))
for attribute in ["start", "steps", "seconds"]:
    case(f"ramp requires {attribute}", mutate(RAMP, "p:load/p:ramp", attribute, None), False)
for attribute, values in [
    ("start", ["0", "-1", "INF", "NaN"]),
    ("step", ["INF", "NaN"]),
    ("growth", ["0", "-1", "INF", "NaN"]),
    ("steps", ["0", "10001"]),
    ("seconds", ["0", "4294967296"]),
]:
    for value in values:
        base = RAMP.replace('step="-1"', 'growth="2"') if attribute == "growth" else RAMP
        case(f"ramp {attribute}={value}", mutate(base, "p:load/p:ramp", attribute, value), False)
case("10000 ramp steps", RAMP.replace('step="-1" steps="3"', 'step="0" steps="10000"'))
for count, valid in [(0, False), (10000, True), (10001, False)]:
    case(f"{count} rate elements", FULL.replace('<rate tx-per-second="0" seconds="1"/>', '<rate tx-per-second="0" seconds="1"/>' * count), valid)
case("rates and ramp are exclusive elements", FULL.replace('</load>', '<ramp start="2" step="1" steps="1" seconds="1"/></load>'), False)
case("unknown attribute", mutate(FULL, ".", "unknown", "x"), False)
case("unknown child", plan('<unknown/>'), False)
for tag in ["plugin", "target", "credential", "workflow", "step"]:
    root = ET.fromstring(FULL)
    node = root.find(f".//{{{NAMESPACE}}}{tag}")
    parent = next(parent for parent in root.iter() if node in list(parent))
    parent.append(ET.fromstring(ET.tostring(node)))
    case(f"duplicate {tag} ID", ET.tostring(root, encoding="unicode"), False)
case("step IDs are global", FULL.replace('<workflow id="users">', '<workflow id="users"><step id="prepare" use="core.echo"/>'), False)
case("duplicate input name", FULL.replace('<input name="value" value="21"/>', '<input name="value" value="21"/>' * 2), False)

# These gaps are intentional XSD 1.0 limitations, documented in the schema/reference.
for label, xml in [
    ("ramp has neither step nor growth", RAMP.replace(' step="-1"', '')),
    ("ramp has both step and growth", RAMP.replace('step="-1"', 'step="-1" growth="2"')),
    ("input has neither ref nor value", FULL.replace(' value="21"', '')),
    ("input has both ref and value", FULL.replace('value="21"', 'value="21" ref="run.id"')),
    ("state-check without state-ref", mutate(FULL, "p:load", "state-check", "example.check")),
    ("state-ref without state-check", mutate(FULL, "p:load", "state-ref", "steps.prepare.state")),
    ("expanded ramp goes negative", RAMP.replace('step="-1"', 'step="-2"')),
    ("expanded ramp overflows", RAMP.replace('step="-1"', 'growth="1e308"')),
    ("unknown operation provider", mutate(FULL, "p:setup/p:step", "use", "missing.prepare")),
    ("unknown reference", mutate(FULL, "p:load", "source-ref", "steps.missing.flood")),
    ("forward reference", FULL.replace('value="21"', 'ref="steps.echo.value"')),
    ("monitors without load", re.sub(r'<load\b.*?</load>', '', FULL, flags=re.DOTALL)),
]:
    case(label, xml, True, False, "cli_only")
for section in ["plugins", "targets", "credentials", "preflight", "setup", "load", "monitors", "thresholds", "evaluate", "teardown"]:
    root = ET.fromstring(FULL)
    node = root.find(f"{{{NAMESPACE}}}{section}")
    if node is None:
        node = ET.SubElement(root, f"{{{NAMESPACE}}}{section}")
    root.append(ET.fromstring(ET.tostring(node)))
    # Avoid duplicate-ID errors, so this exercises the singleton check specifically.
    for descendant in root[-1].iter():
        if "id" in descendant.attrib:
            descendant.set("id", "duplicate-section-" + descendant.get("id"))
    case(f"singleton {section}", ET.tostring(root, encoding="unicode"), True, False, "cli_only")
case("state-check and state-ref together", mutate(mutate(FULL, "p:load", "state-check", "example.check"), "p:load", "state-ref", "steps.prepare.state"))
case("metric role checked only at preflight", mutate(FULL, "p:monitors/p:metric", "role", "other"))
case("undeclared load target", mutate(FULL, "p:load", "target", "missing"), False)
case("undeclared relay target", mutate(FULL, "p:monitors", "relay-target", "missing"), False)

if __name__ == "__main__":
    print(f"Plan cases: {COUNTS}", flush=True)
    unittest.main(verbosity=2)
