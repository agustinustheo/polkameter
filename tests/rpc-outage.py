#!/usr/bin/env python3
"""An RPC outage that is not the chain's fault, on the dev chain.

PolkaMeter runs the CI remarks plan through a TCP proxy. 5 s into the load, the proxy drops every
connection and refuses new ones for 8 s. The node is fine all along, so the run must not blame
it: no "node down" or "pool intake" stop, no failure onset, no FAIL verdict, and the loss check
verifies every transaction on the finalized chain.

Needs the dev chain (scripts/dev-chain.sh start) and the substrate plugin installed with its
credential profile, as in the CI dev-chain job. Run it from the repository root:

    tests/rpc-outage.py target/debug/polkameter target/dev-chain-runs/rpc-outage
"""

import asyncio
import json
import os
import pathlib
import sys

NODE = ("127.0.0.1", 9944)
PROXY = ("127.0.0.1", 9955)
OUTAGE_AFTER_S, OUTAGE_S = 5, 8


class Proxy:
    """Forwards TCP to the node until `down`, and refuses connections until `up`."""

    def __init__(self):
        self.server = None
        self.open = set()

    async def up(self):
        self.server = await asyncio.start_server(self.forward, *PROXY, reuse_address=True)

    async def down(self):
        self.server.close()
        for writer in list(self.open):
            writer.transport.abort()
        await self.server.wait_closed()

    async def forward(self, reader, writer):
        try:
            node_reader, node_writer = await asyncio.open_connection(*NODE)
        except OSError:
            writer.transport.abort()
            return
        pair = {writer, node_writer}
        self.open |= pair

        async def pipe(src, dst):
            try:
                while True:
                    data = await src.read(65536)
                    if not data:
                        break
                    dst.write(data)
                    await dst.drain()
            except OSError:
                pass
            finally:
                dst.transport.abort()

        await asyncio.gather(pipe(reader, node_writer), pipe(node_reader, writer))
        self.open -= pair


async def outage(proxy):
    await asyncio.sleep(OUTAGE_AFTER_S)
    print(f"rpc-outage: proxy down for {OUTAGE_S} s", file=sys.stderr, flush=True)
    await proxy.down()
    await asyncio.sleep(OUTAGE_S)
    await proxy.up()
    print("rpc-outage: proxy up", file=sys.stderr, flush=True)


async def run(polkameter, out):
    out.mkdir(parents=True, exist_ok=True)
    plan = out / "rpc-outage.polkameter.xml"
    source = pathlib.Path("examples/substrate-remarks-ci.polkameter.xml").read_text()
    plan.write_text(source.replace("ws://%s:%d" % NODE, "ws://%s:%d" % PROXY))
    proxy = Proxy()
    await proxy.up()
    # Sample the node's own process: the proxy is the process on the plan's port.
    chain_dir = pathlib.Path(os.environ.get("POLKAMETER_DEV_CHAIN_DIR", ".polkameter/dev-chain"))
    node_pid = (chain_dir / "node.pid").read_text().split()[0]
    env = os.environ | {"POLKAMETER_NODE_PID": node_pid}
    args = [polkameter, "run", str(plan), "--output", str(out), "--fail-on", "error"]
    child = await asyncio.create_subprocess_exec(*args, stderr=asyncio.subprocess.PIPE, env=env)
    fault = None
    async for line in child.stderr:
        text = line.decode(errors="replace")
        sys.stderr.write(text)
        if fault is None and text.startswith("Phase: load"):
            fault = asyncio.ensure_future(outage(proxy))
    code = await child.wait()
    if fault is None:
        sys.exit(f"rpc-outage: the run exited {code} before the load")
    await fault
    return code


def check(out, code):
    (summary,) = out.glob("run-*/summary.json")
    s = json.loads(summary.read_text())
    stop, loss = s["stop"], s["loss"]
    failures = []
    if code != 0:
        failures.append(f"exit code {code}")
    if not any("reconnected" in p for p in s["problems"]):
        failures.append(f"no reconnect recorded, so no outage was seen: {s['problems']}")
    if stop["rule"] in ("node down", "pool intake") or stop["class"] is not None:
        failures.append(f"the load stopped on a chain failure: {stop}")
    if s.get("breakingPoint") is not None:
        failures.append(f"a failure onset: {s['breakingPoint']}")
    if s["verdict"]["result"] == "FAIL":
        failures.append("verdict FAIL")
    counts = {k: loss.get(k) for k in ("lost", "unverified", "unknown", "inPool")}
    if any(n != 0 for n in counts.values()):
        failures.append(f"the loss check did not verify every transaction: {counts}")
    print(summary.with_name("summary.md").read_text())
    if failures:
        sys.exit("rpc-outage: the outage was blamed on the chain:\n  " + "\n  ".join(failures))
    print(f"rpc-outage: ok: {stop['rule']}: {stop['detail']}; verdict {s['verdict']['result']}")


def main():
    polkameter, out = sys.argv[1], pathlib.Path(sys.argv[2])
    code = asyncio.run(run(polkameter, out))
    check(out, code)


if __name__ == "__main__":
    main()
