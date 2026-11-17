// Aventus (AvN) stress test: TokenManager.transfer (AVT) at RATE tx/s for DURATION s from N_ACCOUNTS
// accounts derived from //Alice. usage: node stress.mjs <ws-endpoint> <output-dir>
//
// Design
// - PAPI client (auto-reconnecting ws provider) is used to read the finalized block and to build + sign
//   each transaction (mortal, period MORTALITY, `at` = latest finalized block, explicit nonce).
// - A separate small JSON-RPC client of our own (auto-reconnect) submits the signed bytes with
//   author_submitExtrinsic and walks every finalized block (chain_getFinalizedHead / getBlockHash /
//   getBlock / System.Events) so reconciliation does not depend on a single subscription surviving.
// - Per account: nonces are assigned locally from the finalized nonce, each nonce is signed exactly once,
//   and the next nonce is not signed until the node has acknowledged the previous one. A failed send is
//   retried with the identical bytes.
//
// Categories (sum to sent):
//   finalized-ok      in a finalized block, System.ExtrinsicSuccess
//   failed-in-block   in a finalized block, System.ExtrinsicFailed
//   lost              node acknowledged it, not in any finalized block, and (signer finalized nonce > nonce
//                     or finalized head >= birth block + mortality period)
//   unresolved        anything else the node acknowledged
//   not-accepted      (tool-side) never acknowledged by the node: every send failed in transport or was
//                     rejected at submission. Not a chain verdict; makes the result CAN'T TELL.
// Exit: 0 PASS, 1 FAIL (p95 >= 60 s or proven loss), 3 setup failed, 4 CAN'T TELL.
import fs from 'node:fs';
import path from 'node:path';
import { createClient, getTypedCodecs, AccountId } from 'polkadot-api';
import { getWsProvider } from 'polkadot-api/ws';
import { getTxCreator } from 'polkadot-api/tx-creator';
import { sr25519CreateDerive } from '@polkadot-labs/hdkd';
import { DEV_PHRASE, entropyToMiniSecret, mnemonicToEntropy } from '@polkadot-labs/hdkd-helpers';
import { blake2b } from '@noble/hashes/blake2.js';
import { aventus } from '@polkadot-api/descriptors';

const [ENDPOINT, OUT] = process.argv.slice(2);
if (!ENDPOINT || !OUT) { console.error('usage: node stress.mjs <ws-endpoint> <output-dir>'); process.exit(3); }
const RATE = Number(process.env.RATE ?? 0.25);         // tx/s
const DURATION = Number(process.env.DURATION ?? 200);  // s
const N_ACCOUNTS = 4;
const MORTALITY = 64;                                  // blocks
const P95_LIMIT_MS = 60_000;
const RECONCILE_WAIT_MS = Number(process.env.RECONCILE_WAIT_MS ?? 120_000);
const AMOUNT = 10n ** 15n;                             // 0.001 AVT per transfer
const FUND = 10n ** 21n;                               // 1000 AVT per test account

fs.mkdirSync(OUT, { recursive: true });
const logFile = fs.createWriteStream(path.join(OUT, 'run.log'));
const log = (...a) => { const s = `[${new Date().toISOString()}] ${a.join(' ')}`; console.log(s); logFile.write(s + '\n'); };
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const hex = (u8) => '0x' + Buffer.from(u8).toString('hex');
const unhex = (h) => Uint8Array.from(Buffer.from(h.replace(/^0x/, ''), 'hex'));
const withTimeout = (p, ms, what) => Promise.race([p, sleep(ms).then(() => { throw new Error(`${what} timed out`); })]);

