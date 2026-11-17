// Acala Dex.swap_with_exact_supply (ACA -> AUSD) stress test.
// usage: node stress.mjs <ws-endpoint> <output-dir>
// Exit: 0 PASS, 1 FAIL (chain missed a threshold), 4 CAN'T TELL, 3 setup failed before the load.
//
// Design:
//  - Own reconnecting JSON-RPC client (Rpc) for submits and finalized-block scanning, so a dropped
//    connection only delays us and every request we make is explicitly retried.
//  - Signing is offline (PAPI getOfflineApi) with an explicit nonce and mortality anchored at the
//    latest finalized block we have seen, so signing never depends on the connection.
//  - Each account is a lane: transactions are submitted strictly in nonce order; a transaction is
//    resent (identical bytes) until the node acknowledges it, and later nonces wait behind it.
import fs from 'node:fs';
import path from 'node:path';
import { getOfflineApi, getTypedCodecs, AccountId, Enum } from 'polkadot-api';
import { getTxCreator } from 'polkadot-api/tx-creator';
import { sr25519CreateDerive } from '@polkadot-labs/hdkd';
import { DEV_PHRASE, entropyToMiniSecret, mnemonicToEntropy } from '@polkadot-labs/hdkd-helpers';
import { blake2b } from '@noble/hashes/blake2.js';
import { acala } from '@polkadot-api/descriptors';

// ---- parameters -------------------------------------------------------------------------------
const RATE = 0.25;               // tx/s
const DURATION_S = 200;          // s
const N_TX = Math.round(RATE * DURATION_S); // 50
const N_ACCOUNTS = 4;
const SUPPLY = 10n ** 15n;       // planck ACA per swap
const FUND = 10n ** 18n;         // planck ACA per test account
const MORTALITY_PERIOD = 128;    // blocks
const P95_LIMIT_MS = 60_000;
const SUBMIT_TIMEOUT_MS = 30_000;
const RESOLVE_GRACE_MS = 120_000; // after the last first-submit, wait this long for inclusion
const SETUP_TIMEOUT_MS = 240_000;

const [endpoint, outArg] = process.argv.slice(2);
if (!endpoint || !outArg) { console.error('usage: node stress.mjs <ws-endpoint> <output-dir>'); process.exit(3); }
const outDir = path.resolve(outArg);
fs.mkdirSync(outDir, { recursive: true });
const logFile = fs.createWriteStream(path.join(outDir, 'run.log'), { flags: 'w' });
const t00 = Date.now();
const log = (...a) => {
  const line = `[${((Date.now() - t00) / 1000).toFixed(1).padStart(6)}s] ` + a.map((x) => typeof x === 'string' ? x : JSON.stringify(x, big)).join(' ');
  console.log(line); logFile.write(line + '\n');
};
const big = (_k, v) => typeof v === 'bigint' ? v.toString() : v;
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const hex = (u8) => '0x' + Buffer.from(u8).toString('hex');
const unhex = (h) => Uint8Array.from(Buffer.from(h.slice(2), 'hex'));
const txHash = (bytes) => hex(blake2b(bytes, { dkLen: 32 }));

// ---- reconnecting JSON-RPC client ---------------------------------------------------------------
class TransportError extends Error { constructor(m) { super(m); this.transport = true; } }
class RpcError extends Error { constructor(e) { super(`${e.code}: ${e.message}${e.data ? ' ' + JSON.stringify(e.data) : ''}`); this.rpc = e; } }

