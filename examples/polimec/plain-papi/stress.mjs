// Polimec stress test: LinearRelease.vested_transfer at 0.25 tx/s for 200 s from 4 derived accounts.
// usage: node stress.mjs <ws-endpoint> <output-dir>
// Exit: 0 PASS, 1 FAIL (chain missed a threshold), 3 setup failed before load, 4 CAN'T TELL.
//
// Design:
// - Transport: PAPI's ws provider (auto-reconnects) with our own request ids, per-request timeout and retry.
// - Signing: PAPI OfflineApi built from the live chain's metadata + genesis, so signing never touches the
//   network. Every tx is mortal (period 64) and anchored at the latest finalized block we have seen.
// - Nonces: per account, read from finalized storage once, then incremented only after the node accepted the
//   previous tx (or said it is already known/outdated). A failed send is retried with the identical bytes;
//   later nonces wait behind it. A tx is signed exactly once per nonce.
// - Inclusion: a poller walks every finalized block (chain_getFinalizedHead / chain_getBlockHash(n)) and
//   hashes its extrinsics, so nothing is missed across reconnects. Success/failure comes from System.Events.
import fs from 'node:fs';
import path from 'node:path';
import { Enum, AccountId, getOfflineApi } from 'polkadot-api';
import { getWsProvider } from 'polkadot-api/ws';
import { getTxCreator } from 'polkadot-api/tx-creator';
import { Blake2256, decAnyMetadata, unifyMetadata } from '@polkadot-api/substrate-bindings';
import { getDynamicBuilder, getLookupFn } from '@polkadot-api/metadata-builders';
import { sr25519CreateDerive } from '@polkadot-labs/hdkd';
import { DEV_PHRASE, entropyToMiniSecret, mnemonicToEntropy } from '@polkadot-labs/hdkd-helpers';

const [ENDPOINT, OUT] = process.argv.slice(2);
if (!ENDPOINT || !OUT) { console.error('usage: node stress.mjs <ws-endpoint> <output-dir>'); process.exit(3); }

const RATE = 0.25, DURATION_S = 200, N_TX = Math.round(RATE * DURATION_S), N_ACCOUNTS = 4;
const PERIOD = 32;               // mortality period (blocks); ~128 s of load blocks, well past the 60 s p95 limit
const REBROADCAST_MS = 12_000;   // resend identical bytes if accepted but not finalized after this long
const P95_LIMIT_MS = 60_000;
const FUND = 10n ** 15n;          // per test account (PLMC has 10 decimals)
const LOCKED = 10n ** 12n, PER_BLOCK = 10n ** 9n;
const DRAIN_MS = 180_000;         // max wait after the last send for txs to reach a finalized block

fs.mkdirSync(OUT, { recursive: true });
const logFile = fs.createWriteStream(path.join(OUT, 'run.log'));
const log = (...a) => { const s = `${new Date().toISOString()} ${a.join(' ')}`; console.log(s); logFile.write(s + '\n'); };
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const hex = (u8) => '0x' + Buffer.from(u8).toString('hex');
const unhex = (h) => Uint8Array.from(Buffer.from(h.replace(/^0x/, ''), 'hex'));

// Tool-side counters (never reported as chain problems).
const tool = { reconnects: 0, rpcTimeouts: 0, rpcTransportErrors: 0, sendRetries: 0, rebroadcasts: 0, nudgeTxs: 0, pollErrors: 0 };

