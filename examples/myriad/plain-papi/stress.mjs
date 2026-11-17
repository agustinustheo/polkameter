// Myriad stress test: Tipping.send_tip at a fixed rate from 4 derived accounts, then reconcile every hash
// against the finalized chain.  usage: node stress.mjs <ws-endpoint> <output-dir>
// Exit: 0 PASS, 1 FAIL (chain problem), 4 CAN'T TELL, 3 setup failed before the load.
//
// Design: no chainHead subscription. Transactions are signed offline (PAPI getOfflineApi) against an explicit
// finalized block (mortal, period 64). All chain I/O is plain JSON-RPC over a reconnect-on-demand WebSocket,
// so a dropped connection only costs a retry. Finalized blocks are scanned by number (chain_getBlockHash(n) for
// n <= finalized head), each extrinsic is hashed (blake2-256) and matched; System.Events gives ok/failed.
import { getOfflineApi, getTypedCodecs, AccountId } from 'polkadot-api';
import { getTxCreator } from 'polkadot-api/tx-creator';
import { sr25519CreateDerive } from '@polkadot-labs/hdkd';
import { DEV_PHRASE, entropyToMiniSecret, mnemonicToEntropy } from '@polkadot-labs/hdkd-helpers';
import { blake2b } from '@noble/hashes/blake2.js';
import { myriad } from '@polkadot-api/descriptors';
import fs from 'node:fs';
import path from 'node:path';

// ---------------- configuration ----------------
const [ENDPOINT, OUT] = process.argv.slice(2);
if (!ENDPOINT || !OUT) { console.error('usage: node stress.mjs <ws-endpoint> <output-dir>'); process.exit(3); }
const RATE = 0.25;                 // tx/s
const DURATION_S = 200;            // load window
const TOTAL = Math.round(RATE * DURATION_S); // 50
const N_ACCOUNTS = 4;
const PERIOD = 64;                 // mortality period (blocks)
const P95_LIMIT_MS = 60_000;
const SUBMIT_GIVEUP_MS = 60_000;   // keep resending a tx's identical bytes for this long after window end
const SETTLE_MS = 120_000;         // after the last submit, wait this long for inclusion before reconciling
const TIP = 10n ** 18n;
const FUND = 10n ** 22n;           // per test account
const FUND_BOB = 10n ** 24n;       // server owner (may need stake)

process.stdout.on('error', () => {}); // a closed stdout pipe must not kill the run
fs.mkdirSync(OUT, { recursive: true });
const logFile = fs.createWriteStream(path.join(OUT, 'run.log'), { flags: 'a' });
const T0 = Date.now();
const log = (...a) => {
  const line = `[${((Date.now() - T0) / 1000).toFixed(1).padStart(6)}s] ${a.join(' ')}`;
  console.log(line); logFile.write(line + '\n');
};
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const hex = (u8) => '0x' + Buffer.from(u8).toString('hex');
const unhex = (h) => Uint8Array.from(Buffer.from(h.replace(/^0x/, ''), 'hex'));
const txHash = (u8) => hex(blake2b(u8, { dkLen: 32 }));
const toolIssues = { reconnects: 0, rpcErrors: 0, submitRetries: 0, signRetries: 0 };

