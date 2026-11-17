// Canggu stress test: Payroll.batch_pay at RATE tx/s for DURATION s from ACCOUNTS derived accounts.
// usage: node stress.mjs <ws-endpoint> <output-dir>
// Exit: 0 PASS, 1 FAIL (chain missed a threshold), 4 CAN'T TELL, 3 setup failed before the load.
//
// Design
// - All chain I/O goes through one reconnecting legacy JSON-RPC WebSocket (Rpc below). PAPI is used to build,
//   sign (getOfflineApi with the chain's live metadata) and decode (getTypedCodecs).
// - Every tx is signed when its slot comes up, against the latest finalized block we know (mortality.at =
//   that block, period MORTAL_PERIOD), with an explicit nonce. Per account, nonces go strictly +1, and tx n+1
//   is not signed or sent until tx n's submission was acknowledged by the node. Failed sends / reconnects
//   resend the identical bytes.
// - A poller follows the finalized head and scans every finalized block from the load start, matching
//   extrinsic hashes (blake2b-256 of the bytes) and reading System.Events for success/failure.
//
// Categories (sum to sent):
//   finalized-ok     in a finalized block, ExtrinsicSuccess
//   failed-in-block  in a finalized block, ExtrinsicFailed
//   lost             in no finalized block (all finalized blocks from load start scanned) AND
//                    (the signer's finalized nonce is past it OR the finalized height is past its mortality end)
//                    AND the node acknowledged it at least once (a tx the node never accepted is our problem)
//   unresolved       anything else; `reason` says why (e.g. never-acknowledged, rejected-at-submit, timeout)
import { getOfflineApi, getTypedCodecs, AccountId } from 'polkadot-api';
import { getTxCreator } from 'polkadot-api/tx-creator';
import { canggu } from '@polkadot-api/descriptors';
import { sr25519CreateDerive } from '@polkadot-labs/hdkd';
import { DEV_PHRASE, entropyToMiniSecret, mnemonicToEntropy } from '@polkadot-labs/hdkd-helpers';
import { blake2b } from '@noble/hashes/blake2.js';
import fs from 'node:fs';
import path from 'node:path';

const [ENDPOINT, OUT] = process.argv.slice(2);
if (!ENDPOINT || !OUT) { console.error('usage: node stress.mjs <ws-endpoint> <output-dir>'); process.exit(3); }
const env = (k, d) => Number(process.env[k] ?? d);
const RATE = env('RATE', 5), DURATION = env('DURATION', 120), ACCOUNTS = env('ACCOUNTS', 8);
const RECIPIENTS = 3, MORTAL_PERIOD = 64, P95_LIMIT_MS = 30_000, RECONCILE_MAX_MS = env('RECONCILE_MAX_S', 240) * 1000;
const TOTAL = RATE * DURATION;
const DAG = { customSignedExtensions: { CheckDagParents: { value: [] } } };

fs.mkdirSync(OUT, { recursive: true });
const logFile = fs.createWriteStream(path.join(OUT, 'run.log'));
const t00 = Date.now();
const log = (...a) => {
  const line = `[${((Date.now() - t00) / 1000).toFixed(1).padStart(6)}s] ${a.join(' ')}`;
  console.log(line); logFile.write(line + '\n');
};
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const hex = (u8) => '0x' + Buffer.from(u8).toString('hex');
const unhex = (h) => Uint8Array.from(Buffer.from(h.slice(2), 'hex'));
const txHash = (u8) => hex(blake2b(u8, { dkLen: 32 }));

