// Darwinia stress test: Assets.transfer_keep_alive(asset 1026) at RATE tx/s for DURATION s from 4 derived accounts.
// usage: node stress.mjs <ws-endpoint> <output-dir>
// exit: 0 PASS, 1 FAIL (chain missed a threshold), 3 setup failed, 4 CAN'T TELL
import fs from 'node:fs';
import path from 'node:path';
import { createClient } from 'polkadot-api';
import { getWsProvider } from 'polkadot-api/ws';
import { getTxCreator } from 'polkadot-api/tx-creator';
import { decAnyMetadata, unifyMetadata, Blake2256 } from '@polkadot-api/substrate-bindings';
import { getDynamicBuilder, getLookupFn } from '@polkadot-api/metadata-builders';
import { secp256k1 } from '@noble/curves/secp256k1.js';
import { keccak_256 } from '@noble/hashes/sha3.js';
import { darwinia } from '@polkadot-api/descriptors';

const [ENDPOINT, OUT] = process.argv.slice(2);
if (!ENDPOINT || !OUT) { console.error('usage: node stress.mjs <ws-endpoint> <output-dir>'); process.exit(3); }
fs.mkdirSync(OUT, { recursive: true });

const RATE = 0.25, DURATION_S = 200, N_ACCOUNTS = 4;
const TOTAL = Math.round(RATE * DURATION_S); // 50
const ASSET = 1026n, AMOUNT = 10n ** 15n, ASSET_FUND = 10n ** 21n, NATIVE_FUND = 10n ** 20n;
const PERIOD = 64; // mortality period (blocks)
const P95_LIMIT_MS = 60_000, DRAIN_MS = 120_000;
const SEED_PRIV = '0x5fb92d6e98884f76de468fa3f6278f8807c48bebc13595d45af5bdc4da702133';

const t0 = Date.now();
const logFile = fs.createWriteStream(path.join(OUT, 'run.log'));
const log = (...a) => { const s = `[${((Date.now() - t0) / 1000).toFixed(1).padStart(6)}s] ${a.join(' ')}`; console.log(s); logFile.write(s + '\n'); };
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const hex = (u8) => '0x' + Buffer.from(u8).toString('hex');
const unhex = (h) => Uint8Array.from(Buffer.from(h.slice(2), 'hex'));
const withTimeout = (p, ms, what) => Promise.race([p, sleep(ms).then(() => { throw new Error(`${what} timed out after ${ms}ms`); })]);
const own = { reconnects: 0, sendErrors: 0, resends: 0, rpcErrors: 0 }; // tester-side problems, never chain problems

// ---------- raw JSON-RPC over our own reconnecting WebSocket (submission + finalized-chain reads) ----------
class Rpc {
  constructor(url) { this.url = url; this.id = 0; this.ws = null; this.connecting = null; this.pending = new Map(); this.everOpen = false; }
  connect() {
    if (this.ws?.readyState === 1) return Promise.resolve();
    if (this.connecting) return this.connecting;
    this.connecting = new Promise((res, rej) => {
      const ws = new WebSocket(this.url);
      const timer = setTimeout(() => { try { ws.close(); } catch {} rej(new Error('connect timeout')); }, 10_000);
      ws.onopen = () => { clearTimeout(timer); if (this.everOpen) { own.reconnects++; log('rpc: reconnected'); } this.everOpen = true; this.ws = ws; res();
        // push subscription: finalized-head arrival times don't depend on how fast we fetch block bodies
        if (this.onHead) ws.send(JSON.stringify({ jsonrpc: '2.0', id: ++this.id, method: 'chain_subscribeFinalizedHeads', params: [] }));
      };
      ws.onerror = () => { clearTimeout(timer); rej(new Error('websocket error')); };
      ws.onclose = () => {
        clearTimeout(timer); rej(new Error('websocket closed'));
        if (this.ws === ws) { this.ws = null; log('rpc: connection dropped'); }
        for (const [id, p] of this.pending) if (p.ws === ws) { this.pending.delete(id); p.rej(new Error('connection dropped')); }
      };
      ws.onmessage = (m) => {
        const msg = JSON.parse(m.data);
        if (msg.method === 'chain_finalizedHead' && msg.params?.result?.number) { this.onHead?.(parseInt(msg.params.result.number, 16)); return; }
        const p = this.pending.get(msg.id); if (!p) return;
        this.pending.delete(msg.id);
        if (msg.error) { const e = new Error(msg.error.message + (msg.error.data ? ': ' + JSON.stringify(msg.error.data) : '')); e.rpc = msg.error; p.rej(e); } else p.res(msg.result);
      };
    }).finally(() => { this.connecting = null; });
    return this.connecting;
  }
  async call(method, params = [], timeout = 30_000) {
    await this.connect();
    const ws = this.ws, id = ++this.id;
    return withTimeout(new Promise((res, rej) => { this.pending.set(id, { res, rej, ws }); ws.send(JSON.stringify({ jsonrpc: '2.0', id, method, params })); }), timeout, method)
      .finally(() => this.pending.delete(id));
  }
  // Retries transport failures (our problem) until deadline; RPC-level errors are thrown to the caller.
  async retry(method, params, { timeout = 30_000, deadlineMs = 120_000 } = {}) {
    const end = Date.now() + deadlineMs;
    for (;;) {
      try { return await this.call(method, params, timeout); } catch (e) {
        if (e.rpc || Date.now() > end) throw e;
        own.rpcErrors++; await sleep(1000);
      }
    }
  }
  close() { const ws = this.ws; this.ws = null; try { ws?.close(); } catch {} }
}

