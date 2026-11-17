// Mythos Nfts.mint stress test. usage: node stress.mjs <ws-endpoint> <output-dir>
// Exit: 0 PASS, 1 FAIL (chain missed a threshold), 3 setup failed, 4 CAN'T TELL.
// Signing: PAPI 3.3.0 OfflineApi + getTxCreator (Ecdsa/AccountId20) against live metadata.
// Transport: our own reconnecting JSON-RPC websocket (submit, finalized-head tracking, block/event reads).
import fs from 'node:fs';
import path from 'node:path';
import { getOfflineApi } from 'polkadot-api';
import { getTxCreator } from 'polkadot-api/tx-creator';
import { getDynamicBuilder, getLookupFn } from '@polkadot-api/metadata-builders';
import { unifyMetadata, decAnyMetadata } from '@polkadot-api/substrate-bindings';
import { secp256k1 } from '@noble/curves/secp256k1.js';
import { keccak_256 } from '@noble/hashes/sha3.js';
import { blake2b } from '@noble/hashes/blake2.js';

const [ENDPOINT, OUT] = process.argv.slice(2);
if (!ENDPOINT || !OUT) { console.error('usage: stress.mjs <ws-endpoint> <output-dir>'); process.exit(3); }
const SEED = '0x5fb92d6e98884f76de468fa3f6278f8807c48bebc13595d45af5bdc4da702133';
const RATE = 0.25, DURATION_S = 200, ACCOUNTS = 4, PERIOD = 64;
const TOTAL = Math.round(RATE * DURATION_S), INTERVAL_MS = 1000 / RATE;
const P95_LIMIT_MS = 60_000, SETUP_TIMEOUT_MS = 600_000, RECONCILE_TIMEOUT_MS = 180_000;
// Test-only fault injection: STRESS_DROP_EVERY_MS=N closes our websocket every N ms (proves reconnect handling).
const DROP_EVERY = Number(process.env.STRESS_DROP_EVERY_MS || 0);

fs.mkdirSync(OUT, { recursive: true });
const logFile = fs.createWriteStream(path.join(OUT, 'run.log'));
const log = (...a) => { const s = `[${new Date().toISOString()}] ${a.join(' ')}`; console.log(s); logFile.write(s + '\n'); };
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const hex = (u8) => '0x' + Buffer.from(u8).toString('hex');
const unhex = (h) => Uint8Array.from(Buffer.from(h.slice(2), 'hex'));
const ownProblems = { reconnects: 0, submitRetries: 0, submitsDelayedByUs: 0, rpcErrors: 0 }; // tool-side issues, never chain issues

// ---------- reconnecting JSON-RPC ----------
class ConnError extends Error {}
class Rpc {
  constructor(url) { this.url = url; this.id = 0; this.pending = new Map(); this.ws = null; this.open = false; this.waiters = []; this.onFinalizedHead = null; this.connect(); }
  connect() {
    const ws = new WebSocket(this.url); this.ws = ws;
    ws.onopen = () => {
      this.open = true; this.waiters.splice(0).forEach((w) => w());
      this.call('chain_subscribeFinalizedHeads', [], 10_000).catch(() => {}); // notifications only speed things up; polling is the backstop
      if (DROP_EVERY) { clearTimeout(this.dropT); this.dropT = setTimeout(() => { log('fault injection: closing websocket'); ws.close(); }, DROP_EVERY); }
    };
    ws.onmessage = (m) => {
      const d = JSON.parse(m.data);
      if (d.id != null) { const p = this.pending.get(d.id); if (p) { this.pending.delete(d.id); clearTimeout(p.t); d.error ? p.rej(Object.assign(new Error(d.error.message), { rpc: d.error })) : p.res(d.result); } }
      else if (d.method === 'chain_finalizedHead' && this.onFinalizedHead) this.onFinalizedHead(d.params.result);
    };
    ws.onerror = () => {};
    ws.onclose = () => {
      if (this.ws !== ws) return;
      const wasOpen = this.open; this.open = false;
      for (const [, p] of this.pending) { clearTimeout(p.t); p.rej(new ConnError('connection closed')); }
      this.pending.clear();
      if (wasOpen) { ownProblems.reconnects++; log('websocket closed; reconnecting'); }
      if (!this.closed) setTimeout(() => this.connect(), 1000);
    };
  }
  async call(method, params = [], timeout = 30_000) {
    if (!this.open) await Promise.race([new Promise((r) => this.waiters.push(r)), sleep(timeout).then(() => { throw new ConnError('not connected'); })]);
    return new Promise((res, rej) => {
      const id = ++this.id;
      const t = setTimeout(() => { this.pending.delete(id); rej(new ConnError(`timeout ${method}`)); }, timeout);
      this.pending.set(id, { res, rej, t });
      try { this.ws.send(JSON.stringify({ id, jsonrpc: '2.0', method, params })); } catch (e) { clearTimeout(t); this.pending.delete(id); rej(new ConnError(e.message)); }
    });
  }
  // reads are idempotent: retry through reconnects
  async read(method, params = [], deadline = Date.now() + 120_000) {
    for (;;) {
      try { return await this.call(method, params); }
      catch (e) { ownProblems.rpcErrors++; if (Date.now() > deadline || !(e instanceof ConnError)) throw e; await sleep(1000); }
    }
  }
  close() { this.closed = true; clearTimeout(this.dropT); this.ws?.close(); }
}