// ---------- reconnecting JSON-RPC client ----------
class RpcError extends Error { constructor(e) { super(`${e.code}: ${e.message}${e.data ? ' / ' + e.data : ''}`); this.rpc = e; } }
class Rpc {
  constructor(url) { this.url = url; this.id = 0; this.pending = new Map(); this.reconnects = 0; this.connected = false; this.waiters = []; this.closed = false; this.connect(); }
  connect() {
    const ws = new WebSocket(this.url); this.ws = ws;
    ws.onopen = () => { this.connected = true; this.waiters.splice(0).forEach((w) => w()); };
    ws.onmessage = (m) => {
      const d = JSON.parse(m.data); const p = this.pending.get(d.id); if (!p) return;
      this.pending.delete(d.id); clearTimeout(p.timer);
      d.error ? p.reject(new RpcError(d.error)) : p.resolve(d.result);
    };
    ws.onerror = () => {};
    ws.onclose = () => {
      if (this.ws !== ws) return;
      const was = this.connected; this.connected = false;
      for (const [, p] of this.pending) { clearTimeout(p.timer); p.reject(new Error('connection closed')); }
      this.pending.clear();
      if (this.closed) return;
      if (was) { this.reconnects++; log('connection dropped; reconnecting'); }
      setTimeout(() => this.connect(), 500);
    };
  }
  async waitConnected(ms) {
    if (this.connected) return;
    await new Promise((res, rej) => { const t = setTimeout(() => rej(new Error('not connected')), ms); this.waiters.push(() => { clearTimeout(t); res(); }); });
  }
  async call(method, params = [], timeoutMs = 10_000) {
    await this.waitConnected(timeoutMs);
    return new Promise((resolve, reject) => {
      const id = ++this.id;
      const timer = setTimeout(() => { this.pending.delete(id); reject(new Error(`timeout ${method}`)); }, timeoutMs);
      this.pending.set(id, { resolve, reject, timer });
      try { this.ws.send(JSON.stringify({ id, jsonrpc: '2.0', method, params })); } catch (e) { this.pending.delete(id); clearTimeout(timer); reject(e); }
    });
  }
  // retry transport failures (not RPC errors) until deadline
  async callRetry(method, params, deadlineMs = 60_000) {
    const end = Date.now() + deadlineMs;
    for (;;) {
      try { return await this.call(method, params); } catch (e) {
        if (e instanceof RpcError || Date.now() > end) throw e;
        await sleep(300);
      }
    }
  }
  close() { this.closed = true; try { this.ws.close(); } catch {} }
}

// ---------- chain helpers ----------
const ACCOUNT_PREFIX = '0x26aa394eea5630e07c48ae0c9558cef7b99d880ec681799c0cf30e8886371da9'; // System.Account
const EVENTS_KEY = '0x26aa394eea5630e07c48ae0c9558cef780d41e5e16056765bc8461851072c9d7'; // System.Events
const accountKey = (pub) => ACCOUNT_PREFIX + Buffer.from(blake2b(pub, { dkLen: 16 })).toString('hex') + Buffer.from(pub).toString('hex');

let rpc, api, codecs;
async function accountAt(pub, blockHash) {
  const v = await rpc.callRetry('state_getStorage', [accountKey(pub), blockHash]);
  return v ? codecs.query.System.Account.value.dec(v) : { nonce: 0, data: { free: 0n } };
}

// ---------- finality tracking & block scanning ----------
const fin = { hash: null, number: -1 };       // latest finalized we know
const finSeenAt = new Map();                  // block number -> wall ms when we first saw it finalized
const txs = new Map();                        // hash -> record
let scanFrom = null, scannedTo = -1;          // block numbers scanned (inclusive)
let pollerStop = false;

async function pollFinalized() {
  const h = await rpc.call('chain_getFinalizedHead');
  const hdr = await rpc.call('chain_getHeader', [h]);
  const n = parseInt(hdr.number, 16);
  if (n > fin.number) {
    const now = Date.now();
    for (let i = Math.max(fin.number + 1, 0); i <= n; i++) if (!finSeenAt.has(i)) finSeenAt.set(i, now);
    fin.hash = h; fin.number = n;
  }
}
async function scanBlocks() {
  if (scanFrom == null) return;
  while (scannedTo < fin.number) {
    const n = Math.max(scannedTo + 1, scanFrom);
    const bh = await rpc.call('chain_getBlockHash', [n]);
    const blk = await rpc.call('chain_getBlock', [bh]);
    const exts = blk.block.extrinsics;
    let events = null;
    for (let idx = 0; idx < exts.length; idx++) {
      const rec = txs.get(txHash(unhex(exts[idx])));
      if (!rec) continue;
      if (!events) events = codecs.query.System.Events.value.dec(await rpc.call('state_getStorage', [EVENTS_KEY, bh]));
      const ev = events.find((e) => e.phase.type === 'ApplyExtrinsic' && e.phase.value === idx && e.event.type === 'System' &&
        (e.event.value.type === 'ExtrinsicSuccess' || e.event.value.type === 'ExtrinsicFailed'));
      rec.block = n; rec.blockHash = bh; rec.index = idx;
      rec.finalizedMs = finSeenAt.get(n);
      rec.ok = ev?.event.value.type === 'ExtrinsicSuccess';
      if (ev && !rec.ok) rec.error = JSON.stringify(ev.event.value.value.dispatch_error, (k, v) => typeof v === 'bigint' ? v.toString() : v);
      rec.onIncluded?.();
    }
    scannedTo = n;
  }
}
async function poller() {
  while (!pollerStop) {
    try { await pollFinalized(); await scanBlocks(); } catch (e) { /* transport trouble: retry next tick */ }
    await sleep(500);
  }
}