// ---------- own JSON-RPC client with reconnect ----------
class Rpc {
  constructor(url) { this.url = url; this.id = 0; this.pending = new Map(); this.reconnects = 0; this.closed = false; this.connect(); }
  connect() {
    if (this.closed) return;
    const ws = new WebSocket(this.url); this.ws = ws; this.open = false;
    ws.onopen = () => { if (this.ws === ws) this.open = true; };
    ws.onmessage = (m) => {
      let msg; try { msg = JSON.parse(m.data); } catch { return; }
      const p = this.pending.get(msg.id); if (!p) return;
      this.pending.delete(msg.id); clearTimeout(p.t);
      if (msg.error) p.rej(Object.assign(new Error(msg.error.message), { rpc: msg.error })); else p.res(msg.result);
    };
    ws.onerror = () => { if (!this.open) { try { ws.close(); } catch {} } };
    ws.onclose = () => {
      if (this.ws !== ws) return;
      for (const [id, p] of this.pending) { clearTimeout(p.t); p.rej(Object.assign(new Error('connection closed'), { transport: true })); }
      this.pending.clear();
      if (this.closed) return;
      this.reconnects++;
      this.open = false;
      setTimeout(() => this.connect(), 500);
    };
  }
  async call(method, params = [], timeoutMs = 30_000) {
    await this.waitOpen(timeoutMs);
    const id = ++this.id;
    return new Promise((res, rej) => {
      const t = setTimeout(() => {
        this.pending.delete(id);
        rej(Object.assign(new Error(`${method} timed out`), { transport: true }));
        try { this.ws.close(); } catch {}
      }, timeoutMs);
      this.pending.set(id, { res, rej, t });
      if (this.ws.readyState !== WebSocket.OPEN) { this.pending.delete(id); clearTimeout(t); rej(Object.assign(new Error('not connected'), { transport: true })); return; }
      try { this.ws.send(JSON.stringify({ jsonrpc: '2.0', id, method, params })); }
      catch (e) { this.pending.delete(id); clearTimeout(t); rej(Object.assign(e, { transport: true })); }
    });
  }
  // retry transport errors until deadline
  async callRetry(method, params, deadline = Date.now() + 120_000) {
    for (;;) {
      try { return await this.call(method, params); }
      catch (e) { if (!e.transport || Date.now() > deadline) throw e; await sleep(1000); }
    }
  }
  async waitOpen(ms) {
    const end = Date.now() + ms;
    while (!this.open) { if (Date.now() > end) throw Object.assign(new Error('ws connect timed out'), { transport: true }); await sleep(100); }
  }
  close() { this.closed = true; try { this.ws.close(); } catch {} }
}

// ---------- keys ----------
const derive = sr25519CreateDerive(entropyToMiniSecret(mnemonicToEntropy(DEV_PHRASE)));
const ss58 = AccountId(42);
function account(pathStr) {
  const kp = derive(pathStr);
  return { name: pathStr, pub: kp.publicKey, address: ss58.dec(kp.publicKey), signer: getTxCreator(kp.publicKey, 'Sr25519', kp.sign),
    nonce: 0, lastAcked: Promise.resolve(true), stuck: false };
}
const seed = account('//Alice');
const accounts = Array.from({ length: N_ACCOUNTS }, (_, i) => account(`//Alice//stress//${i}`));
const recipient = account('//Alice//stress//sink');

// ---------- state ----------
const txs = [];          // all transactions (setup + load)
const byHash = new Map();
let codecs, rpc, client, api;
let scannedTo = 0;       // highest finalized block number scanned
let startBlock = 0;
const SYS_EVENTS_KEY = '0x26aa394eea5630e07c48ae0c9558cef780d41e5e16056765bc8461851072c9d7';
const accountKey = (pub) => '0x26aa394eea5630e07c48ae0c9558cef7b99d880ec681799c0cf30e8886371da9' + hex(blake2b(pub, { dkLen: 16 })).slice(2) + hex(pub).slice(2);

async function finalizedNonce(acct, at) {
  const v = await rpc.callRetry('state_getStorage', [accountKey(acct.pub), at]);
  return v ? codecs.query.System.Account.value.dec(v).nonce : 0;
}

// ---------- finalized-chain tracker ----------
async function scanOnce() {
  const head = await rpc.call('chain_getFinalizedHead');
  const hdr = await rpc.call('chain_getHeader', [head]);
  const headNum = parseInt(hdr.number, 16);
  const seenAt = Date.now();
  while (scannedTo < headNum) {
    const n = scannedTo + 1;
    const h = await rpc.call('chain_getBlockHash', [n]);
    const blk = await rpc.call('chain_getBlock', [h]);
    let events = null;
    blk.block.extrinsics.forEach((x, idx) => {
      const hash = hex(blake2b(unhex(x), { dkLen: 32 }));
      const tx = byHash.get(hash);
      if (tx && !tx.block) { tx.block = { number: n, hash: h, idx }; tx.finalizedMs = seenAt; }
    });
    for (const tx of txs) {
      if (tx.block?.number === n && tx.ok === undefined) {
        if (!events) events = codecs.query.System.Events.value.dec(await rpc.call('state_getStorage', [SYS_EVENTS_KEY, h]));
        const mine = events.filter((e) => e.phase.type === 'ApplyExtrinsic' && e.phase.value === tx.block.idx && e.event.type === 'System');
        if (mine.some((e) => e.event.value.type === 'ExtrinsicSuccess')) tx.ok = true;
        else if (mine.some((e) => e.event.value.type === 'ExtrinsicFailed')) { tx.ok = false; tx.error = JSON.stringify(mine.find((e) => e.event.value.type === 'ExtrinsicFailed').event.value.value.dispatch_error, (k, v) => typeof v === 'bigint' ? v.toString() : v); }
        else tx.ok = null; // no outcome event (should not happen)
      }
    }
    scannedTo = n;
  }
  return { headNum, head };
}
let trackerRunning = true;
let trackerDone = Promise.resolve();
let trackerErrors = 0;
async function tracker() {
  while (trackerRunning) {
    try { await scanOnce(); } catch (e) { trackerErrors++; }
    await sleep(500);
  }
}