// ---------------------------------------------------------------- JSON-RPC over PAPI's ws provider
let nextId = 1;
const pending = new Map();
let everConnected = false;
const provider = getWsProvider(ENDPOINT, {
  onStatusChanged: (s) => {
    if (s.type === 'CONNECTED') { if (everConnected) { tool.reconnects++; log('[tool] reconnected'); } everConnected = true; }
    if (s.type === 'CLOSE' || s.type === 'ERROR') {
      if (everConnected) log(`[tool] connection ${s.type.toLowerCase()}`);
      // Fail in-flight requests fast; callers retry (submits retry with identical bytes).
      for (const [id, p] of pending) { pending.delete(id); p.reject(Object.assign(new Error('connection lost'), { transport: true })); }
    }
  },
});
const conn = provider((msg) => {
  const p = pending.get(msg.id);
  if (!p) return;
  pending.delete(msg.id);
  if ('error' in msg) p.reject(Object.assign(new Error(msg.error.message + (msg.error.data ? `: ${JSON.stringify(msg.error.data)}` : '')), { rpc: msg.error }));
  else p.resolve(msg.result);
});
function rpc(method, params = [], timeoutMs = 20_000) {
  return new Promise((resolve, reject) => {
    const id = `s${nextId++}`;
    const t = setTimeout(() => { pending.delete(id); tool.rpcTimeouts++; reject(Object.assign(new Error(`timeout ${method}`), { transport: true })); }, timeoutMs);
    pending.set(id, { resolve: (v) => { clearTimeout(t); resolve(v); }, reject: (e) => { clearTimeout(t); if (e.transport) tool.rpcTransportErrors++; reject(e); } });
    try { conn.send({ jsonrpc: '2.0', id, method, params }); } catch (e) { pending.delete(id); clearTimeout(t); reject(Object.assign(e, { transport: true })); }
  });
}
// Read-only calls: retry until they succeed or the deadline passes.
async function rpcRetry(method, params = [], deadlineMs = 120_000) {
  const end = Date.now() + deadlineMs;
  for (let i = 0; ; i++) {
    try { return await rpc(method, params); } catch (e) {
      if (Date.now() > end || (e.rpc && !e.transport)) throw e;
      await sleep(Math.min(500 * 2 ** i, 5000));
    }
  }
}

// ---------------------------------------------------------------- chain context
let codecs, offline, ss58, genesis;
const EVENTS_KEY = '0x26aa394eea5630e07c48ae0c9558cef780d41e5e16056765bc8461851072c9d7';
const derive = sr25519CreateDerive(entropyToMiniSecret(mnemonicToEntropy(DEV_PHRASE)));
function account(pathStr) {
  const kp = derive(pathStr);
  return { path: pathStr, pub: kp.publicKey, signer: getTxCreator(kp.publicKey, 'Sr25519', kp.sign), address: null, nonce: 0, txs: [], halted: false };
}

async function finalizedNonce(acc, at) {
  const v = await rpcRetry('state_getStorage', [codecs.account.keys.enc(acc.address), at]);
  return v ? Number(codecs.account.value.dec(v).nonce) : 0;
}

// ---------------------------------------------------------------- finalized block tracker
const fin = { hash: null, number: -1 };
const included = new Map(); // txHash -> { blockNumber, blockHash, index, ok, seenAt }
let scanned = -1;           // highest block number scanned
let startBlock = 0;
let pollStop = false;
async function scanTo() {
  const head = await rpc('chain_getFinalizedHead');
  const hdr = await rpc('chain_getHeader', [head]);
  const num = parseInt(hdr.number, 16);
  for (let n = scanned + 1; n <= num; n++) {
    const h = n === num ? head : await rpc('chain_getBlockHash', [n]);
    const blk = await rpc('chain_getBlock', [h]);
    const exts = blk.block.extrinsics;
    let failedIdx = new Set();
    const evRaw = await rpc('state_getStorage', [EVENTS_KEY, h]);
    let evOk = true;
    try {
      for (const ev of evRaw ? codecs.events.value.dec(evRaw) : []) {
        if (ev.phase.type === 'ApplyExtrinsic' && ev.event.type === 'System' && ev.event.value.type === 'ExtrinsicFailed') failedIdx.add(ev.phase.value);
      }
    } catch (e) { evOk = false; log(`[tool] could not decode events of block ${n} (its load txs, if any, become unresolved): ${e.message}`); }
    const now = Date.now();
    exts.forEach((x, i) => {
      const th = hex(Blake2256(unhex(x)));
      if (!included.has(th)) included.set(th, { blockNumber: n, blockHash: h, index: i, ok: evOk ? !failedIdx.has(i) : null, seenAt: now });
    });
    scanned = n;
  }
  if (num >= fin.number) { fin.hash = head; fin.number = num; }
}
async function pollLoop() {
  while (!pollStop) {
    try { await scanTo(); } catch (e) { tool.pollErrors++; }
    await sleep(500);
  }
}
async function waitFinalized(timeoutMs = 60_000) {
  const end = Date.now() + timeoutMs;
  while (!fin.hash) { if (Date.now() > end) throw new Error('no finalized block'); await sleep(200); }
}