// ---------- accounts ----------
function account(privHex) {
  const priv = unhex(privHex);
  const pub = secp256k1.getPublicKey(priv, false);
  const raw = keccak_256(pub.slice(1)).slice(-20);
  const signer = getTxCreator(raw, 'Ecdsa', (data) => {
    const s = secp256k1.sign(keccak_256(data), priv, { prehash: false, format: 'recovered' }); // [rec, r, s]
    return Uint8Array.from([...s.slice(1), s[0]]);
  });
  return { priv: privHex, address: hex(raw), signer };
}
const deriveKey = (i) => hex(keccak_256(new Uint8Array([...unhex(SEED), ...new TextEncoder().encode(`mythos-stress/${i}`)])));

// ---------- finalized chain tracker ----------
const rpc = new Rpc(ENDPOINT);
let builder, eventsKey, accountStore, offline;
const included = new Map(); // txHash -> { block, blockHash, index, ok, error, events, seenMs }
const finalized = { number: -1, hash: null };
let scanFrom = null, scanning = false;
const observed = []; // [number, wall-clock ms we first learned block #number (or a descendant) was finalized]
const observe = (n, ms) => { if (!observed.length || n > observed[observed.length - 1][0]) observed.push([n, ms]); };
const firstSeen = (n) => observed.find(([m]) => m >= n)?.[1] ?? Date.now();

async function headerOf(h) { const hd = await rpc.read('chain_getHeader', [h]); return { number: parseInt(hd.number, 16), hash: h }; }
function onHead(header) { observe(parseInt(header.number, 16), Date.now()); scan(); }
rpc.onFinalizedHead = onHead;

async function scan() {
  if (scanning) return; scanning = true;
  try {
    for (;;) {
      const seenMs = Date.now();
      const fh = await rpc.read('chain_getFinalizedHead');
      const head = await headerOf(fh);
      observe(head.number, seenMs);
      if (scanFrom == null) { finalized.number = head.number; finalized.hash = head.hash; scanFrom = head.number + 1; break; }
      if (head.number < scanFrom) break;
      for (let n = scanFrom; n <= head.number; n++) {
        const bh = n === head.number ? head.hash : await rpc.read('chain_getBlockHash', [n]);
        const { block } = await rpc.read('chain_getBlock', [bh]);
        const evs = builder.buildStorage('System', 'Events').value.dec(await rpc.read('state_getStorage', [eventsKey, bh]));
        block.extrinsics.forEach((x, index) => {
          const h = hex(blake2b(unhex(x), { dkLen: 32 }));
          const mine = evs.filter((e) => e.phase.type === 'ApplyExtrinsic' && e.phase.value === index).map((e) => e.event);
          const fail = mine.find((e) => e.type === 'System' && e.value.type === 'ExtrinsicFailed');
          if (!included.has(h)) included.set(h, { block: n, blockHash: bh, index, ok: !fail, error: fail ? fail.value.value.dispatch_error : undefined, events: mine, seenMs: firstSeen(n) });
        });
        finalized.number = n; finalized.hash = bh; scanFrom = n + 1;
      }
    }
  } catch (e) { log('scan error (will retry):', e.message); }
  finally { scanning = false; }
}
setInterval(scan, 2000).unref();