// ---------- sign + submit ----------
const isTransport = (e) => e.transport || /closed|disconnect|timed out|ECONN|socket/i.test(e.message ?? '');

async function signTx(acct, call, deadline) {
  for (;;) {
    try {
      const fin = await withTimeout(client.getFinalizedBlock(), 15_000, 'getFinalizedBlock');
      const bytes = await withTimeout(call.create(acct.signer, { nonce: acct.nonce, mortality: { mortal: true, period: MORTALITY, at: { hash: fin.hash, number: fin.number } } }), 30_000, 'sign');
      return { bytes, birth: fin.number, birthHash: fin.hash };
    } catch (e) {
      if (Date.now() > deadline) throw e;
      log(`sign retry ${acct.name} nonce ${acct.nonce}: ${e.message}`);
      await sleep(1000);
    }
  }
}

// Sends identical bytes until the node acknowledges (or definitively rejects) them.
async function submitUntilAck(tx, deadline) {
  const hexTx = hex(tx.bytes);
  for (;;) {
    if (tx.block) { tx.acked = true; return true; }
    tx.attempts++;
    if (tx.firstSubmitMs === undefined) tx.firstSubmitMs = Date.now();
    try {
      const h = await rpc.call('author_submitExtrinsic', [hexTx]);
      if (h !== tx.hash) log(`WARN node returned hash ${h} for ${tx.hash}`);
      tx.acked = true; return true;
    } catch (e) {
      const code = e.rpc?.code; const data = String(e.rpc?.data ?? '');
      if (code === 1013 || /already imported/i.test(e.message)) { tx.acked = true; return true; }
      if (/outdated|stale/i.test(e.message + data)) { tx.acked = true; tx.note = 'stale-on-resend'; return true; } // nonce already used: resolved by the chain scan
      if (isTransport(e)) { tx.sendErrors++; }
      else {
        tx.sendErrors++; tx.rejections = (tx.rejections ?? 0) + 1; tx.lastReject = `${code} ${e.message} ${data}`;
        log(`submit rejected ${tx.signer} nonce ${tx.nonce}: ${tx.lastReject}`);
        if (tx.rejections >= 5) { tx.rejected = true; return false; }
      }
      if (Date.now() > deadline) return false;
      await sleep(1000);
    }
  }
}

async function sendFrom(acct, makeCall, kind, deadline, scheduledMs) {
  // gap rule: do not sign the next nonce before the previous one is acknowledged
  if (!(await acct.lastAcked)) { acct.stuck = true; }
  if (acct.stuck) { log(`skip ${kind} for ${acct.name}: previous nonce unresolved`); return null; }
  const { bytes, birth, birthHash } = await signTx(acct, makeCall(), deadline);
  const tx = { kind, signer: acct.address, signerName: acct.name, nonce: acct.nonce, hash: hex(blake2b(bytes, { dkLen: 32 })), bytes, birth, birthHash,
    scheduledMs, attempts: 0, sendErrors: 0 };
  acct.nonce++;  // nonce consumed by these exact bytes; never re-signed
  txs.push(tx); byHash.set(tx.hash, tx);
  const p = submitUntilAck(tx, deadline);
  acct.lastAcked = p;
  return { tx, acked: p };
}

async function waitFinal(list, ms) {
  const end = Date.now() + ms;
  while (Date.now() < end && list.some((t) => t.ok === undefined)) await sleep(500);
}