// ---------------------------------------------------------------- sign + send
// classify a submit error: 'known' (in pool/chain already), 'retry' (transient / tool side), 'reject' (chain refused)
function classify(e) {
  if (e.transport) return 'retry';
  const m = (e.message || '').toLowerCase();
  if (/already imported|outdated|stale|temporarily banned|known/.test(m)) return 'known';
  if (/future|exhausts|resources|priority is too low|too many|immediately dropped/.test(m)) return 'retry';
  return 'reject';
}

async function signTx(acc, call, kind) {
  await waitFinalized();
  const at = { hash: fin.hash, number: fin.number };
  const nonce = acc.nonce;
  const bytes = await call.create(acc.signer, { nonce, mortality: { mortal: true, period: PERIOD, at } });
  acc.nonce++; // this nonce is now bound to these bytes forever
  const tx = { kind, signer: acc.address, signerPath: acc.path, nonce, hex: hex(bytes), hash: hex(Blake2256(bytes)),
    birth: at.number, birthHash: at.hash, firstSubmitMs: null, lastSendMs: 0, attempts: 0, rebroadcasts: 0, accepted: false, rejected: null, lastError: null };
  acc.txs.push(tx);
  return tx;
}

// Send identical bytes until the node accepts them (or says it already knows them).
async function sendUntilAccepted(tx, giveUpMs = 90_000) {
  const start = Date.now();
  let rejects = 0;
  for (let i = 0; ; i++) {
    if (tx.firstSubmitMs === null) tx.firstSubmitMs = Date.now();
    tx.attempts++;
    tx.lastSendMs = Date.now();
    try {
      await rpc('author_submitExtrinsic', [tx.hex], 30_000);
      tx.accepted = true; return true;
    } catch (e) {
      tx.lastError = e.message;
      const c = classify(e);
      if (c === 'known') { tx.accepted = true; return true; }
      tool.sendRetries++;
      if (c === 'reject') rejects++;
      log(`[tool] send ${tx.kind} ${tx.signerPath}#${tx.nonce} attempt ${tx.attempts} failed (${c}): ${e.message}`);
      // A tx we already sent may have been included meanwhile; then it is resolved.
      if (included.has(tx.hash)) { tx.accepted = true; return true; }
      if ((c === 'reject' && rejects >= 5) || Date.now() - start > giveUpMs) { tx.rejected = c === 'reject' ? e.message : null; return false; }
      await sleep(Math.min(1000 * 2 ** Math.min(i, 3), 8000));
    }
  }
}

async function waitIncluded(hashes, timeoutMs) {
  const end = Date.now() + timeoutMs;
  while (Date.now() < end) {
    if (hashes.every((h) => included.has(h))) return true;
    await sleep(300);
  }
  return false;
}

// ---------------------------------------------------------------- main
function setupFail(msg) { log(`SETUP FAILED: ${msg}`); log('VERDICT: SETUP FAILED (exit 3)'); try { conn.disconnect(); } catch {} process.exit(3); }

const alice = account('//Alice');
const accounts = Array.from({ length: N_ACCOUNTS }, (_, i) => account(`//Alice//polimec-stress//${i}`));