// ---------------- reconnecting JSON-RPC client ----------------
class Rpc {
  constructor(url) { this.url = url; this.ws = null; this.id = 0; this.pending = new Map(); this.connecting = null; this.everConnected = false; }
  connect() {
    if (this.ws && this.ws.readyState === WebSocket.OPEN) return Promise.resolve();
    if (this.connecting) return this.connecting;
    this.connecting = new Promise((resolve, reject) => {
      const ws = new WebSocket(this.url);
      const timer = setTimeout(() => { try { ws.close(); } catch {} reject(new Error('connect timeout')); }, 10_000);
      ws.onopen = () => {
        clearTimeout(timer);
        if (this.everConnected) { toolIssues.reconnects++; log('rpc: reconnected'); }
        this.everConnected = true; this.ws = ws; resolve();
      };
      ws.onerror = () => { clearTimeout(timer); reject(new Error('ws error')); };
      ws.onclose = () => {
        clearTimeout(timer);
        if (this.ws === ws) { this.ws = null; log('rpc: connection closed'); }
        for (const [, p] of this.pending) p.reject(new Error('connection closed'));
        this.pending.clear();
        reject(new Error('closed'));
      };
      ws.onmessage = (ev) => {
        let m; try { m = JSON.parse(ev.data); } catch { return; }
        const p = this.pending.get(m.id); if (!p) return;
        this.pending.delete(m.id);
        if (m.error) { const e = new Error(m.error.message + (m.error.data ? ': ' + JSON.stringify(m.error.data) : '')); e.rpc = m.error; p.reject(e); }
        else p.resolve(m.result);
      };
    }).finally(() => { this.connecting = null; });
    return this.connecting;
  }
  // One attempt. Throws Error with .rpc for node-returned errors, otherwise transport error.
  async call(method, params = [], timeoutMs = 15_000) {
    await this.connect();
    const id = ++this.id;
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => { this.pending.delete(id); reject(new Error(`timeout ${method}`)); }, timeoutMs);
      this.pending.set(id, { resolve: (v) => { clearTimeout(timer); resolve(v); }, reject: (e) => { clearTimeout(timer); reject(e); } });
      try { this.ws.send(JSON.stringify({ jsonrpc: '2.0', id, method, params })); }
      catch (e) { clearTimeout(timer); this.pending.delete(id); reject(e); }
    });
  }
  // Retries transport errors (not node errors) until deadline.
  async req(method, params = [], deadlineMs = 60_000) {
    const end = Date.now() + deadlineMs; let last;
    while (Date.now() < end) {
      try { return await this.call(method, params); }
      catch (e) { if (e.rpc) throw e; last = e; toolIssues.rpcErrors++; await sleep(500); }
    }
    throw new Error(`rpc ${method} failed: ${last?.message}`);
  }
  close() { try { this.ws?.close(); } catch {} }
}
const rpc = new Rpc(ENDPOINT);

// ---------------- chain helpers ----------------
let codecs, offline, genesis;
const EVENTS_KEY = '0x26aa394eea5630e07c48ae0c9558cef780d41e5e16056765bc8461851072c9d7';
const accountKey = (pub) => '0x26aa394eea5630e07c48ae0c9558cef7b99d880ec681799c0cf30e8886371da9' +
  Buffer.from(blake2b(pub, { dkLen: 16 })).toString('hex') + Buffer.from(pub).toString('hex');
async function finalizedHead() {
  const hash = await rpc.req('chain_getFinalizedHead');
  const hdr = await rpc.req('chain_getHeader', [hash]);
  return { hash, number: parseInt(hdr.number, 16) };
}
async function accountAt(pub, at) {
  const raw = await rpc.req('state_getStorage', [accountKey(pub), at]);
  if (!raw) return { nonce: 0, free: 0n };
  const v = codecs.query.System.Account.value.dec(raw);
  return { nonce: v.nonce, free: v.data.free };
}

// ---------------- accounts ----------------
const derive = sr25519CreateDerive(entropyToMiniSecret(mnemonicToEntropy(DEV_PHRASE)));
const ss58 = AccountId(42);
function account(pathStr) {
  const kp = derive(pathStr);
  return { name: pathStr, pub: kp.publicKey, address: ss58.dec(kp.publicKey), signer: getTxCreator(kp.publicKey, 'Sr25519', kp.sign), nonce: 0 };
}
const alice = account('//Alice');
const bob = account('//Bob');
const testAccounts = Array.from({ length: N_ACCOUNTS }, (_, i) => account(`//Alice//myriad-tip-stress//${i}`));