async function finalizedNonce(addr) {
  const v = await rpc.read('state_getStorage', [accountStore.keys.enc(addr), finalized.hash]);
  return v ? Number(accountStore.value.dec(v).nonce) : 0;
}

// ---------- sign + submit ----------
async function sign(acct, call, nonce) {
  const at = { hash: finalized.hash, number: finalized.number };
  const bytes = await call.create(acct.signer, { nonce, mortality: { mortal: true, period: PERIOD, at } });
  return { bytes: hex(bytes), hash: hex(blake2b(bytes, { dkLen: 32 })), birth: at.number, birthHash: at.hash };
}
// Resends the identical bytes until the node accepts them (or reports it already knows them).
async function submit(tx, deadline) {
  for (;;) {
    tx.attempts = (tx.attempts || 0) + 1;
    if (tx.firstSubmitMs == null) tx.firstSubmitMs = Date.now();
    if (!rpc.open) { ownProblems.submitsDelayedByUs++; tx.cleanFirstAttempt = false; log(`submit ${tx.hash.slice(0, 10)} attempt ${tx.attempts}: websocket down, waiting to reconnect`); }
    try { await rpc.call('author_submitExtrinsic', [tx.bytes], 20_000); tx.submit = 'accepted'; return; }
    catch (e) {
      if (e instanceof ConnError) { ownProblems.submitRetries++; tx.cleanFirstAttempt = false; log(`submit ${tx.hash.slice(0, 10)} attempt ${tx.attempts} failed (${e.message}); resending same bytes`); }
      else {
        const msg = `${e.rpc?.code} ${e.message} ${JSON.stringify(e.rpc?.data ?? '')}`;
        if (/already imported|AlreadyImported|1013/i.test(msg)) { tx.submit = 'accepted'; return; }
        if (tx.attempts > 1 && /outdated|stale/i.test(msg)) { tx.submit = 'accepted-earlier'; return; } // an earlier attempt landed
        tx.submit = 'rejected'; tx.rejectReason = msg; log(`submit ${tx.hash.slice(0, 10)} rejected by node: ${msg}`); return;
      }
      if (Date.now() > deadline) { tx.submit = 'gave-up'; return; }
      await sleep(1000);
    }
  }
}
async function until(cond, timeout, what) {
  const end = Date.now() + timeout;
  while (!cond()) { if (Date.now() > end) throw new Error(`timeout waiting for ${what}`); await sleep(500); }
}

// ---------- setup ----------
const seed = account(SEED);
const accts = Array.from({ length: ACCOUNTS }, (_, i) => account(deriveKey(i)));

async function setup() {
  const metaHex = await rpc.read('state_getMetadata');
  const genesis = await rpc.read('chain_getBlockHash', [0]);
  builder = getDynamicBuilder(getLookupFn(unifyMetadata(decAnyMetadata(metaHex))));
  eventsKey = builder.buildStorage('System', 'Events').keys.enc();
  accountStore = builder.buildStorage('System', 'Account');
  offline = await getOfflineApi({ genesis, getMetadata: async () => unhex(metaHex) });
  await scan(); await until(() => finalized.hash, 60_000, 'finalized head');
  log(`chain: finalized #${finalized.number}, seed ${seed.address}, test accounts ${accts.map((a) => a.address).join(' ')}`);

  const runSetupTx = async (acct, call, label) => {
    const tx = await sign(acct, call, await finalizedNonce(acct.address));
    await submit(tx, Date.now() + 120_000);
    if (!/accepted/.test(tx.submit)) throw new Error(`${label}: submit ${tx.submit} ${tx.rejectReason ?? ''}`);
    await until(() => included.has(tx.hash), SETUP_TIMEOUT_MS, `${label} finalized`);
    const r = included.get(tx.hash);
    if (!r.ok) throw new Error(`${label} failed in block #${r.block}: ${JSON.stringify(r.error, big)}`);
    log(`${label} ok in #${r.block}`); return r;
  };
  const amount = 10n ** 23n; // 100k MYTH each
  await runSetupTx(seed, offline.tx.Utility.batch_all({ calls: accts.map((a) => offline.tx.Balances.transfer_keep_alive({ dest: a.address, value: amount }).decodedCall) }), 'fund 4 accounts (Utility.batch_all)');
  const cfg = { settings: 0n, max_supply: undefined, mint_settings: { mint_type: { type: 'Issuer', value: undefined }, price: undefined, start_block: undefined, end_block: undefined, default_item_settings: 0n, serial_mint: true } };
  await Promise.all(accts.map(async (a, i) => {
    const r = await runSetupTx(a, offline.tx.Nfts.create({ admin: a.address, config: cfg }), `Nfts.create account ${i}`);
    const ev = r.events.find((e) => e.type === 'Nfts' && e.value.type === 'Created');
    if (!ev) throw new Error(`no Nfts.Created event for account ${i}`);
    a.collection = ev.value.value.collection;
    a.nonce = await finalizedNonce(a.address);
  }));
}
const big = (k, v) => (typeof v === 'bigint' ? v.toString() : v instanceof Uint8Array ? hex(v) : v);