try {
  log(`endpoint ${ENDPOINT}; load ${N_TX} x LinearRelease.vested_transfer at ${RATE} tx/s for ${DURATION_S} s from ${N_ACCOUNTS} accounts`);
  genesis = await rpcRetry('chain_getBlockHash', [0], 60_000);
  const metaHex = await rpcRetry('state_getMetadata', [], 60_000);
  const metaRaw = unhex(metaHex);
  const meta = unifyMetadata(decAnyMetadata(metaRaw));
  const builder = getDynamicBuilder(getLookupFn(meta));
  codecs = { account: builder.buildStorage('System', 'Account'), events: builder.buildStorage('System', 'Events') };
  ss58 = Number(builder.buildConstant('System', 'SS58Prefix').dec(meta.pallets.find((p) => p.name === 'System').constants.find((c) => c.name === 'SS58Prefix').value));
  if (!meta.pallets.find((p) => p.name === 'LinearRelease')) setupFail('runtime has no LinearRelease pallet');
  offline = await getOfflineApi({ genesis, getMetadata: async () => metaRaw });
  const rv = await rpcRetry('state_getRuntimeVersion');
  log(`chain ${await rpcRetry('system_chain')} genesis ${genesis} spec ${rv.specName}/${rv.specVersion} ss58 ${ss58}`);
  for (const a of [alice, ...accounts]) a.address = AccountId(ss58).dec(a.pub);

  pollLoop();
  await waitFinalized(60_000);
  startBlock = scanned; // blocks <= startBlock were scanned too; fine.
  alice.nonce = await finalizedNonce(alice, fin.hash);
  for (const a of accounts) a.nonce = await finalizedNonce(a, fin.hash);
  log(`finalized #${fin.number}; //Alice nonce ${alice.nonce}; test accounts: ${accounts.map((a) => `${a.address}(n${a.nonce})`).join(' ')}`);

  // Fund the test accounts from //Alice (signed and sent the same way as the load).
  const fundTxs = [];
  for (const a of accounts) {
    const tx = await signTx(alice, offline.tx.Balances.transfer_keep_alive({ dest: Enum('Id', a.address), value: FUND }), 'setup');
    if (!(await sendUntilAccepted(tx))) setupFail(`funding transfer to ${a.path} not accepted: ${tx.lastError}`);
    fundTxs.push(tx);
  }
  if (!(await waitIncluded(fundTxs.map((t) => t.hash), 120_000))) setupFail('funding transfers did not reach a finalized block within 120 s');
  const bad = fundTxs.filter((t) => !included.get(t.hash).ok);
  if (bad.length) setupFail(`funding transfer failed in block: ${bad.map((t) => t.hash).join(', ')}`);
  for (const a of accounts) a.nonce = await finalizedNonce(a, fin.hash);
  log(`funded ${N_ACCOUNTS} accounts with ${FUND} each`);
} catch (e) {
  setupFail(e.stack || e.message);
}

// ---------------------------------------------------------------- load
const t0 = Date.now() + 1000;
const windowEnd = t0 + DURATION_S * 1000;
log(`load start; window ${DURATION_S} s (+2 s grace)`);
const intervalMs = 1000 / RATE;
async function worker(ai) {
  const acc = accounts[ai];
  const target = accounts[(ai + 1) % N_ACCOUNTS].address;
  for (let k = ai; k < N_TX; k += N_ACCOUNTS) {
    const due = t0 + k * intervalMs;
    if (Date.now() < due) await sleep(due - Date.now());
    if (acc.halted) return;
    const now = fin.number;
    let tx;
    try {
      const call = offline.tx.LinearRelease.vested_transfer({
        target, // AccountId (not MultiAddress)
        schedule: { locked: LOCKED, per_block: PER_BLOCK, starting_block: now + 10 },
        reason: Enum('ParachainStaking', Enum('StakingDelegator')),
      });
      tx = await signTx(acc, call, 'load');
    } catch (e) { log(`[tool] signing failed for ${acc.path}: ${e.message}`); acc.halted = true; return; }
    tx.seq = k;
    const ok = await sendUntilAccepted(tx, Math.max(30_000, windowEnd + 60_000 - Date.now()));
    if (!ok) { log(`[tool] ${acc.path}#${tx.nonce} never accepted (${tx.lastError}); halting this account so no later nonce passes the gap`); acc.halted = true; return; }
  }
}
// Rebroadcast: a tx the node accepted but that has not reached a finalized block after REBROADCAST_MS is
// sent again with its identical bytes (the node may have dropped it). Never re-signed. In nonce order.
let rebroadcastStop = false;
async function rebroadcastLoop() {
  while (!rebroadcastStop) {
    for (const a of accounts) for (const t of a.txs) {
      if (rebroadcastStop) return;
      if (t.kind !== 'load' || !t.accepted || included.has(t.hash) || fin.number >= t.birth + PERIOD) continue;
      if (Date.now() - t.lastSendMs < REBROADCAST_MS) continue;
      t.lastSendMs = Date.now(); t.rebroadcasts++; tool.rebroadcasts++;
      try { await rpc('author_submitExtrinsic', [t.hex], 15_000); log(`[tool] rebroadcast ${a.path}#${t.nonce} ${t.hash}: accepted`); }
      catch (e) { log(`[tool] rebroadcast ${a.path}#${t.nonce} ${t.hash}: ${e.message}`); }
    }
    await sleep(1000);
  }
}
const rebroadcasting = rebroadcastLoop();
await Promise.all(accounts.map((_, i) => worker(i)));
const loadTxs = accounts.flatMap((a) => a.txs).filter((t) => t.kind === 'load').sort((a, b) => a.seq - b.seq);
const lastSend = Date.now();
log(`load done: ${loadTxs.length} signed/submitted; waiting up to ${DRAIN_MS / 1000} s for finalization`);