// ---------- submission ----------
const stats = { submitAttempts: 0, resends: 0, transportErrors: 0, rejected: 0 };
// Returns when the node acknowledged the tx (accepted into pool / already known), or throws 'halt' when it
// was rejected as invalid long enough that the tx can never be valid (mortality passed).
async function submitUntilAck(rec) {
  for (let attempt = 0; ; attempt++) {
    if (rec.firstSubmitMs == null) rec.firstSubmitMs = Date.now();
    else stats.resends++;
    stats.submitAttempts++; rec.attempts++;
    try {
      const h = await rpc.call('author_submitExtrinsic', [rec.hex], 10_000);
      if (h !== rec.hash) log(`WARN node hash ${h} != local ${rec.hash}`);
      rec.acked = true; rec.ackMs = Date.now(); return;
    } catch (e) {
      if (rec.block != null) { rec.acked = true; return; } // already seen in a finalized block
      if (e instanceof RpcError) {
        const { code, message = '', data = '' } = e.rpc;
        // 1013 already imported, 1012 temporarily banned (it was imported before), 1010 Stale/outdated (nonce used,
        // i.e. most likely by this very tx being included): all mean the node had it; reconciliation decides.
        if (code === 1013 || code === 1012 || (code === 1010 && /outdated|stale/i.test(String(data) + message))) {
          rec.acked = true; rec.ackMs = Date.now(); rec.ackNote = `${code} ${message}`; return;
        }
        stats.rejected++; rec.lastReject = e.message;
        if (fin.number > rec.signedAt + MORTAL_PERIOD) { rec.reason = `rejected-at-submit: ${e.message}`; throw new Error('halt'); }
        log(`submit rejected ${rec.signer}#${rec.nonce}: ${e.message}; resending same bytes`);
        await sleep(2000);
      } else {
        stats.transportErrors++;
        await sleep(300); // reconnect is handled by Rpc; resend identical bytes
      }
    }
  }
}

function makeRecipients(seedTag, k) {
  return Array.from({ length: RECIPIENTS }, (_, j) => AccountId().dec(blake2b(new TextEncoder().encode(`${seedTag}/${k}/${j}`), { dkLen: 32 })));
}

async function signTx(acct, nonce, payments) {
  const at = { hash: fin.hash, number: fin.number };
  const bytes = await api.tx.Payroll.batch_pay({ payments }).create(acct.signer, { nonce, mortality: { mortal: true, period: MORTAL_PERIOD, at }, ...DAG });
  const rec = { hash: txHash(bytes), hex: hex(bytes), signer: acct.address, nonce, signedAt: at.number, firstSubmitMs: null, attempts: 0, acked: false, block: null };
  txs.set(rec.hash, rec);
  return rec;
}

const waitIncluded = (rec, ms) => new Promise((res) => {
  if (rec.block != null) return res(true);
  const t = setTimeout(() => res(false), ms); rec.onIncluded = () => { clearTimeout(t); res(true); };
});

function pct(sorted, p) { return sorted.length ? sorted[Math.min(sorted.length - 1, Math.ceil(p / 100 * sorted.length) - 1)] : null; }