// ---------------- finalized block scanner ----------------
// Records for every tracked hash: block number/hash, ok/failed, and when we first saw that block finalized.
const tracked = new Map();     // hash -> tx record
const seenFinalAt = new Map(); // block number -> wall ms when first seen finalized
let scannedTo = -1;            // highest block number scanned
let finalized = null;          // latest finalized {hash, number}
async function scanTo(head) {
  for (let n = scannedTo + 1; n <= head.number; n++) {
    if (!seenFinalAt.has(n)) seenFinalAt.set(n, Date.now());
    const bh = n === head.number ? head.hash : await rpc.req('chain_getBlockHash', [n]);
    const block = await rpc.req('chain_getBlock', [bh]);
    const exts = block.block.extrinsics;
    let events = null;
    for (let i = 0; i < exts.length; i++) {
      const rec = tracked.get(txHash(unhex(exts[i])));
      if (!rec || rec.blockNumber != null) continue;
      if (!events) events = codecs.query.System.Events.value.dec(await rpc.req('state_getStorage', [EVENTS_KEY, bh]));
      const mine = events.filter((e) => e.phase.type === 'ApplyExtrinsic' && e.phase.value === i);
      const failed = mine.find((e) => e.event.type === 'System' && e.event.value.type === 'ExtrinsicFailed');
      rec.blockNumber = n; rec.blockHash = bh; rec.index = i;
      rec.finalizedMs = seenFinalAt.get(n);
      rec.ok = !failed;
      rec.events = mine.map((e) => `${e.event.type}.${e.event.value.type}`);
      if (failed) rec.dispatchError = JSON.stringify(failed.event.value.value.dispatch_error, (k, v) => typeof v === 'bigint' ? v.toString() : v);
    }
    scannedTo = n;
  }
}
let scannerStop = false;
async function scannerLoop() {
  while (!scannerStop) {
    try {
      const head = await finalizedHead();
      finalized = head;
      if (scannedTo < 0) scannedTo = head.number; // only blocks after start are relevant (all txs born >= this)
      // mark seen time for any newly finalized numbers immediately, then fetch bodies
      for (let n = scannedTo + 1; n <= head.number; n++) if (!seenFinalAt.has(n)) seenFinalAt.set(n, Date.now());
      await scanTo(head);
    } catch (e) { toolIssues.rpcErrors++; log('scanner: retry after error:', e.message); }
    await sleep(200);
  }
}

// ---------------- signing & submission ----------------
// Sign against the latest finalized block with mortality. Returns record; bytes never change afterwards.
async function signTx(acct, call, meta = {}) {
  for (let attempt = 0; ; attempt++) {
    try {
      const at = finalized ?? await finalizedHead();
      const bytes = await call.create(acct.signer, { nonce: acct.nonce, mortality: { mortal: true, period: PERIOD, at: { hash: at.hash, number: at.number } } });
      const rec = { hash: txHash(bytes), bytes: hex(bytes), signer: acct.address, account: acct, nonce: acct.nonce, birth: at.number, birthHash: at.hash, status: 'pending', ...meta };
      acct.nonce++;
      tracked.set(rec.hash, rec);
      return rec;
    } catch (e) {
      toolIssues.signRetries++; log(`sign error (${acct.name} nonce ${acct.nonce}), retrying:`, e.message);
      if (attempt > 20) throw e; await sleep(1000);
    }
  }
}
// Submit identical bytes until the node accepts them (or tells us they are already known / used / included).
// status: 'accepted' | 'rejected' (node said invalid) | 'gaveup' (never got an answer)
async function submit(rec, giveUpAt) {
  rec.attempts = 0;
  while (true) {
    if (rec.blockNumber != null) { rec.status = 'accepted'; return; }
    rec.attempts++;
    if (rec.firstSubmitMs == null) rec.firstSubmitMs = Date.now();
    else toolIssues.submitRetries++;
    try {
      const h = await rpc.call('author_submitExtrinsic', [rec.bytes]);
      if (h !== rec.hash) log(`warning: node returned hash ${h} for ${rec.hash}`);
      rec.status = 'accepted'; return;
    } catch (e) {
      const msg = e.message || '';
      if (e.rpc) {
        const code = e.rpc.code;
        // already in pool / recently included / nonce already used (included while we were disconnected)
        if (code === 1013 || code === 1012 || /already imported|temporarily banned|outdated|Stale/i.test(msg)) {
          rec.status = 'accepted'; rec.submitNote = msg; return;
        }
        // transient pool conditions
        if (code === 1014 || code === 1016 || /future|priority|immediately dropped|limit/i.test(msg)) {
          log(`submit ${rec.hash.slice(0, 10)} transient: ${msg}`);
        } else {
          rec.status = 'rejected'; rec.submitNote = msg; log(`submit ${rec.hash.slice(0, 10)} REJECTED by node: ${msg}`); return;
        }
      } else {
        log(`submit ${rec.hash.slice(0, 10)} transport error, will resend identical bytes: ${msg}`);
      }
    }
    if (Date.now() > giveUpAt) { rec.status = 'gaveup'; log(`submit ${rec.hash.slice(0, 10)} gave up after ${rec.attempts} attempts`); return; }
    await sleep(Math.min(500 * rec.attempts, 3000));
  }
}
async function waitIncluded(recs, timeoutMs) {
  const end = Date.now() + timeoutMs;
  while (Date.now() < end) {
    if (recs.every((r) => r.blockNumber != null || r.status === 'rejected' || r.status === 'gaveup')) return true;
    await sleep(250);
  }
  return false;
}