// ---------------------------------------------------------------- drain + reconcile
// Wait until every accepted tx is in a finalized block or provably can no longer be (nonce passed or mortality
// expired). This chain only builds a block when a tx arrives, so if nothing happens for a while we send a
// small //Alice transfer ("nudge", not part of the load) to let block numbers advance past mortality.
let lastBlock = fin.number, lastBlockAt = Date.now(), lastNudge = 0, nudgeOk = true;
while (Date.now() - lastSend < DRAIN_MS) {
  const nonces = {};
  for (const a of accounts) { try { nonces[a.address] = await finalizedNonce(a, fin.hash); } catch { nonces[a.address] = -1; } }
  const open = loadTxs.filter((t) => t.accepted && !included.has(t.hash) && fin.number < t.birth + PERIOD && !(nonces[t.signer] > t.nonce));
  if (!open.length) break;
  if (fin.number !== lastBlock) { lastBlock = fin.number; lastBlockAt = Date.now(); }
  if (nudgeOk && Date.now() - lastBlockAt > 5000 && Date.now() - lastNudge > 3000) {
    lastNudge = Date.now();
    try {
      const n = await signTx(alice, offline.tx.Balances.transfer_keep_alive({ dest: Enum('Id', accounts[0].address), value: 10n ** 10n }), 'nudge');
      if (await sendUntilAccepted(n, 20_000)) tool.nudgeTxs++;
      else { nudgeOk = false; log('[tool] nudge not accepted; no more nudges (would leave a nonce gap)'); }
    } catch (e) { nudgeOk = false; log(`[tool] nudge failed: ${e.message}`); }
  }
  await sleep(500);
}
rebroadcastStop = true;
await rebroadcasting;
pollStop = true;
await sleep(600);
// One last full scan up to the current finalized head (retrying through reconnects).
for (let i = 0; i < 20; i++) { try { await scanTo(); break; } catch { await sleep(1000); } }

const finalNonce = {};
for (const a of accounts) { try { finalNonce[a.address] = await finalizedNonce(a, fin.hash); } catch { finalNonce[a.address] = null; } }

const rows = [];
const cats = { 'finalized-ok': 0, 'failed-in-block': 0, lost: 0, rejected: 0, unresolved: 0 };
const latencies = [];
for (const t of loadTxs) {
  const inc = included.get(t.hash);
  let category, proof = null;
  if (inc) {
    category = inc.ok === null ? 'unresolved' : inc.ok ? 'finalized-ok' : 'failed-in-block';
    if (inc.ok !== null) latencies.push(inc.seenAt - t.firstSubmitMs);
  } else {
    const fn = finalNonce[t.signer];
    const expired = fin.number >= t.birth + PERIOD;
    if (fn !== null && fn > t.nonce) { category = t.accepted ? 'lost' : 'rejected'; proof = `finalized nonce ${fn} > ${t.nonce} at #${fin.number}, hash absent from blocks #${startBlock}..#${scanned}`; }
    else if (expired) { category = t.accepted ? 'lost' : 'rejected'; proof = `mortality expired (birth #${t.birth} + ${PERIOD} <= #${fin.number}), hash absent from blocks #${startBlock}..#${scanned}`; }
    else category = t.accepted ? 'unresolved' : 'rejected';
  }
  cats[category]++;
  rows.push({ hash: t.hash, category, signer: t.signer, nonce: t.nonce, firstSubmitMs: t.firstSubmitMs,
    blockNumber: inc?.blockNumber ?? null, blockHash: inc?.blockHash ?? null, latencyMs: inc ? inc.seenAt - t.firstSubmitMs : null,
    attempts: t.attempts, rebroadcasts: t.rebroadcasts, accepted: t.accepted, rejectReason: t.rejected, lastError: t.accepted ? null : t.lastError, proof });
}
fs.writeFileSync(path.join(OUT, 'transactions.jsonl'), rows.map((r) => JSON.stringify(r)).join('\n') + (rows.length ? '\n' : ''));