// ---------- main ----------
async function setup() {
  rpc = new Rpc(ENDPOINT);
  await rpc.waitConnected(30_000);
  const genesis = await rpc.callRetry('chain_getBlockHash', [0]);
  const meta = unhex(await rpc.callRetry('state_getMetadata', []));
  const def = { ...canggu, genesis, getMetadata: async () => meta };
  api = await getOfflineApi(def);
  codecs = await getTypedCodecs(def);
  const ed = api.constants.Balances.ExistentialDeposit;
  const chain = await rpc.callRetry('system_chain', []);
  const ver = await rpc.callRetry('state_getRuntimeVersion', []);
  log(`connected ${ENDPOINT}: ${chain} ${ver.specName}/${ver.specVersion}, ED ${ed}`);

  const derive = sr25519CreateDerive(entropyToMiniSecret(mnemonicToEntropy(DEV_PHRASE)));
  const mk = (p) => { const kp = derive(p); return { path: p, pub: kp.publicKey, address: AccountId().dec(kp.publicKey), signer: getTxCreator(kp.publicKey, 'Sr25519', kp.sign) }; };
  const alice = mk('//Alice');
  const accts = Array.from({ length: ACCOUNTS }, (_, i) => mk(`//Alice//stress//${i}`));

  // start finality tracking
  for (let i = 0; fin.number < 0; i++) { try { await pollFinalized(); } catch (e) { if (i > 60) throw e; await sleep(500); } }
  scanFrom = fin.number + 1; scannedTo = fin.number;
  poller();

  // funding: one Payroll.batch_pay from Alice to every test account below the target balance
  const perTx = BigInt(RECIPIENTS) * 2n * ed + 10n * ed; // payments + generous fee bound
  const target = perTx * BigInt(Math.ceil(TOTAL / ACCOUNTS) + 20);
  const need = [];
  for (const a of accts) { const ai = await accountAt(a.pub, fin.hash); if (ai.data.free < target) need.push([a.address, target]); }
  if (need.length) {
    const nonce = Math.max((await accountAt(alice.pub, fin.hash)).nonce, await rpc.callRetry('system_accountNextIndex', [alice.address]));
    const rec = await signTx(alice, nonce, need);
    rec.setup = true;
    log(`funding ${need.length} accounts with ${target} each (Alice nonce ${nonce}, tx ${rec.hash})`);
    await submitUntilAck(rec);
    if (!(await waitIncluded(rec, 90_000))) throw new Error('funding tx not finalized within 90 s');
    if (!rec.ok) throw new Error(`funding tx failed: ${rec.error}`);
    txs.delete(rec.hash);
  }
  for (const a of accts) {
    await pollFinalized();
    const ai = await accountAt(a.pub, fin.hash);
    if (ai.data.free < target) throw new Error(`account ${a.address} not funded (${ai.data.free})`);
    // never reuse a nonce: start at the max of finalized nonce and the pool-aware next index
    a.nonce = Math.max(ai.nonce, await rpc.callRetry('system_accountNextIndex', [a.address]));
  }
  log(`funded ${ACCOUNTS} accounts (≥ ${target}); finalized #${fin.number}`);
  return { accts, ed };
}

async function load({ accts, ed }) {
  const t0 = Date.now() + 1000;
  const interval = 1000 / RATE;
  const runTag = `${t0}`;
  const order = [];
  log(`load: ${TOTAL} x Payroll.batch_pay (${RECIPIENTS} recipients) at ${RATE}/s for ${DURATION}s from ${ACCOUNTS} accounts`);
  const workers = accts.map(async (a, ai) => {
    for (let k = ai; k < TOTAL; k += ACCOUNTS) {
      const due = t0 + k * interval;
      if (due > Date.now()) await sleep(due - Date.now());
      const payments = makeRecipients(runTag, k).map((r) => [r, 2n * ed]);
      let rec;
      try { rec = await signTx(a, a.nonce, payments); } catch (e) { log(`sign failed for ${a.address}: ${e.message}`); a.halted = 'sign failed'; return; }
      order.push(rec.hash);
      try { await submitUntilAck(rec); } catch { a.halted = rec.reason; log(`account ${ai} halted at nonce ${rec.nonce}: ${rec.reason}`); return; }
      a.nonce++;
    }
  });
  const progress = setInterval(() => {
    const sent = [...txs.values()].filter((r) => r.firstSubmitMs != null).length;
    const inc = [...txs.values()].filter((r) => r.block != null).length;
    log(`progress: submitted ${sent}, finalized ${inc}, fin #${fin.number}, reconnects ${rpc.reconnects}`);
  }, 10_000);
  await Promise.all(workers);
  clearInterval(progress);
  log(`load done in ${((Date.now() - t0) / 1000).toFixed(1)}s`);
  return { t0, order };
}