// ---------------- setup ----------------
async function setup() {
  genesis = await rpc.req('chain_getBlockHash', [0]);
  const chain = await rpc.req('system_chain');
  const rv = await rpc.req('state_getRuntimeVersion');
  codecs = await getTypedCodecs(myriad);
  offline = await getOfflineApi({ genesis, getMetadata: myriad.getMetadata });
  const metaSpec = offline.constants.System.Version.spec_version;
  log(`chain ${chain} genesis ${genesis.slice(0, 18)}… runtime ${rv.specName} v${rv.specVersion} (descriptor metadata v${metaSpec})`);
  if (rv.specVersion !== metaSpec) throw new Error(`runtime spec ${rv.specVersion} != descriptor metadata spec ${metaSpec}`);

  finalized = await finalizedHead();
  scannerLoop(); // background
  while (scannedTo < 0) await sleep(50);

  const setupTxs = [];
  const run = async (acct, call, label) => {
    const rec = await signTx(acct, call, { label, setup: true });
    await submit(rec, Date.now() + 60_000);
    setupTxs.push(rec); return rec;
  };
  // 1. fund Bob and the test accounts from Alice (sequential nonces, submitted back to back)
  alice.nonce = (await accountAt(alice.pub, finalized.hash)).nonce;
  const funding = [];
  for (const [a, amt] of [[bob, FUND_BOB], ...testAccounts.map((t) => [t, FUND])]) {
    const free = (await accountAt(a.pub, finalized.hash)).free;
    if (free >= amt / 2n) { log(`setup: ${a.name} already funded (${free})`); continue; }
    funding.push(await run(alice, offline.tx.Balances.transfer_keep_alive({ dest: { type: 'Id', value: a.address }, value: amt }), `fund ${a.name}`));
  }
  if (!(await waitIncluded(funding, 90_000))) throw new Error('funding transfers not finalized within 90 s');
  for (const r of funding) if (!r.ok) throw new Error(`${r.label} failed: ${r.status} ${r.dispatchError ?? r.submitNote ?? ''}`);
  log(`setup: funded ${funding.length} accounts`);

  // 2. Bob registers a server (AlreadyExists is fine: server from an earlier run)
  const fh = await finalizedHead();
  bob.nonce = (await accountAt(bob.pub, fh.hash)).nonce;
  const reg = await run(bob, offline.tx.Server.register({ api_url: new TextEncoder().encode('https://stress-test.myriad.invalid'), stake_amount: undefined }), 'Server.register');
  if (!(await waitIncluded([reg], 90_000))) throw new Error('Server.register not finalized within 90 s');
  if (!reg.ok && !/AlreadyExists/.test(reg.dispatchError ?? '')) throw new Error(`Server.register failed: ${reg.status} ${reg.dispatchError ?? reg.submitNote ?? ''}`);
  log(`setup: server registered by //Bob (${reg.ok ? 'new' : 'already existed'})`);

  // 3. nonces of test accounts from finalized state
  const fh2 = await finalizedHead();
  for (const a of testAccounts) {
    const st = await accountAt(a.pub, fh2.hash);
    a.nonce = st.nonce; a.startNonce = st.nonce;
    if (st.free < TIP * 20n) throw new Error(`${a.name} balance too low after funding: ${st.free}`);
  }
  for (const r of setupTxs) tracked.delete(r.hash);
}