// ---------- main ----------
async function main() {
  // setup
  try {
    rpc = new Rpc(ENDPOINT);
    await rpc.waitOpen(60_000);
    codecs = await getTypedCodecs(aventus);
    client = createClient(getWsProvider(ENDPOINT));
    api = client.getTypedApi(aventus);
    const fin = await withTimeout(client.getFinalizedBlock(), 60_000, 'finalized block');
    startBlock = fin.number; scannedTo = startBlock;
    log(`connected ${ENDPOINT}; chain=${await rpc.call('system_chain')} finalized #${fin.number}`);
    const token_id = await withTimeout(api.query.TokenManager.AVTTokenContract.getValue({ at: 'finalized' }), 60_000, 'AVTTokenContract');
    if (!token_id) throw new Error('TokenManager.AVTTokenContract is empty');
    log(`AVT token contract ${token_id}`);
    globalThis.TOKEN_ID = token_id;
    for (const a of [seed, ...accounts]) a.nonce = await finalizedNonce(a, fin.hash);
    trackerDone = tracker();

    // fund test accounts from the seed (sequential nonces; one tx per account)
    const setupDeadline = Date.now() + 180_000;
    const fundTxs = [];
    for (const a of accounts) {
      const r = await sendFrom(seed, () => api.tx.Balances.transfer_keep_alive({ dest: { type: 'Id', value: a.address }, value: FUND }), 'setup-fund', setupDeadline);
      if (!r || !(await r.acked)) throw new Error(`funding tx for ${a.name} not accepted: ${r?.tx.lastReject ?? 'skipped'}`);
      fundTxs.push(r.tx);
    }
    await waitFinal(fundTxs, Math.max(0, setupDeadline - Date.now()));
    const bad = fundTxs.filter((t) => t.ok !== true);
    if (bad.length) throw new Error(`funding not finalized ok: ${bad.map((t) => `${t.nonce}:${t.ok}:${t.error ?? ''}`).join(', ')}`);
    log(`funded ${accounts.length} accounts with ${FUND} each (blocks ${fundTxs.map((t) => t.block.number).join(',')})`);
  } catch (e) {
    log(`SETUP FAILED: ${e.stack ?? e}`);
    return 3;
  }

  // load
  const total = Math.round(RATE * DURATION);
  const intervalMs = 1000 / RATE;
  const T0 = Date.now() + 1000;
  const loadDeadline = T0 + DURATION * 1000 + 60_000; // stop retrying sends 60 s after the window
  log(`load: ${total} x TokenManager.transfer, ${RATE} tx/s for ${DURATION} s, ${N_ACCOUNTS} accounts`);
  const perAccount = accounts.map(() => []);
  for (let i = 0; i < total; i++) perAccount[i % N_ACCOUNTS].push({ i, at: T0 + i * intervalMs });
  const loadTxs = [];
  await Promise.all(accounts.map(async (a, ai) => {
    for (const slot of perAccount[ai]) {
      const w = slot.at - Date.now(); if (w > 0) await sleep(w);
      try {
        const r = await sendFrom(a, () => api.tx.TokenManager.transfer({ to: recipient.address, token_id: globalThis.TOKEN_ID, amount: AMOUNT }), 'load', loadDeadline, slot.at);
        if (r) { loadTxs.push(r.tx); log(`#${slot.i} ${a.name} nonce ${r.tx.nonce} ${r.tx.hash} (lag ${r.tx.firstSubmitMs - slot.at} ms)`); }
      } catch (e) { log(`#${slot.i} ${a.name}: could not sign: ${e.message}`); }
    }
    await a.lastAcked;
  }));
  log(`load done: ${loadTxs.length} sent; waiting up to ${RECONCILE_WAIT_MS / 1000} s for finalization`);

  // reconcile
  trackerRunning = false; await trackerDone; // reconcile loop scans from here on (no concurrent scans)
  const recEnd = Date.now() + RECONCILE_WAIT_MS;
  let headNum = scannedTo;
  const finNonce = new Map();
  const classify = () => {
    for (const t of loadTxs) {
      if (t.block && t.ok !== undefined) t.category = t.ok === true ? 'finalized-ok' : t.ok === false ? 'failed-in-block' : 'unresolved';
      else if (!t.acked) t.category = 'not-accepted';
      else if (scannedTo >= headNum && (finNonce.get(t.signer) > t.nonce || headNum >= t.birth + MORTALITY)) t.category = 'lost';
      else t.category = 'unresolved';
    }
  };
  for (;;) {
    try {
      const r = await scanOnce(); headNum = r.headNum;
      for (const a of accounts) finNonce.set(a.address, await finalizedNonce(a, r.head));
    } catch (e) { trackerErrors++; }
    classify();
    if (!loadTxs.some((t) => t.category === 'unresolved') || Date.now() > recEnd) break;
    await sleep(1000);
  }

  // report
  const windowEnd = T0 + DURATION * 1000 + 2000;
  const inWindow = loadTxs.filter((t) => t.firstSubmitMs <= windowEnd).length;
  const delivered = inWindow >= 0.95 * total;
  const cats = {};
  for (const c of ['finalized-ok', 'failed-in-block', 'lost', 'unresolved', 'not-accepted']) cats[c] = loadTxs.filter((t) => t.category === c).length;
  const lat = loadTxs.filter((t) => t.block).map((t) => t.finalizedMs - t.firstSubmitMs).sort((a, b) => a - b);
  const pct = (q) => lat.length ? lat[Math.min(lat.length - 1, Math.ceil(q * lat.length) - 1)] : null;
  const p95 = pct(0.95);
  const lines = loadTxs.map((t) => JSON.stringify({ hash: t.hash, category: t.category, signer: t.signer, nonce: t.nonce, firstSubmitMs: t.firstSubmitMs,
    scheduledMs: t.scheduledMs, finalizedMs: t.finalizedMs ?? null, latencyMs: t.block ? t.finalizedMs - t.firstSubmitMs : null,
    block: t.block?.number ?? null, birthBlock: t.birth, attempts: t.attempts, sendErrors: t.sendErrors, error: t.error ?? t.lastReject ?? null }));
  fs.writeFileSync(path.join(OUT, 'transactions.jsonl'), lines.join('\n') + (lines.length ? '\n' : ''));

  let verdict, code, reason;
  if (cats.lost > 0 || (p95 !== null && p95 >= P95_LIMIT_MS)) { verdict = 'FAIL'; code = 1; reason = cats.lost > 0 ? `${cats.lost} proven lost` : `p95 ${p95} ms >= ${P95_LIMIT_MS}`; }
  else if (!delivered || cats.unresolved > 0 || cats['not-accepted'] > 0 || p95 === null) {
    verdict = "CAN'T TELL"; code = 4;
    reason = [!delivered && `load not delivered (${inWindow}/${total} first-submitted in window, need ${Math.ceil(0.95 * total)})`, cats.unresolved && `${cats.unresolved} unresolved`, cats['not-accepted'] && `${cats['not-accepted']} not accepted by node (tool-side)`, p95 === null && 'no finalized transactions'].filter(Boolean).join('; ');
  } else { verdict = 'PASS'; code = 0; reason = `p95 ${p95} ms < ${P95_LIMIT_MS}, no loss, load delivered`; }

  const span = loadTxs.length > 1 ? (Math.max(...loadTxs.map((t) => t.firstSubmitMs)) - Math.min(...loadTxs.map((t) => t.firstSubmitMs))) / 1000 : 0;
  const summary = {
    chain: 'aventus', endpoint: ENDPOINT, call: 'TokenManager.transfer(AVT)', target: { rate: RATE, durationS: DURATION, total, accounts: N_ACCOUNTS },
    sent: loadTxs.length, categories: cats, firstSubmittedInWindow: inWindow, delivered,
    deliveredRateTxPerS: +(inWindow / DURATION).toFixed(4), sentSpanS: span,
    latencyMs: { p50: pct(0.5), p95, max: lat.length ? lat[lat.length - 1] : null, n: lat.length },
    toolSide: { resends: loadTxs.reduce((s, t) => s + t.attempts - 1, 0), sendErrors: loadTxs.reduce((s, t) => s + t.sendErrors, 0),
      rpcReconnects: rpc.reconnects, trackerErrors, maxStartLagMs: Math.max(0, ...loadTxs.map((t) => t.firstSubmitMs - t.scheduledMs)) },
    verdict, exitCode: code, reason,
  };
  fs.writeFileSync(path.join(OUT, 'summary.json'), JSON.stringify(summary, null, 2) + '\n');
  log('---- result ----');
  log(`sent ${summary.sent} | finalized-ok ${cats['finalized-ok']} | failed-in-block ${cats['failed-in-block']} | lost ${cats.lost} | unresolved ${cats.unresolved} | not-accepted ${cats['not-accepted']}`);
  log(`delivered ${inWindow}/${total} in window (${summary.deliveredRateTxPerS} tx/s) | latency p50 ${summary.latencyMs.p50} ms p95 ${p95} ms max ${summary.latencyMs.max} ms`);
  log(`tool-side: resends ${summary.toolSide.resends}, send errors ${summary.toolSide.sendErrors}, rpc reconnects ${rpc.reconnects}, tracker errors ${trackerErrors}, max start lag ${summary.toolSide.maxStartLagMs} ms`);
  log(`VERDICT: ${verdict} (exit ${code}) — ${reason}`);
  log(`files: ${path.join(OUT, 'transactions.jsonl')} ${path.join(OUT, 'summary.json')}`);
  return code;
}

const code = await main().catch((e) => { log(`UNEXPECTED: ${e.stack ?? e}`); return 4; });
try { client?.destroy(); } catch {}
rpc?.close();
await new Promise((r) => logFile.end(r));
process.exit(code);