// ---------- accounts ----------
function ethAccount(privHex) {
  const priv = unhex(privHex);
  const raw = keccak_256(secp256k1.getPublicKey(priv, false).slice(1)).slice(-20);
  const signer = getTxCreator(raw, 'Ecdsa', (data) => {
    const sig = secp256k1.sign(keccak_256(data), priv, { prehash: false, format: 'recovered' }); // [rec, r, s]
    return Uint8Array.from([...sig.slice(1), sig[0]]);
  });
  return { address: hex(raw), signer };
}
const seed = ethAccount(SEED_PRIV);
const senders = Array.from({ length: N_ACCOUNTS }, (_, i) => ethAccount(hex(keccak_256(new TextEncoder().encode(`${SEED_PRIV}/darwinia-stress/${i}`)))));

const rpc = new Rpc(ENDPOINT);
const client = createClient(getWsProvider(ENDPOINT));
const api = client.getTypedApi(darwinia);

let codecs; // System.Events storage codec from the chain's own metadata
async function loadCodecs() {
  const meta = unifyMetadata(decAnyMetadata(await rpc.retry('state_getMetadata', [])));
  const b = getDynamicBuilder(getLookupFn(meta));
  const ev = b.buildStorage('System', 'Events'), acct = b.buildStorage('System', 'Account');
  codecs = { eventsKey: ev.keys.enc(), events: (h) => ev.value.dec(h), acctKey: (a) => acct.keys.enc(a), acct: (h) => acct.value.dec(h) };
}
// Wall-clock time we first learned each block number was finalized (from the push subscription or a poll).
// Blocks finalized while our connection was down get the reconnect time (a small upper bound, our side).
const finalSeenMs = new Map(); let lastNoted = null;
function noteFinal(n) {
  const now = Date.now();
  if (lastNoted == null) { lastNoted = n; finalSeenMs.set(n, now); return; }
  for (; lastNoted < n; ) finalSeenMs.set(++lastNoted, now);
}
rpc.onHead = noteFinal;
async function finalizedHead() {
  const hash = await rpc.retry('chain_getFinalizedHead', []);
  const hdr = await rpc.retry('chain_getHeader', [hash]);
  const number = parseInt(hdr.number, 16); noteFinal(number);
  return { hash, number };
}
async function finalizedNonce(address, at) {
  const raw = await rpc.retry('state_getStorage', [codecs.acctKey(address), at]);
  return raw ? Number(codecs.acct(raw).nonce) : 0;
}

// Sign against a finalized block with explicit nonce + mortality. Retries only the signing step (no bytes exist yet).
async function signTx(call, acct, nonce) {
  for (let attempt = 0; ; attempt++) {
    try {
      const fin = await finalizedHead();
      const bytes = await withTimeout(call.create(acct.signer, { nonce, mortality: { mortal: true, period: PERIOD, at: { hash: fin.hash, number: fin.number } } }), 30_000, 'sign');
      return { bytes: hex(bytes), hash: hex(Blake2256(bytes)), birth: fin.number };
    } catch (e) { own.rpcErrors++; log(`sign retry ${attempt} for ${acct.address} nonce ${nonce}: ${e.message}`); await sleep(1000); }
  }
}