// ---------------- load ----------------
const loadTxs = [];
async function load() {
  const info = (k) => ({ server_id: bob.address, reference_type: new TextEncoder().encode('people'), reference_id: new TextEncoder().encode(`stress-${T0}-${k}`), ft_identifier: new TextEncoder().encode('native') });
  const start = Date.now();
  const windowEnd = start + DURATION_S * 1000;
  const giveUpAt = windowEnd + SUBMIT_GIVEUP_MS;
  const busy = new Set();      // accounts with a tx not yet accepted by the node
  const blocked = new Set();   // accounts with a rejected / given-up tx (later nonces would be past a gap)
  const inflight = [];
  let next = 0;
  log(`load: ${TOTAL} x Tipping.send_tip at ${RATE} tx/s over ${DURATION_S} s from ${N_ACCOUNTS} accounts`);
  while (next < TOTAL) {
    const due = start + Math.round((next / RATE) * 1000);
    if (Date.now() < due) { await sleep(Math.min(100, due - Date.now())); continue; }
    if (Date.now() > giveUpAt) { log(`load: stopping, ${TOTAL - next} slots never sent (all accounts busy/blocked)`); break; }
    // prefer round-robin account, else any account that is free
    const order = testAccounts.map((_, i) => testAccounts[(next + i) % N_ACCOUNTS]);
    const acct = order.find((a) => !busy.has(a) && !blocked.has(a));
    if (!acct) { await sleep(100); continue; }
    const slot = next++;
    busy.add(acct);
    const rec = await signTx(acct, offline.tx.Tipping.send_tip({ info: info(slot), amount: TIP }), { slot, dueMs: due });
    loadTxs.push(rec);
    inflight.push(submit(rec, giveUpAt).then(() => {
      if (rec.status === 'accepted') busy.delete(acct);
      else { blocked.add(acct); busy.delete(acct); }
      log(`tx #${String(slot).padStart(2)} ${acct.name.split('//').pop()} nonce ${rec.nonce} ${rec.hash.slice(0, 12)} ${rec.status}${rec.attempts > 1 ? ` (${rec.attempts} attempts)` : ''}`);
    }));
  }
  await Promise.all(inflight);
  return { start, windowEnd };
}