class Rpc {
  constructor(url) {
    this.url = url; this.id = 0; this.pending = new Map(); this.ws = null; this.open = false;
    this.connects = 0; this.drops = 0; this.waiters = []; this.onReconnect = () => {};
    this.closed = false; this.connect();
  }
  connect() {
    if (this.closed) return;
    let ws;
    try { ws = new WebSocket(this.url); } catch (e) { setTimeout(() => this.connect(), 1000); return; }
    this.ws = ws;
    ws.onopen = () => {
      this.open = true; this.connects++;
      if (this.connects > 1) { log(`rpc: reconnected (#${this.connects - 1})`); this.onReconnect(); }
      this.waiters.splice(0).forEach((w) => w());
    };
    ws.onmessage = (m) => {
      let msg; try { msg = JSON.parse(m.data); } catch { return; }
      const p = this.pending.get(msg.id); if (!p) return;
      this.pending.delete(msg.id); clearTimeout(p.timer);
      if (msg.error) p.reject(new RpcError(msg.error)); else p.resolve(msg.result);
    };
    ws.onerror = () => {};
    ws.onclose = () => {
      const was = this.open; this.open = false;
      if (was && !this.closed) { this.drops++; log('rpc: connection dropped'); }
      for (const [id, p] of this.pending) { clearTimeout(p.timer); p.reject(new TransportError('connection closed')); this.pending.delete(id); }
      if (!this.closed) setTimeout(() => this.connect(), 1000);
    };
  }
  async waitOpen(ms) {
    if (this.open) return;
    await new Promise((resolve, reject) => {
      const t = setTimeout(() => { this.waiters = this.waiters.filter((w) => w !== done); reject(new TransportError('not connected')); }, ms);
      const done = () => { clearTimeout(t); resolve(); };
      this.waiters.push(done);
    });
  }
  async call(method, params = [], timeoutMs = 20_000) {
    const deadline = Date.now() + timeoutMs;
    await this.waitOpen(timeoutMs);
    const id = ++this.id;
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => { this.pending.delete(id); reject(new TransportError(`timeout ${method}`)); }, Math.max(1, deadline - Date.now()));
      this.pending.set(id, { resolve, reject, timer });
      try { this.ws.send(JSON.stringify({ jsonrpc: '2.0', id, method, params })); }
      catch (e) { clearTimeout(timer); this.pending.delete(id); reject(new TransportError('send failed: ' + e.message)); }
    });
  }
  // retry transport errors until the overall deadline
  async callRetry(method, params, totalMs = 60_000) {
    const end = Date.now() + totalMs;
    for (;;) {
      try { return await this.call(method, params, Math.min(20_000, Math.max(1000, end - Date.now()))); }
      catch (e) { if (!e.transport || Date.now() > end) throw e; await sleep(500); }
    }
  }
  close() { this.closed = true; try { this.ws?.close(); } catch {} }
}

// ---- keys ---------------------------------------------------------------------------------------
const derive = sr25519CreateDerive(entropyToMiniSecret(mnemonicToEntropy(DEV_PHRASE)));
const ss58 = AccountId(42);
function keyFor(p) {
  const kp = derive(p);
  return { path: p, pub: kp.publicKey, address: ss58.dec(kp.publicKey), signer: getTxCreator(kp.publicKey, 'Sr25519', kp.sign) };
}
const seed = keyFor('//Alice');
const accounts = Array.from({ length: N_ACCOUNTS }, (_, i) => keyFor(`//Alice//stress//${i}`));

// storage keys
const SYSTEM_ACCOUNT_PREFIX = '0x26aa394eea5630e07c48ae0c9558cef7b99d880ec681799c0cf30e8886371da9';
const SYSTEM_EVENTS_KEY = '0x26aa394eea5630e07c48ae0c9558cef780d41e5e16056765bc8461851072c9d7';
const accountKey = (pub) => SYSTEM_ACCOUNT_PREFIX + hex(blake2b(pub, { dkLen: 16 })).slice(2) + hex(pub).slice(2);

// ---- state --------------------------------------------------------------------------------------
const rpc = new Rpc(endpoint);
let codecs, offline;
const txs = [];                 // every transaction we signed (load only)
const byHash = new Map();       // hash -> tx (load + setup)
const finalized = { hash: null, number: -1 }; // latest finalized we have seen
let scanned = -1;               // highest finalized block number scanned for our hashes
let lastPollOk = Date.now();
let trackerStop = false;

async function readAccount(pub, at, totalMs = 60_000) {
  const v = await rpc.callRetry('state_getStorage', [accountKey(pub), at], totalMs);
  if (!v) return { nonce: 0, free: 0n };
  const d = codecs.query.System.Account.value.dec(v);
  return { nonce: d.nonce, free: d.data.free };
}