// Submit identical signed bytes until the pool has it (or the chain says the nonce is already used).
// Returns 'accepted' | 'stale' | 'rejected' (invalid for a reason that resending can't fix, after giving up).
async function submitUntilAccepted(tx, giveUpAt) {
  for (let attempt = 0; ; attempt++) {
    if (tx.firstSubmitMs == null) tx.firstSubmitMs = Date.now();
    if (attempt > 0) own.resends++;
    try { await rpc.call('author_submitExtrinsic', [tx.bytes], 60_000); tx.submitOk = true; return 'accepted'; } catch (e) {
      const m = e.message || '';
      if (/already imported|priority is too low/i.test(m)) { tx.submitOk = true; return 'accepted'; }
      if (/stale/i.test(m)) { tx.submitOk = true; return 'stale'; } // nonce already consumed (e.g. an earlier attempt landed)
      own.sendErrors++; tx.lastError = m;
      log(`send error ${tx.signer.slice(0, 8)} nonce ${tx.nonce} attempt ${attempt}: ${m.slice(0, 160)}`);
      if (Date.now() > giveUpAt) return 'rejected';
      await sleep(Math.min(1000 * 2 ** Math.min(attempt, 3), 5000));
    }
  }
}

// ---------- finalized-chain watcher: scans every finalized block once, in order ----------
const byHash = new Map(); // tx hash -> tx
const seenAt = new Map(); // tx hash -> { blockHash, number, index, ok, seenMs }
let scannedTo = null, finalNow = null;
async function scanFinalized() {
  const head = await finalizedHead(); finalNow = head;
  if (scannedTo == null) { scannedTo = head.number; return; }
  for (let n = scannedTo + 1; n <= head.number; n++) {
    const bh = await rpc.retry('chain_getBlockHash', [n]);
    const blk = await rpc.retry('chain_getBlock', [bh]);
    const exts = blk.block.extrinsics; let evs = null;
    for (let i = 0; i < exts.length; i++) {
      const h = hex(Blake2256(unhex(exts[i])));
      if (!byHash.has(h) || seenAt.has(h)) continue;
      evs ??= codecs.events(await rpc.retry('state_getStorage', [codecs.eventsKey, bh]));
      const mine = evs.filter((e) => e.phase.type === 'ApplyExtrinsic' && e.phase.value === i && e.event.type === 'System');
      const ok = mine.some((e) => e.event.value.type === 'ExtrinsicSuccess') && !mine.some((e) => e.event.value.type === 'ExtrinsicFailed');
      const failed = mine.find((e) => e.event.value.type === 'ExtrinsicFailed');
      seenAt.set(h, { blockHash: bh, number: n, index: i, ok, seenMs: finalSeenMs.get(n) ?? Date.now(), error: failed ? JSON.stringify(failed.event.value.value.dispatch_error, (k, v) => typeof v === 'bigint' ? v.toString() : v) : undefined });
    }
    scannedTo = n;
  }
}
let watching = true;
const watcher = (async () => { while (watching) { try { await scanFinalized(); } catch (e) { own.rpcErrors++; log('watcher error (ours):', e.message); } await sleep(1000); } })();

// ---------- setup ----------
async function setup() {
  await rpc.connect(); await loadCodecs();
  await withTimeout(client.getFinalizedBlock(), 60_000, 'papi connect');
  log(`chain ${await rpc.retry('system_chain', [])}, finalized #${(await finalizedHead()).number}`);
  log(`seed ${seed.address}; senders ${senders.map((s) => s.address).join(' ')}`);
  // 1) native funds for fees: one batch of transfers from the seed, signed/sent with the same rules as the load
  const fin = await finalizedHead();
  const need = [];
  for (const s of senders) {
    const raw = await rpc.retry('state_getStorage', [codecs.acctKey(s.address), fin.hash]);
    const free = raw ? codecs.acct(raw).data.free : 0n;
    if (free < NATIVE_FUND / 2n) need.push(s);
  }
  if (need.length) {
    const call = api.tx.Utility.batch_all({ calls: need.map((s) => api.tx.Balances.transfer_keep_alive({ dest: s.address, value: NATIVE_FUND }).decodedCall) });
    const nonce = await finalizedNonce(seed.address, fin.hash);
    const tx = { ...(await signTx(call, seed, nonce)), signer: seed.address, nonce };
    byHash.set(tx.hash, tx);
    const r = await submitUntilAccepted(tx, Date.now() + 60_000);
    if (r === 'rejected') throw new Error('funding transfer rejected: ' + tx.lastError);
    const end = Date.now() + 180_000;
    while (!seenAt.has(tx.hash)) { if (Date.now() > end) throw new Error('funding transfer not finalized in 180s'); await sleep(500); }
    if (!seenAt.get(tx.hash).ok) throw new Error('funding transfer failed in block: ' + seenAt.get(tx.hash).error);
    byHash.delete(tx.hash);
    log(`funded ${need.length} senders with native token in #${seenAt.get(tx.hash).number}`);
  }
  // 2) asset 1026 balance: Assets.create/mint are permissioned on Darwinia, so set it via the fork's dev_setStorage
  await rpc.retry('dev_setStorage', [{ Assets: { Account: senders.map((s) => [[Number(ASSET), s.address], { balance: ASSET_FUND.toString(), status: 'Liquid', reason: 'Sufficient', extra: null }]) } }]);
  await rpc.retry('dev_newBlock', [{ count: 1 }], { timeout: 180_000 });
  for (const s of senders) {
    const a = await withTimeout(api.query.Assets.Account.getValue(ASSET, s.address, { at: 'finalized' }), 60_000, 'asset query');
    if (!a || a.balance < AMOUNT * BigInt(TOTAL)) throw new Error(`asset balance missing for ${s.address}`);
  }
  log('asset 1026 balances set for all senders');
}

