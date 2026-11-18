// Runs a PolkaMeter call file. The substrate plugin runs it from memory in the call file's
// directory, so imports resolve from the user's node_modules:
// node --input-type=module --eval <this> -- <call file> <ws url>.
// Protocol, one JSON object per line. In: {accounts, seedAccount, count}, then replies
// {id, ok, hash} or {id, error}. Out: {id, submit: <hex call>, from: null | <account index>} for
// setup transactions, then one {call: <hex call>} per transaction in order, then {done: <count>};
// or {error}. Calls are streamed one per line, so there is no limit on their total size.
import { createInterface } from "node:readline";
import { pathToFileURL } from "node:url";

// With --eval, process.argv has no script path: [node, call file, ws url].
const [file, url] = process.argv.slice(1);
// Waits until the line is handed to the pipe: on macOS a pipe is asynchronous, and exiting with
// output still buffered would cut it.
const out = (message) =>
	new Promise((resolve, reject) =>
		process.stdout.write(JSON.stringify(message) + "\n", (e) => (e ? reject(e) : resolve())),
	);
const toHex = (bytes) => "0x" + Buffer.from(bytes).toString("hex");
const isChain = (d) => d && typeof d === "object" && "descriptors" in d;

let client;
try {
	const { createClient } = await import("polkadot-api");
	// The WebSocket provider moved between polkadot-api versions.
	const ws = await import("polkadot-api/ws")
		.catch(() => import("polkadot-api/ws-provider"))
		.catch(() => import("polkadot-api/ws-provider/node"));
	const user = await import(pathToFileURL(file).href);
	let chain = user.chain;
	if (!chain || typeof chain === "string") {
		// No chain, or a key of @polkadot-api/descriptors (the name given to `papi add`).
		const descriptors = await import("@polkadot-api/descriptors");
		const keys = Object.keys(descriptors).filter((k) => isChain(descriptors[k]));
		if (typeof chain === "string") {
			if (!isChain(descriptors[chain])) {
				throw new Error(`@polkadot-api/descriptors has no chain ${JSON.stringify(chain)}; it has ${keys.join(", ") || "none"}`);
			}
			chain = descriptors[chain];
		} else if (keys.length !== 1) {
			throw new Error(`@polkadot-api/descriptors has ${keys.length} chains (${keys.join(", ")}); export the one to use as \`chain\` from the call file, such as export const chain = "${keys[0] ?? "name"}"`);
		} else {
			chain = descriptors[keys[0]];
		}
	}
	if (typeof user.call !== "function") throw new Error("the call file must export a function `call`");
	client = createClient(ws.getWsProvider(url));
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
		await out({ id, submit: await encode(tx), from });
		return done;
	};
	const ctx = { api, client, accounts, seedAccount, count, submit };
	const data = user.setup ? await user.setup(ctx) : undefined;
	for (let i = 0; i < count; i++) {
		const account = accounts[i % accounts.length];
		await out({ call: await encode(await user.call({ ...ctx, i, account, data })) });
	}
	await out({ done: count });
	client.destroy();
	process.exit(0);
} catch (error) {
	try {
		client?.destroy();
	} catch {}
	await out({ error: String(error?.stack ?? error) }).catch(() => {});
	process.exit(1);
}