// ---------------- reconcile & report ----------------
function pct(sorted, p) { if (!sorted.length) return null; return sorted[Math.min(sorted.length - 1, Math.ceil((p / 100) * sorted.length) - 1)]; }
async function reconcile({ start, windowEnd }) {
  const lastSubmit = Math.max(...loadTxs.map((r) => r.firstSubmitMs));
  log(`reconcile: waiting up to ${SETTLE_MS / 1000} s for inclusion`);
  await waitIncluded(loadTxs, Math.max(0, lastSubmit + SETTLE_MS - Date.now()));
  scannerStop = true; await sleep(300);
  // final full scan up to the current finalized head
  const head = await finalizedHead(); finalized = head; await scanTo(head);
  const nonces = new Map();
  for (const a of testAccounts) nonces.set(a.address, (await accountAt(a.pub, head.hash)).nonce);

  for (const r of loadTxs) {
    if (r.blockNumber != null) { r.category = r.ok ? 'finalized-ok' : 'failed-in-block'; r.latencyMs = r.finalizedMs - r.firstSubmitMs; continue; }
    const finNonce = nonces.get(r.signer);
    if (finNonce > r.nonce) { r.category = 'lost'; r.proof = `finalized nonce ${finNonce} > ${r.nonce} at #${head.number}; hash in no finalized block #${r.birth + 1}..#${head.number}`; }
    else if (head.number >= r.birth + PERIOD) { r.category = 'lost'; r.proof = `mortality expired (birth #${r.birth} + ${PERIOD} <= finalized #${head.number}); hash in no finalized block`; }
    else { r.category = 'unresolved'; r.proof = `not in finalized blocks up to #${head.number}; finalized nonce ${finNonce} <= ${r.nonce}; mortal until #${r.birth + PERIOD}; submit status ${r.status}${r.submitNote ? ' (' + r.submitNote + ')' : ''}`; }
  }
  const lines = loadTxs.map((r) => JSON.stringify({
    hash: r.hash, category: r.category, signer: r.signer, nonce: r.nonce, firstSubmitMs: r.firstSubmitMs,
    slot: r.slot, dueMs: r.dueMs, submitAttempts: r.attempts, submitStatus: r.status, birthBlock: r.birth,
    block: r.blockNumber ?? null, blockHash: r.blockHash ?? null, finalizedSeenMs: r.finalizedMs ?? null, latencyMs: r.latencyMs ?? null,
    events: r.events, dispatchError: r.dispatchError, proof: r.proof, submitNote: r.submitNote,
  }));
  fs.writeFileSync(path.join(OUT, 'txs.jsonl'), lines.join('\n') + '\n');

  const cats = { 'finalized-ok': 0, 'failed-in-block': 0, lost: 0, unresolved: 0 };
  for (const r of loadTxs) cats[r.category]++;
  const sent = loadTxs.length;
  const inWindow = loadTxs.filter((r) => r.firstSubmitMs <= windowEnd + 2000).length;
  const required = Math.ceil(0.95 * TOTAL);
  const delivered = inWindow >= required;
  const lat = loadTxs.filter((r) => r.latencyMs != null).map((r) => r.latencyMs).sort((a, b) => a - b);
  const p95 = pct(lat, 95), p50 = pct(lat, 50);
  let verdict, code, reasons = [];
  if ((p95 != null && p95 >= P95_LIMIT_MS) || cats.lost > 0) {
    verdict = 'FAIL'; code = 1;
    if (p95 != null && p95 >= P95_LIMIT_MS) reasons.push(`p95 latency ${p95} ms >= ${P95_LIMIT_MS} ms`);
    if (cats.lost) reasons.push(`${cats.lost} transaction(s) proven lost`);
  } else if (!delivered || cats.unresolved > 0) {
    verdict = "CAN'T TELL"; code = 4;
    if (!delivered) reasons.push(`load not delivered: ${inWindow}/${TOTAL} first-submitted within window+2s (need ${required}) — test-tool issue, not a chain problem`);
    if (cats.unresolved) reasons.push(`${cats.unresolved} transaction(s) unresolved`);
  } else { verdict = 'PASS'; code = 0; reasons.push(`p95 ${p95} ms < ${P95_LIMIT_MS} ms, no proven loss, load delivered`); }

  const summary = {
    endpoint: ENDPOINT, call: 'Tipping.send_tip', targetRate: RATE, durationS: DURATION_S, target: TOTAL,
    sent, ...cats, sumCheck: Object.values(cats).reduce((a, b) => a + b, 0) === sent,
    firstSubmittedInWindow: inWindow, deliveredRateTxPerS: +(inWindow / DURATION_S).toFixed(4),
    latencyMs: { p50, p95, max: lat.at(-1) ?? null, n: lat.length },
    finalizedHeadAtReconcile: head.number, toolIssues, verdict, exitCode: code, reasons,
  };
  fs.writeFileSync(path.join(OUT, 'summary.json'), JSON.stringify(summary, null, 2) + '\n');
  log('================ RESULT ================');
  log(`sent ${sent} | finalized-ok ${cats['finalized-ok']} | failed-in-block ${cats['failed-in-block']} | lost ${cats.lost} | unresolved ${cats.unresolved}`);
  log(`delivered: ${inWindow}/${TOTAL} first-submitted within window+2s -> ${summary.deliveredRateTxPerS} tx/s (target ${RATE})`);
  log(`latency first-submit -> finalized: p50 ${p50} ms, p95 ${p95} ms, max ${summary.latencyMs.max} ms (n=${lat.length})`);
  log(`test-tool issues (not chain problems): reconnects ${toolIssues.reconnects}, rpc retries ${toolIssues.rpcErrors}, submit resends ${toolIssues.submitRetries}, sign retries ${toolIssues.signRetries}`);
  if (cats['failed-in-block']) log(`note: failed-in-block dispatch errors: ${[...new Set(loadTxs.filter((r) => r.category === 'failed-in-block').map((r) => r.dispatchError))].join(' | ')}`);
  log(`VERDICT: ${verdict} (exit ${code}) — ${reasons.join('; ')}`);
  log(`files: ${path.join(OUT, 'txs.jsonl')} ${path.join(OUT, 'summary.json')}`);
  return code;
}

// ---------------- main ----------------
let code;
try {
  await setup();
} catch (e) {
  log('SETUP FAILED:', e.stack || e.message);
  fs.writeFileSync(path.join(OUT, 'summary.json'), JSON.stringify({ verdict: 'SETUP FAILED', exitCode: 3, error: String(e.message) }, null, 2) + '\n');
  scannerStop = true; rpc.close(); logFile.end(); await sleep(100); process.exit(3);
}
try {
  const w = await load();
  code = await reconcile(w);
} catch (e) {
  log('ERROR during load/reconcile (tool problem):', e.stack || e.message);
  code = 4;
}
scannerStop = true; rpc.close(); logFile.end(); await sleep(100);
process.exit(code);