// ---------- load ----------
const txs = [];
async function load() {
  const fin = await finalizedHead();
  const accts = await Promise.all(senders.map(async (s) => ({ ...s, nextNonce: await finalizedNonce(s.address, fin.hash), queue: [], wake: null, blocked: false })));
  const startMs = Date.now(), endMs = startMs + DURATION_S * 1000;
  log(`load: ${TOTAL} tx at ${RATE}/s for ${DURATION_S}s from ${N_ACCOUNTS} accounts (start nonces ${accts.map((a) => a.nextNonce).join(',')})`);
  // One sequential worker per account: sign next nonce only after the previous one is in the pool.
  const workers = accts.map(async (a) => {
    for (;;) {
      if (!a.queue.length) { if (a.done) return; await new Promise((r) => (a.wake = r)); a.wake = null; continue; }
      const job = a.queue.shift();
      if (a.blocked) { job.skipped = true; continue; } // never send past an unresolved gap
      const call = api.tx.Assets.transfer_keep_alive({ id: ASSET, target: seed.address, amount: AMOUNT });
      const nonce = a.nextNonce;
      const tx = { idx: job.idx, signer: a.address, nonce, scheduledMs: job.at, firstSubmitMs: null };
      Object.assign(tx, await signTx(call, a, nonce));
      a.nextNonce++; txs.push(tx); byHash.set(tx.hash, tx);
      // keep resending the identical bytes until well after the window; if still rejected, block this account
      const r = await submitUntilAccepted(tx, endMs + DRAIN_MS);
      if (r === 'rejected') { a.blocked = true; log(`account ${a.address} blocked at unresolved nonce ${nonce}`); }
    }
  });
  for (let i = 0; i < TOTAL; i++) {
    const at = startMs + (i * 1000) / RATE;
    const w = at - Date.now(); if (w > 0) await sleep(w);
    const a = accts[i % N_ACCOUNTS];
    a.queue.push({ idx: i, at }); a.wake?.();
  }
  for (const a of accts) { a.done = true; a.wake?.(); }
  await Promise.all(workers);
  log(`load done: ${txs.length} signed and submitted`);
  return { startMs, endMs, accts };
}