// Scan finalized blocks for our hashes. Inclusion = found in a finalized block.
async function trackerTick() {
  const head = await rpc.call('chain_getFinalizedHead', [], 10_000);
  const header = await rpc.call('chain_getHeader', [head], 10_000);
  const n = parseInt(header.number, 16);
  const late = Date.now() - lastPollOk > 3000; // our observer had a gap: observation time is only an upper bound
  for (let k = scanned + 1; k <= n; k++) {
    const h = k === n ? head : await rpc.call('chain_getBlockHash', [k], 10_000);
    const blk = await rpc.call('chain_getBlock', [h], 20_000);
    const seenMs = Date.now();
    const exts = blk.block.extrinsics;
    let events = null;
    for (let i = 0; i < exts.length; i++) {
      const tx = byHash.get(txHash(unhex(exts[i])));
      if (!tx || tx.block) continue;
      if (!events) events = codecs.query.System.Events.value.dec(await rpc.call('state_getStorage', [SYSTEM_EVENTS_KEY, h], 20_000));
      const mine = events.filter((e) => e.phase.type === 'ApplyExtrinsic' && e.phase.value === i);
      const failed = mine.find((e) => e.event.type === 'System' && e.event.value.type === 'ExtrinsicFailed');
      tx.block = { number: k, hash: h, index: i };
      tx.finalizedSeenMs = seenMs; tx.observedLate = late;
      tx.ok = !failed;
      if (failed) tx.dispatchError = JSON.stringify(failed.event.value.value.dispatch_error, big);
    }
    scanned = k;
  }
  finalized.hash = head; finalized.number = n;
  lastPollOk = Date.now();
}
async function trackerLoop() {
  while (!trackerStop) {
    try { await trackerTick(); } catch (e) { if (!e.transport) log('tracker error:', e.message); }
    await sleep(500);
  }
}

function sign(entry, key, nonce) {
  const at = { hash: finalized.hash, number: finalized.number };
  return entry.create(key.signer, { nonce, mortality: { mortal: true, period: MORTALITY_PERIOD, at } })
    .then((bytes) => ({ bytes, hex: hex(bytes), hash: txHash(bytes), at }));
}

// Submit one transaction until the node acknowledges it (or definitively rejects it).
const ACK_RE = /already imported|Priority is too low|Temporarily banned|Transaction is outdated|\bStale\b/i;
async function submitUntilAck(tx) {
  for (;;) {
    const start = Date.now();
    if (tx.firstSubmitMs == null) tx.firstSubmitMs = start;
    tx.attempts++;
    try {
      await rpc.call('author_submitExtrinsic', [tx.hex], SUBMIT_TIMEOUT_MS);
      tx.ackMs = Date.now(); tx.ackAttemptMs = start; return true;
    } catch (e) {
      if (e.transport) { tx.transportRetries++; log(`  ${tx.label} send failed (${e.message}); resending identical bytes`); await sleep(1000); continue; }
      const msg = e.message;
      if (ACK_RE.test(msg)) { tx.ackMs = Date.now(); tx.ackAttemptMs = start; tx.ackNote = msg; log(`  ${tx.label} resend answered: ${msg}`); return true; }
      tx.rejected = msg; log(`  ${tx.label} REJECTED by node: ${msg}`); return false;
    }
  }
}

// ---- lanes --------------------------------------------------------------------------------------
class Lane {
  constructor(key, nonce) { this.key = key; this.nonce = nonce; this.queue = []; this.running = false; this.halted = null; }
  push(tx) { this.queue.push(tx); this.run(); }
  async run() {
    if (this.running) return; this.running = true;
    while (this.queue.length && !this.halted) {
      const tx = this.queue[0];
      if (!(await submitUntilAck(tx))) { this.halted = `nonce ${tx.nonce} rejected: ${tx.rejected}`; break; }
      this.queue.shift();
    }
    this.running = false;
  }
}

// After a reconnect (or periodically while waiting) resend acknowledged-but-unincluded txs, identical bytes, nonce order.
async function rebroadcast(reason) {
  const open = txs.filter((t) => t.ackMs && !t.block).sort((a, b) => a.nonce - b.nonce);
  if (!open.length) return;
  log(`rebroadcast ${open.length} pending tx (${reason})`);
  for (const t of open) {
    try { await rpc.call('author_submitExtrinsic', [t.hex], SUBMIT_TIMEOUT_MS); t.rebroadcasts++; }
    catch (e) { t.rebroadcasts++; /* already imported / stale / transport: inclusion is decided by the scan */ }
  }
}