// ---------- load ----------
const txs = [];
async function load(t0) {
  const end = t0 + DURATION_S * 1000;
  await Promise.all(accts.map(async (a, ai) => {
    for (let k = ai; k < TOTAL; k += ACCOUNTS) {
      const sched = t0 + k * INTERVAL_MS;
      if (Date.now() < sched) await sleep(sched - Date.now());
      const call = offline.tx.Nfts.mint({ collection: a.collection, maybe_item: undefined, mint_to: a.address, witness_data: undefined });
      let signed;
      for (;;) { try { signed = await sign(a, call, a.nonce); break; } catch (e) { log(`sign failed for account ${ai}: ${e.message}; retrying`); await sleep(1000); } }
      const tx = { k, signer: a.address, account: ai, nonce: a.nonce, scheduledMs: sched, cleanFirstAttempt: true, ...signed };
      a.nonce++; txs.push(tx);
      await submit(tx, end + 60_000);
      if (tx.submit === 'rejected' || tx.submit === 'gave-up') { log(`account ${ai}: nonce ${tx.nonce} not accepted; stopping this account (no later nonces past a gap)`); return; }
    }
  }));
}

// ---------- reconcile ----------
async function reconcile() {
  const end = Date.now() + RECONCILE_TIMEOUT_MS;
  const nonces = {};
  for (;;) {
    await scan();
    for (const a of accts) { try { nonces[a.address] = await finalizedNonce(a.address); } catch { /* keep previous */ } }
    for (const tx of txs) {
      const r = included.get(tx.hash);
      if (r) { tx.category = r.ok ? 'finalized-ok' : 'failed-in-block'; tx.block = r.block; tx.latencyMs = r.seenMs - tx.firstSubmitMs; if (!r.ok) tx.dispatchError = r.error; continue; }
      const noncePassed = nonces[tx.signer] != null && nonces[tx.signer] > tx.nonce;
      const expired = finalized.number >= tx.birth + PERIOD;
      if (noncePassed || expired) {
        tx.lossProof = noncePassed ? `finalized nonce ${nonces[tx.signer]} > ${tx.nonce}` : `finalized #${finalized.number} >= birth #${tx.birth} + period ${PERIOD}`;
        tx.category = tx.submit === 'rejected' ? 'rejected-at-submit' : 'lost';
      } else tx.category = tx.submit === 'rejected' ? 'rejected-at-submit' : 'unresolved';
    }
    if (!txs.some((t) => t.category === 'unresolved') || Date.now() > end) return;
    await sleep(2000);
  }
}

function pct(arr, p) { if (!arr.length) return null; const s = [...arr].sort((a, b) => a - b); return s[Math.min(s.length - 1, Math.ceil((p / 100) * s.length) - 1)]; }