// ---------- reconcile ----------
async function reconcile(startMs, endMs) {
  const allIn = () => txs.every((t) => seenAt.has(t.hash));
  const drainEnd = Date.now() + DRAIN_MS;
  while (!allIn() && Date.now() < drainEnd) await sleep(1000);
  watching = false; await watcher;
  await scanFinalized(); // final pass over every finalized block
  const fin = finalNow;
  const nonces = {};
  for (const s of senders) nonces[s.address] = await finalizedNonce(s.address, fin.hash);
  const rows = txs.map((t) => {
    const s = seenAt.get(t.hash);
    let category;
    if (s) category = s.ok ? 'finalized-ok' : 'failed-in-block';
    else if (nonces[t.signer] > t.nonce || fin.number >= t.birth + PERIOD) category = 'lost'; // proof: nonce passed it or mortality expired, and in no finalized block
    else category = 'unresolved';
    return { hash: t.hash, category, signer: t.signer, nonce: t.nonce, firstSubmitMs: t.firstSubmitMs, block: s?.number ?? null, latencyMs: s ? s.seenMs - t.firstSubmitMs : null, ...(s?.error ? { error: s.error } : {}), ...(t.lastError && !t.submitOk ? { submitError: t.lastError } : {}) };
  });
  fs.writeFileSync(path.join(OUT, 'transactions.jsonl'), rows.map((r) => JSON.stringify(r)).join('\n') + (rows.length ? '\n' : ''));
  const count = (c) => rows.filter((r) => r.category === c).length;
  const sent = rows.length;
  const onTime = rows.filter((r) => r.firstSubmitMs != null && r.firstSubmitMs <= endMs + 2000).length;
  const required = Math.ceil(0.95 * TOTAL);
  const lat = rows.filter((r) => r.latencyMs != null).map((r) => r.latencyMs).sort((a, b) => a - b);
  const p95 = lat.length ? lat[Math.ceil(0.95 * lat.length) - 1] : null;
  const lastSubmit = Math.max(...rows.map((r) => r.firstSubmitMs));
  const summary = {
    target: { rate: RATE, durationS: DURATION_S, transactions: TOTAL },
    sent, finalizedOk: count('finalized-ok'), failedInBlock: count('failed-in-block'), lost: count('lost'), unresolved: count('unresolved'),
    submittedOnTime: onTime, requiredOnTime: required, loadDelivered: onTime >= required,
    deliveredRateTxPerS: +(sent / (Math.max(lastSubmit, endMs) - startMs) * 1000).toFixed(4),
    finalizedRateTxPerS: +(count('finalized-ok') / DURATION_S).toFixed(4),
    p95LatencyMs: p95, maxLatencyMs: lat.at(-1) ?? null,
    finalizedHead: fin.number, testerSide: own,
  };
  let verdict, code;
  if ((p95 != null && p95 >= P95_LIMIT_MS) || summary.lost > 0) { verdict = 'FAIL'; code = 1; }
  else if (!summary.loadDelivered || summary.unresolved > 0 || p95 == null) { verdict = "CAN'T TELL"; code = 4; }
  else { verdict = 'PASS'; code = 0; }
  const reasons = [];
  if (p95 != null && p95 >= P95_LIMIT_MS) reasons.push(`chain: p95 latency ${p95}ms >= ${P95_LIMIT_MS}ms`);
  if (summary.lost) reasons.push(`chain: ${summary.lost} transaction(s) provably lost`);
  if (!summary.loadDelivered) reasons.push(`tester: load not delivered (${onTime}/${TOTAL} submitted on time, need ${required})`);
  if (summary.unresolved) reasons.push(`${summary.unresolved} transaction(s) unresolved`);
  Object.assign(summary, { verdict, exitCode: code, reasons });
  fs.writeFileSync(path.join(OUT, 'summary.json'), JSON.stringify(summary, null, 2) + '\n');
  log(`sent=${sent} finalized-ok=${summary.finalizedOk} failed-in-block=${summary.failedInBlock} lost=${summary.lost} unresolved=${summary.unresolved}`);
  log(`delivered: ${onTime}/${TOTAL} first-submitted within window+2s (need ${required}); delivered rate ${summary.deliveredRateTxPerS} tx/s; finalized-ok rate ${summary.finalizedRateTxPerS} tx/s`);
  log(`p95 latency (first submit -> finalized) ${p95 == null ? 'n/a' : (p95 / 1000).toFixed(1) + 's'}; max ${summary.maxLatencyMs == null ? 'n/a' : (summary.maxLatencyMs / 1000).toFixed(1) + 's'}`);
  log(`tester-side (not chain) issues: reconnects=${own.reconnects} sendErrors=${own.sendErrors} resends=${own.resends} rpcRetries=${own.rpcErrors}`);
  log(`VERDICT: ${verdict}${reasons.length ? ' — ' + reasons.join('; ') : ''}`);
  return code;
}

let code;
try {
  await withTimeout(setup(), 600_000, 'setup');
} catch (e) {
  log('SETUP FAILED:', e.stack || e.message);
  fs.writeFileSync(path.join(OUT, 'summary.json'), JSON.stringify({ verdict: 'SETUP FAILED', exitCode: 3, error: String(e.message) }, null, 2) + '\n');
  code = 3;
}
if (code == null) {
  try {
    const { startMs, endMs } = await load();
    code = await reconcile(startMs, endMs);
  } catch (e) {
    log('ERROR during load/reconcile (tester-side):', e.stack || e.message);
    code = 4;
  }
}
watching = false; rpc.close(); client.destroy();
logFile.end(() => process.exit(code));