latencies.sort((a, b) => a - b);
const p95 = latencies.length ? latencies[Math.min(latencies.length - 1, Math.ceil(0.95 * latencies.length) - 1)] : null;
const sent = loadTxs.length;
const inWindow = loadTxs.filter((t) => t.firstSubmitMs !== null && t.firstSubmitMs <= windowEnd + 2000).length;
const required = Math.ceil(0.95 * N_TX);
const delivered = inWindow >= required;
const includedCount = cats['finalized-ok'] + cats['failed-in-block'];

let verdict, code;
if ((p95 !== null && p95 >= P95_LIMIT_MS) || cats.lost > 0) { verdict = 'FAIL'; code = 1; }
else if (!delivered || cats.unresolved > 0 || cats.rejected > 0 || p95 === null) { verdict = "CAN'T TELL"; code = 4; }
else { verdict = 'PASS'; code = 0; }

const reasons = [];
if (p95 !== null && p95 >= P95_LIMIT_MS) reasons.push(`chain: p95 latency ${p95} ms >= ${P95_LIMIT_MS} ms`);
if (cats.lost) reasons.push(`chain: ${cats.lost} transaction(s) proven lost`);
if (!delivered) reasons.push(`tool: load not delivered (${inWindow}/${N_TX} first-submitted within window+2s, need ${required})`);
if (cats.unresolved) reasons.push(`${cats.unresolved} unresolved`);
if (cats.rejected) reasons.push(`${cats.rejected} rejected at submit (never accepted by the node)`);

const summary = {
  verdict, exitCode: code, reasons,
  target: { rate: RATE, durationS: DURATION_S, transactions: N_TX, accounts: N_ACCOUNTS },
  sent, ...cats,
  firstSubmittedInWindow: inWindow, loadDelivered: delivered,
  offeredRateTxPerS: +(inWindow / DURATION_S).toFixed(4),
  deliveredRateTxPerS: +(includedCount / DURATION_S).toFixed(4),
  latencyMs: { p50: latencies.length ? latencies[Math.ceil(0.5 * latencies.length) - 1] : null, p95, max: latencies.at(-1) ?? null, samples: latencies.length },
  finalizedHead: fin.number, blocksScanned: [startBlock, scanned],
  toolEvents: tool,
};
fs.writeFileSync(path.join(OUT, 'summary.json'), JSON.stringify(summary, null, 2) + '\n');

log('---------------- result ----------------');
log(`sent ${sent} | finalized-ok ${cats['finalized-ok']} | failed-in-block ${cats['failed-in-block']} | lost ${cats.lost} | rejected ${cats.rejected} | unresolved ${cats.unresolved}`);
log(`first-submitted within window+2s: ${inWindow}/${N_TX} (need ${required}) -> load ${delivered ? 'delivered' : 'NOT delivered'}`);
log(`offered rate ${summary.offeredRateTxPerS} tx/s; delivered (included) rate ${summary.deliveredRateTxPerS} tx/s`);
log(`latency first submit -> finalized block: p50 ${summary.latencyMs.p50} ms, p95 ${p95} ms, max ${summary.latencyMs.max} ms (${latencies.length} samples)`);
log(`tool events (not chain problems): ${JSON.stringify(tool)}`);
log(`files: ${path.join(OUT, 'transactions.jsonl')}, ${path.join(OUT, 'summary.json')}, ${path.join(OUT, 'run.log')}`);
log(`VERDICT: ${verdict} (exit ${code})${reasons.length ? ' — ' + reasons.join('; ') : ''}`);
try { conn.disconnect(); } catch {}
await new Promise((r) => logFile.end(r));
process.exit(code);