async function reconcile(accts) {
  const end = Date.now() + RECONCILE_MAX_MS;
  const byAddr = new Map(accts.map((a) => [a.address, a]));
  const all = [...txs.values()];
  for (const r of all) r.category = 'unresolved';
  let lastLog = 0;
  for (;;) {
    let finNonce = new Map();
    try {
      const snap = { ...fin };
      for (const a of accts) finNonce.set(a.address, (await accountAt(a.pub, snap.hash)).nonce);
      // all blocks up to snap scanned?
      const fullyScanned = scannedTo >= snap.number;
      for (const r of all) {
        if (r.block != null) { r.category = r.ok ? 'finalized-ok' : 'failed-in-block'; continue; }
        r.category = 'unresolved';
        if (!fullyScanned) continue;
        const nonceProof = finNonce.get(r.signer) > r.nonce;
        const mortalProof = snap.number > r.signedAt + MORTAL_PERIOD;
        if ((nonceProof || mortalProof) && r.acked) { r.category = 'lost'; r.lostProof = nonceProof ? `finalized nonce ${finNonce.get(r.signer)} > ${r.nonce} at #${snap.number}` : `finalized #${snap.number} > mortality end ${r.signedAt + MORTAL_PERIOD}`; }
        else if (nonceProof || mortalProof) r.reason = r.reason ?? `never acknowledged by node (${r.lastReject ?? 'transport errors'})`;
      }
    } catch (e) { /* transport problem; retry */ }
    const open = all.filter((r) => r.category === 'unresolved' && !r.reason?.startsWith('never') && !r.reason?.startsWith('rejected'));
    if (!open.length || Date.now() > end) break;
    if (Date.now() - lastLog > 10_000) { lastLog = Date.now(); log(`reconcile: ${open.length} tx not yet in a finalized block (fin #${fin.number})`); }
    await sleep(1000);
  }
  for (const r of all) if (r.category === 'unresolved' && !r.reason) r.reason = 'timeout: not in a finalized block, no proof of loss yet';
  return all;
}