// ---- setup --------------------------------------------------------------------------------------
async function setup() {
  await rpc.waitOpen(60_000).catch(() => { throw new Error(`cannot connect to ${endpoint}`); });
  const genesis = await rpc.callRetry('chain_getBlockHash', [0]);
  const version = await rpc.callRetry('state_getRuntimeVersion', []);
  codecs = await getTypedCodecs(acala);
  offline = await getOfflineApi({ ...acala, genesis });
  const descSpec = offline.constants.System.Version.spec_version;
  log(`chain ${version.specName} spec ${version.specVersion}, descriptors spec ${descSpec}, genesis ${genesis}`);
  if (version.specName !== 'acala') throw new Error(`not an Acala chain: ${version.specName}`);
  if (version.specVersion !== descSpec) throw new Error(`runtime spec ${version.specVersion} != descriptor spec ${descSpec}`);

  // start the scan at the current finalized head (all our transactions are signed after this point)
  const head0 = await rpc.callRetry('chain_getFinalizedHead', []);
  scanned = parseInt((await rpc.callRetry('chain_getHeader', [head0])).number, 16);
  finalized.hash = head0; finalized.number = scanned;
  trackerLoop();

  const alice = await readAccount(seed.pub, finalized.hash);
  log(`seed ${seed.address} nonce ${alice.nonce} free ${alice.free}`);
  if (alice.free < FUND * BigInt(N_ACCOUNTS) * 2n) throw new Error('seed account is not funded enough');

  // fund the test accounts (top up to FUND) with plain transfers from the seed, sequential nonces
  const fundTxs = [];
  let nonce = alice.nonce;
  for (const a of accounts) {
    const cur = await readAccount(a.pub, finalized.hash);
    if (cur.free >= FUND / 2n) { log(`  ${a.path} already funded (${cur.free})`); continue; }
    const s = await sign(offline.tx.Balances.transfer_keep_alive({ dest: Enum('Id', a.address), value: FUND }), seed, nonce);
    const t = { label: `fund ${a.path}`, nonce: nonce++, ...s, attempts: 0, transportRetries: 0, rebroadcasts: 0 };
    byHash.set(t.hash, t); fundTxs.push(t);
  }
  for (const t of fundTxs) if (!(await submitUntilAck(t))) throw new Error(`funding rejected: ${t.rejected}`);
  const end = Date.now() + SETUP_TIMEOUT_MS;
  let lastRb = Date.now();
  while (fundTxs.some((t) => !t.block)) {
    if (Date.now() > end) throw new Error('funding not finalized in time');
    if (Date.now() - lastRb > 20_000) { lastRb = Date.now(); for (const t of fundTxs.filter((x) => !x.block)) await rpc.call('author_submitExtrinsic', [t.hex], SUBMIT_TIMEOUT_MS).catch(() => {}); }
    await sleep(300);
  }
  const bad = fundTxs.filter((t) => !t.ok);
  if (bad.length) throw new Error(`funding failed in block: ${bad.map((t) => t.dispatchError).join('; ')}`);
  if (fundTxs.length) log(`funded ${fundTxs.length} accounts (finalized #${fundTxs.map((t) => t.block.number).join(',#')})`);
  // wait until the tracker has seen the funding blocks as finalized, then read lane nonces at finalized
  const lanes = [];
  for (const a of accounts) {
    const st = await readAccount(a.pub, finalized.hash);
    if (st.free < SUPPLY * 20n) throw new Error(`${a.path} not funded: ${st.free}`);
    lanes.push(new Lane(a, st.nonce));
    log(`  lane ${a.path} ${a.address} nonce ${st.nonce} free ${st.free}`);
  }
  return lanes;
}

// ---- load ---------------------------------------------------------------------------------------
async function load(lanes) {
  rpc.onReconnect = () => { rebroadcast('reconnect').catch(() => {}); };
  const intervalMs = 1000 / RATE;
  const T0 = Date.now() + 1000;
  const swapCall = () => offline.tx.Dex.swap_with_exact_supply({
    path: [Enum('Token', Enum('ACA')), Enum('Token', Enum('AUSD'))], supply_amount: SUPPLY, min_target_amount: 0n,
  });
  log(`load: ${N_TX} x Dex.swap_with_exact_supply at ${RATE} tx/s for ${DURATION_S}s from ${N_ACCOUNTS} accounts`);
  for (let i = 0; i < N_TX; i++) {
    const due = T0 + i * intervalMs;
    if (due > Date.now()) await sleep(due - Date.now());
    const lane = lanes[i % N_ACCOUNTS];
    if (lane.halted) { log(`  slot ${i}: lane ${lane.key.path} halted (${lane.halted}); not signing`); continue; }
    const nonce = lane.nonce;
    const s = await sign(swapCall(), lane.key, nonce); // offline: cannot fail on the network
    lane.nonce++;
    const tx = { label: `#${i} ${lane.key.path} n${nonce}`, slot: i, dueMs: due, signer: lane.key.address, nonce, ...s,
      attempts: 0, transportRetries: 0, rebroadcasts: 0 };
    txs.push(tx); byHash.set(tx.hash, tx);
    lane.push(tx);
    if (i % 10 === 0) log(`  slot ${i} signed at finalized #${s.at.number}; sent so far ${txs.filter((t) => t.firstSubmitMs).length}, finalized ${txs.filter((t) => t.block).length}`);
  }
  return { T0, intervalMs };
}