async function main() {
  try { await setup(); }
  catch (e) { log('SETUP FAILED:', e.stack ?? e); rpc.close(); return 3; }
  const t0 = Date.now() + 1000;
  log(`load: ${TOTAL} x Nfts.mint at ${RATE} tx/s for ${DURATION_S}s from ${ACCOUNTS} accounts, mortality ${PERIOD}`);
  try { await load(t0); } catch (e) { log('load error (tool-side):', e.stack ?? e); }
  log(`load done: ${txs.length} signed; reconciling (up to ${RECONCILE_TIMEOUT_MS / 1000}s)`);
  await reconcile();

  const lines = txs.map((t) => JSON.stringify({ hash: t.hash, category: t.category, signer: t.signer, nonce: t.nonce, firstSubmitMs: t.firstSubmitMs,
    scheduledMs: t.scheduledMs, attempts: t.attempts, submit: t.submit, birth: t.birth, block: t.block, latencyMs: t.latencyMs, lossProof: t.lossProof, rejectReason: t.rejectReason, dispatchError: t.dispatchError }, big));
  fs.writeFileSync(path.join(OUT, 'txs.jsonl'), lines.join('\n') + '\n');

  const cats = {}; for (const c of ['finalized-ok', 'failed-in-block', 'lost', 'unresolved', 'rejected-at-submit']) cats[c] = txs.filter((t) => t.category === c).length;
  const windowEnd = t0 + DURATION_S * 1000 + 2000;
  const onTime = txs.filter((t) => t.firstSubmitMs != null && t.firstSubmitMs <= windowEnd).length;
  const delivered = onTime >= 0.95 * TOTAL;
  const incl = txs.filter((t) => t.latencyMs != null);
  const p95 = pct(incl.map((t) => t.latencyMs), 95);
  // txs whose first submit attempt failed on our side carry our delay in their latency; judge the chain on clean ones
  const p95Clean = pct(incl.filter((t) => t.cleanFirstAttempt).map((t) => t.latencyMs), 95);
  const reasons = [];
  let verdict = 'PASS', code = 0;
  const chainFail = [];
  if (cats.lost > 0) chainFail.push(`${cats.lost} transaction(s) proven lost`);
  if (p95Clean != null && p95Clean >= P95_LIMIT_MS) chainFail.push(`p95 latency ${p95Clean} ms >= ${P95_LIMIT_MS} ms`);
  if (chainFail.length) { verdict = 'FAIL'; code = 1; reasons.push(...chainFail); }
  else {
    if (!delivered) reasons.push(`load not delivered: ${onTime}/${TOTAL} first-submitted within window+2s (need ${Math.ceil(0.95 * TOTAL)})`);
    if (cats.unresolved) reasons.push(`${cats.unresolved} unresolved`);
    if (cats['rejected-at-submit']) reasons.push(`${cats['rejected-at-submit']} rejected at submit (never entered the pool)`);
    if (p95 != null && p95 >= P95_LIMIT_MS) reasons.push(`p95 ${p95} ms only over the limit because of tool-side submit retries`);
    if (!incl.length) reasons.push('nothing included; no latency measured');
    if (reasons.length) { verdict = "CAN'T TELL"; code = 4; }
  }
  const summary = { endpoint: ENDPOINT, load: { call: 'Nfts.mint', rate: RATE, durationS: DURATION_S, accounts: ACCOUNTS, planned: TOTAL }, sent: txs.length, categories: cats,
    firstSubmittedInWindow: onTime, deliveredRateTxPerS: +(onTime / DURATION_S).toFixed(4), p95LatencyMs: p95, p95LatencyCleanMs: p95Clean,
    p50LatencyMs: pct(incl.map((t) => t.latencyMs), 50), maxLatencyMs: pct(incl.map((t) => t.latencyMs), 100), toolSide: ownProblems, verdict, exitCode: code, reasons };
  fs.writeFileSync(path.join(OUT, 'summary.json'), JSON.stringify(summary, big, 2) + '\n');
  log('---- result ----');
  log(`sent ${txs.length}/${TOTAL}: ` + Object.entries(cats).map(([k, v]) => `${k}=${v}`).join(' '));
  log(`delivered: ${onTime} first-submitted within window+2s -> ${summary.deliveredRateTxPerS} tx/s (target ${RATE})`);
  log(`latency (first submit -> finalized block): p50 ${summary.p50LatencyMs} ms, p95 ${p95} ms, max ${summary.maxLatencyMs} ms`);
  log(`tool-side (not chain) issues: reconnects=${ownProblems.reconnects} submitRetries=${ownProblems.submitRetries} submitsDelayedByUs=${ownProblems.submitsDelayedByUs} rpcErrors=${ownProblems.rpcErrors}`);
  log(`VERDICT: ${verdict} (exit ${code})${reasons.length ? ' - ' + reasons.join('; ') : ''}`);
  log(`files: ${path.join(OUT, 'txs.jsonl')} ${path.join(OUT, 'summary.json')}`);
  rpc.close();
  return code;
}

main().then((c) => logFile.end(() => process.exit(c)), (e) => { log('unexpected:', e.stack ?? e); logFile.end(() => process.exit(4)); });