async function main() {
  let ctx;
  try { ctx = await setup(); } catch (e) {
    log(`SETUP FAILED: ${e.message}`); pollerStop = true; rpc?.close(); return 3;
  }
  const { t0 } = await load(ctx);
  const all = await reconcile(ctx.accts);
  pollerStop = true; rpc.close();

  const sent = all.filter((r) => r.firstSubmitMs != null);
  const cat = { 'finalized-ok': 0, 'failed-in-block': 0, lost: 0, unresolved: 0 };
  for (const r of sent) cat[r.category]++;
  const jsonl = sent.sort((a, b) => a.firstSubmitMs - b.firstSubmitMs).map((r) => JSON.stringify({
    hash: r.hash, category: r.category, signer: r.signer, nonce: r.nonce, firstSubmitMs: r.firstSubmitMs,
    signedAtBlock: r.signedAt, block: r.block, blockHash: r.blockHash ?? null, finalizedMs: r.finalizedMs ?? null,
    latencyMs: r.block != null ? r.finalizedMs - r.firstSubmitMs : null, attempts: r.attempts,
    ...(r.error ? { dispatchError: r.error } : {}), ...(r.lostProof ? { lostProof: r.lostProof } : {}), ...(r.reason && r.category === 'unresolved' ? { reason: r.reason } : {}),
  }));
  fs.writeFileSync(path.join(OUT, 'transactions.jsonl'), jsonl.join('\n') + '\n');

  const windowEnd = t0 + DURATION * 1000 + 2000;
  const inWindow = sent.filter((r) => r.firstSubmitMs <= windowEnd).length;
  const delivered = inWindow >= Math.ceil(0.95 * TOTAL);
  const lat = sent.filter((r) => r.block != null).map((r) => r.finalizedMs - r.firstSubmitMs).sort((a, b) => a - b);
  const p95 = pct(lat, 95), p50 = pct(lat, 50);
  // "clean" = node acknowledged the first submit attempt within 2 s, so no tester-side delay is in its latency
  const clean = sent.filter((r) => r.block != null && r.attempts === 1 && r.ackMs - r.firstSubmitMs < 2000);
  const latClean = clean.map((r) => r.finalizedMs - r.firstSubmitMs).sort((a, b) => a - b);
  const p95Clean = pct(latClean, 95);
  const slow = p95 != null && p95 >= P95_LIMIT_MS;
  const slowChain = slow && p95Clean != null && p95Clean >= P95_LIMIT_MS;
  const lastSubmit = Math.max(...sent.map((r) => r.firstSubmitMs));
  const deliveredRate = sent.length / ((lastSubmit - t0) / 1000 + 1 / RATE);

  let verdict, code, why = [];
  if (slowChain || cat.lost > 0) {
    verdict = 'FAIL'; code = 1;
    if (slowChain) why.push(`p95 latency ${p95} ms ≥ ${P95_LIMIT_MS} ms (also ${p95Clean} ms over the ${latClean.length} tx with no tester-side delay)`);
    if (cat.lost > 0) why.push(`${cat.lost} transactions proven lost`);
  } else if (!delivered || cat.unresolved > 0 || lat.length === 0 || slow) {
    verdict = "CAN'T TELL"; code = 4;
    if (slow) why.push(`p95 ${p95} ms ≥ ${P95_LIMIT_MS} ms, but only because of tester-side delays (p95 without them: ${p95Clean} ms)`);
    if (!delivered) why.push(`load not delivered: ${inWindow}/${TOTAL} first-submitted within ${DURATION}+2 s (need ${Math.ceil(0.95 * TOTAL)}) — tester-side`);
    if (cat.unresolved > 0) why.push(`${cat.unresolved} unresolved`);
  } else { verdict = 'PASS'; code = 0; why.push(`p95 ${p95} ms < ${P95_LIMIT_MS} ms, no loss, load delivered`); }

  const summary = {
    endpoint: ENDPOINT, call: 'Payroll.batch_pay', rate: RATE, durationS: DURATION, accounts: ACCOUNTS, target: TOTAL,
    sent: sent.length, categories: cat, firstSubmittedInWindow: inWindow, deliveredRateTxPerS: +deliveredRate.toFixed(3),
    latencyMs: { p50, p95, max: lat.at(-1) ?? null, n: lat.length, p95NoTesterDelay: p95Clean, nNoTesterDelay: latClean.length },
    testerSide: { submitAttempts: stats.submitAttempts, resends: stats.resends, transportErrors: stats.transportErrors, submitRejections: stats.rejected, reconnects: rpc.reconnects, haltedAccounts: ctx.accts.filter((a) => a.halted).length },
    verdict, exitCode: code, reasons: why,
  };
  fs.writeFileSync(path.join(OUT, 'summary.json'), JSON.stringify(summary, null, 2) + '\n');
  log('---------------- RESULT ----------------');
  log(`sent ${sent.length}/${TOTAL}: finalized-ok ${cat['finalized-ok']}, failed-in-block ${cat['failed-in-block']}, lost ${cat.lost}, unresolved ${cat.unresolved}`);
  log(`delivered: ${inWindow} first-submitted within window (+2 s); rate ${deliveredRate.toFixed(2)} tx/s`);
  log(`latency first-submit → finalized: p50 ${p50} ms, p95 ${p95} ms, max ${lat.at(-1)} ms (n=${lat.length}); p95 without tester-side delays ${p95Clean} ms (n=${latClean.length})`);
  log(`tester-side (not chain problems): ${stats.resends} resends, ${stats.transportErrors} transport errors, ${stats.rejected} submit rejections, ${rpc.reconnects} reconnects`);
  log(`VERDICT: ${verdict} (exit ${code}) — ${why.join('; ')}`);
  log(`files: ${path.join(OUT, 'transactions.jsonl')}, ${path.join(OUT, 'summary.json')}, ${path.join(OUT, 'run.log')}`);
  return code;
}

const code = await main().catch((e) => { log(`INTERNAL ERROR: ${e.stack ?? e}`); return 4; });
await new Promise((r) => logFile.end(r));
process.exit(code);