// ---- main ---------------------------------------------------------------------------------------
let lanes;
try { lanes = await setup(); }
catch (e) {
  log('SETUP FAILED:', e.message);
  fs.writeFileSync(path.join(outDir, 'summary.json'), JSON.stringify({ verdict: 'SETUP FAILED', error: e.message }, null, 2));
  trackerStop = true; rpc.close(); logFile.end(); process.exitCode = 3;
  setTimeout(() => process.exit(3), 200);
  await new Promise(() => {});
}

const { T0 } = await load(lanes);
const windowEnd = T0 + DURATION_S * 1000;

// wait for lanes to drain and every sent tx to be included, bounded by the grace period
log('load window done; waiting for inclusion');
let lastRb = Date.now();
for (;;) {
  const sent = txs.filter((t) => t.firstSubmitMs != null);
  const lastFirst = Math.max(windowEnd, ...sent.map((t) => t.firstSubmitMs));
  const queued = lanes.reduce((n, l) => n + (l.halted ? 0 : l.queue.length), 0);
  const open = sent.filter((t) => !t.block && !t.rejected);
  if (!queued && !open.length) break;
  if (Date.now() > lastFirst + RESOLVE_GRACE_MS) { log(`grace period over: ${open.length} unincluded, ${queued} still queued`); break; }
  if (Date.now() - lastRb > 20_000) { lastRb = Date.now(); await rebroadcast('periodic').catch(() => {}); }
  await sleep(500);
}

// ---- reconcile ----------------------------------------------------------------------------------
// make sure the scan covers the latest finalized head
for (let i = 0; i < 5; i++) { try { await trackerTick(); break; } catch { await sleep(1000); } }
trackerStop = true;
const sent = txs.filter((t) => t.firstSubmitMs != null);
const finalNonce = new Map();
for (const a of accounts) {
  try { finalNonce.set(a.address, (await readAccount(a.pub, finalized.hash, 15_000)).nonce); } catch { /* unknown -> no proof */ }
}
for (const t of sent) {
  if (t.block) { t.category = t.ok ? 'finalized-ok' : 'failed-in-block'; continue; }
  const fn = finalNonce.get(t.signer);
  const noncePassed = fn != null && fn > t.nonce;
  const expired = finalized.number >= t.at.number + MORTALITY_PERIOD;
  // "lost" needs proof that the chain had it and dropped it: the node acknowledged it, our scan covered
  // every finalized block since signing, and the nonce moved past it or its mortality ran out.
  if (t.ackMs && scanned >= finalized.number && (noncePassed || expired)) {
    t.category = 'lost'; t.lostProof = noncePassed ? `finalized nonce ${fn} > ${t.nonce}` : `finalized #${finalized.number} >= birth #${t.at.number} + ${MORTALITY_PERIOD}`;
  } else {
    t.category = 'unresolved';
    t.unresolvedWhy = t.rejected ? `node rejected: ${t.rejected}` : !t.ackMs ? 'never acknowledged by node (our send problem)' : `not in finalized chain up to #${finalized.number}; nonce/mortality not yet past`;
  }
}

const count = (c) => sent.filter((t) => t.category === c).length;
const cats = { 'finalized-ok': count('finalized-ok'), 'failed-in-block': count('failed-in-block'), lost: count('lost'), unresolved: count('unresolved') };
const inWindow = sent.filter((t) => t.firstSubmitMs <= windowEnd + 2000).length;
const required = Math.ceil(0.95 * RATE * DURATION_S);
const delivered = inWindow >= required;
const pct = (arr, p) => { if (!arr.length) return null; const s = [...arr].sort((a, b) => a - b); return s[Math.min(s.length - 1, Math.ceil(p * s.length) - 1)]; };
const included = sent.filter((t) => t.block);
for (const t of included) {
  t.latencyMs = t.finalizedSeenMs - t.firstSubmitMs;
  // chain-attributable latency: from the submit attempt the node accepted (excludes our resend delays)
  t.chainLatencyMs = t.finalizedSeenMs - (t.ackAttemptMs ?? t.firstSubmitMs);
}
const p95 = pct(included.map((t) => t.latencyMs), 0.95);
const chainSample = included.filter((t) => !t.observedLate).map((t) => t.chainLatencyMs);
const p95Chain = pct(chainSample, 0.95);
const firsts = sent.map((t) => t.firstSubmitMs);
const span = firsts.length > 1 ? (Math.max(...firsts) - Math.min(...firsts)) / 1000 : 0;
const deliveredRate = sent.length / DURATION_S;

