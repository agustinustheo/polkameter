// Runs a PolkaMeter call file. The substrate plugin writes this next to the call file, so imports
// resolve from the user's node_modules, and runs: node <this> <call file> <ws url>.
// Protocol, one JSON object per line. In: {accounts, seedAccount, count}, then replies
// {id, ok, hash} or {id, error}. Out: {id, submit: <hex call>, from: null | <account index>} for
// setup transactions, then {calls: [<hex call>, ...]}, or {error}.
import { createInterface } from "node:readline";
import { pathToFileURL } from "node:url";

const [file, url] = process.argv.slice(2);
const out = (message) => process.stdout.write(JSON.stringify(message) + "\n");
const toHex = (bytes) => "0x" + Buffer.from(bytes).toString("hex");

try {
	const { createClient } = await import("polkadot-api");
	// The WebSocket provider moved between polkadot-api versions.
	const ws = await import("polkadot-api/ws")
		.catch(() => import("polkadot-api/ws-provider"))
		.catch(() => import("polkadot-api/ws-provider/node"));
	const user = await import(pathToFileURL(file).href);
	let chain = user.chain;
	if (!chain) {
		const descriptors = await import("@polkadot-api/descriptors");
		const chains = Object.values(descriptors).filter((d) => d && typeof d === "object" && "descriptors" in d);
		if (chains.length !== 1) {
			throw new Error(`@polkadot-api/descriptors has ${chains.length} chains; export the one to use as \`chain\` from the call file`);
		}
		chain = chains[0];
	}
	if (typeof user.call !== "function") throw new Error("the call file must export a function `call`");
	const client = createClient(ws.getWsProvider(url));
	const api = client.getTypedApi(chain);

	const waiting = new Map();
	let init;
	const ready = new Promise((resolve) => (init = resolve));
	createInterface({ input: process.stdin }).on("line", (line) => {
		const message = JSON.parse(line);
		if (message.id === undefined) return init(message);
		const w = waiting.get(message.id);
		waiting.delete(message.id);
		if (message.error) w.reject(new Error(message.error));
		else w.resolve(message);
	});
	const { accounts, seedAccount, count } = await ready;

	const encode = async (tx) => {
		if (typeof tx === "string") return tx;
		if (tx instanceof Uint8Array) return toHex(tx);
		const data = await tx.getEncodedData();
		return typeof data === "string" ? data : data.asHex ? data.asHex() : toHex(data);
	};
	let nextId = 0;
	// Signed by PolkaMeter (the seed, or test account `from`), sent, and resolved once final.
	const submit = async (tx, from = null) => {
		const id = nextId++;
		const done = new Promise((resolve, reject) => waiting.set(id, { resolve, reject }));
		out({ id, submit: await encode(tx), from });
		return done;
	};
	const ctx = { api, client, accounts, seedAccount, count, submit };
	const data = user.setup ? await user.setup(ctx) : undefined;
	const calls = [];
	for (let i = 0; i < count; i++) {
		const account = accounts[i % accounts.length];
		calls.push(await encode(await user.call({ ...ctx, i, account, data })));
	}
	out({ calls });
	client.destroy();
	process.exit(0);
} catch (error) {
	out({ error: String(error?.stack ?? error) });
	process.exit(1);
}