const ourProblems = {
  transportRetries: sent.reduce((n, t) => n + t.transportRetries, 0),
  reconnects: Math.max(0, rpc.connects - 1), drops: rpc.drops,
  notSigned: N_TX - txs.length, signedNotSent: txs.length - sent.length,
  lanesHalted: lanes.filter((l) => l.halted).map((l) => `${l.key.path}: ${l.halted}`),
  latenciesObservedLate: included.filter((t) => t.observedLate).length,
};

let verdict, exit, reasons = [];
if (cats.lost > 0) reasons.push(`${cats.lost} transaction(s) provably lost`);
if (p95Chain != null && p95Chain >= P95_LIMIT_MS) reasons.push(`p95 latency ${p95Chain} ms >= ${P95_LIMIT_MS} ms`);
if (reasons.length) { verdict = 'FAIL'; exit = 1; }
else {
  if (!delivered) reasons.push(`load not delivered: ${inWindow}/${N_TX} first-submitted within window+2s (need ${required})`);
  if (cats.unresolved) reasons.push(`${cats.unresolved} unresolved`);
  if (!included.length) reasons.push('no latency sample');
  if (reasons.length) { verdict = "CAN'T TELL"; exit = 4; } else { verdict = 'PASS'; exit = 0; }
}

const jsonl = sent.map((t) => JSON.stringify({
  hash: t.hash, category: t.category, signer: t.signer, nonce: t.nonce, firstSubmitMs: t.firstSubmitMs,
  slot: t.slot, block: t.block?.number ?? null, blockHash: t.block?.hash ?? null, latencyMs: t.latencyMs ?? null,
  attempts: t.attempts, transportRetries: t.transportRetries, birthBlock: t.at.number,
  ...(t.dispatchError ? { dispatchError: t.dispatchError } : {}), ...(t.lostProof ? { lostProof: t.lostProof } : {}),
  ...(t.unresolvedWhy ? { unresolvedWhy: t.unresolvedWhy } : {}),
}, big)).join('\n') + '\n';
fs.writeFileSync(path.join(outDir, 'transactions.jsonl'), jsonl);

const summary = {
  verdict, exitCode: exit, reasons,
  load: { call: 'Dex.swap_with_exact_supply ACA->AUSD', supply: SUPPLY.toString(), rate: RATE, durationS: DURATION_S, accounts: N_ACCOUNTS, planned: N_TX },
  sent: sent.length, ...cats,
  firstSubmittedInWindow: inWindow, required, delivered,
  deliveredRateTxPerS: +deliveredRate.toFixed(4), firstSubmitSpanS: +span.toFixed(1),
  p95LatencyMs: p95, p95ChainLatencyMs: p95Chain, maxLatencyMs: included.length ? Math.max(...included.map((t) => t.latencyMs)) : null,
  medianLatencyMs: pct(included.map((t) => t.latencyMs), 0.5),
  finalizedHead: finalized.number, ourProblems,
};
fs.writeFileSync(path.join(outDir, 'summary.json'), JSON.stringify(summary, big, 2));

log('---------------- result ----------------');
log(`sent ${sent.length}: finalized-ok ${cats['finalized-ok']}, failed-in-block ${cats['failed-in-block']}, lost ${cats.lost}, unresolved ${cats.unresolved}`);
log(`delivered ${inWindow}/${N_TX} within window+2s (need ${required}); delivered rate ${deliveredRate.toFixed(3)} tx/s (target ${RATE})`);
log(`latency first submit -> finalized: p50 ${summary.medianLatencyMs} ms, p95 ${p95} ms, max ${summary.maxLatencyMs} ms; chain-attributable p95 ${p95Chain} ms`);
log(`our side: ${JSON.stringify(ourProblems)}`);
for (const t of sent.filter((x) => x.category === 'failed-in-block')) log(`  failed-in-block ${t.label} ${t.dispatchError}`);
log(`VERDICT: ${verdict}${reasons.length ? ' — ' + reasons.join('; ') : ''} (exit ${exit})`);
log(`files: ${path.join(outDir, 'transactions.jsonl')}, ${path.join(outDir, 'summary.json')}, ${path.join(outDir, 'run.log')}`);
rpc.close(); logFile.end();
setTimeout(() => process.exit(exit), 200);
