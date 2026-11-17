// Energy Web X stress test: LiquidStaking.stake at RATE tx/s for DURATION s from 4 derived accounts.
// usage: node run.mjs <ws-endpoint> <output-dir>
// Exit: 0 PASS, 1 FAIL (chain missed a threshold), 3 setup failed, 4 CAN'T TELL.
//
// Categories (sum to sent):
//   finalized-ok     in a finalized block, ExtrinsicSuccess
//   failed-in-block  in a finalized block, ExtrinsicFailed
//   lost             in no finalized block AND (signer's finalized nonce > tx nonce OR mortality expired)
//   unresolved       none of the above could be proven by the end of the grace period
//   rejected         (extra) the node returned a validity error on submit and never included it; the
//                    signer is then halted (no later nonces). Counts as "not provable" → CAN'T TELL.
// Tool-side problems (send errors, reconnects, resends) are counted under "tool" and never as chain problems.
import fs from 'node:fs';
import path from 'node:path';
import { createClient, AccountId, getTypedCodecs } from 'polkadot-api';
import { getWsProvider } from 'polkadot-api/ws';
import { getTxCreator } from 'polkadot-api/tx-creator';
import { sr25519CreateDerive } from '@polkadot-labs/hdkd';
import { DEV_PHRASE, entropyToMiniSecret, mnemonicToEntropy } from '@polkadot-labs/hdkd-helpers';
import { blake2b } from '@noble/hashes/blake2.js';
import { ewx } from '@polkadot-api/descriptors';

const [ENDPOINT, OUT] = process.argv.slice(2);
if (!ENDPOINT || !OUT) { console.error('usage: run.mjs <ws-endpoint> <output-dir>'); process.exit(3); }
fs.mkdirSync(OUT, { recursive: true });

const RATE = 0.25, DURATION = +(process.env.EWX_DURATION_S ?? 200),  // env override only for quick dev runs
      ACCOUNTS = 4, PERIOD = 64;   // mortality period in blocks
const TOTAL = Math.round(RATE * DURATION), INTERVAL = 1000 / RATE;
const P95_LIMIT_MS = 60_000, GRACE_MS = 180_000;                // max wait after the window for inclusion
const FUND = 10_000n * 10n ** 18n;

const t00 = Date.now();
const logf = fs.createWriteStream(path.join(OUT, 'log.txt'));
const log = (...a) => { const s = `[${((Date.now() - t00) / 1000).toFixed(1)}s] ${a.join(' ')}`; console.log(s); logf.write(s + '\n'); };
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const hex = (u8) => '0x' + Buffer.from(u8).toString('hex');
const unhex = (h) => Uint8Array.from(Buffer.from(h.slice(2), 'hex'));
const txHash = (bytesHex) => hex(blake2b(unhex(bytesHex), { dkLen: 32 }));
const tool = { reconnects: 0, sendErrors: 0, resends: 0, rpcRetries: 0 };

// ---------- client ----------
let connectedOnce = false;
// `dropped` rejects when the socket closes, so an in-flight submit is resent right away instead of waiting a timeout.
let onDrop, dropped;
const armDrop = () => { dropped = new Promise((_, rej) => { onDrop = rej; }); dropped.catch(() => {}); };
armDrop();
const client = createClient(getWsProvider(ENDPOINT, {
  timeout: 10_000,
  onStatusChanged: (s) => {
    if (s.type === 'CONNECTED') { if (connectedOnce) { tool.reconnects++; log('ws reconnected'); } connectedOnce = true; }
    else if (s.type === 'CLOSE' || s.type === 'ERROR') { log('ws closed; reconnecting'); onDrop(new Error('connection dropped')); armDrop(); }
  },
}));
const api = client.getTypedApi(ewx);
const codecs = await getTypedCodecs(ewx);
// Any crash before the load starts is a setup failure (exit 3); the handlers are replaced when the load starts.
const setupFail = (e) => { console.error('SETUP FAILED:', e?.stack ?? e); process.exit(3); };
process.on('uncaughtException', setupFail); process.on('unhandledRejection', setupFail);

const withTimeout = (p, ms, what) => Promise.race([p, new Promise((_, rej) => setTimeout(() => rej(new Error(`timeout: ${what}`)), ms))]);
// Read-only RPC with retry across connection problems (tool-side; never a chain verdict).
async function rpc(method, params, tries = 1e9) {
  for (let i = 0; ; i++) {
    try { return await withTimeout(client._request(method, params), 20_000, method); }
    catch (e) { if (i + 1 >= tries) throw e; tool.rpcRetries++; await sleep(Math.min(5000, 500 * (i + 1))); }
  }
}
async function finalizedHead() {
  const hash = await rpc('chain_getFinalizedHead', []);
  const hdr = await rpc('chain_getHeader', [hash]);
  return { hash, number: parseInt(hdr.number, 16) };
}
const eventsKey = '0x26aa394eea5630e07c48ae0c9558cef780d41e5e16056765bc8461851072c9d7'; // twox128(System)++twox128(Events)
async function blockAt(n) {          // finalized block n → { hash, txs: [{hash, ok}] }
  const hash = await rpc('chain_getBlockHash', [n]);
  const blk = await rpc('chain_getBlock', [hash]);
  const raw = await rpc('state_getStorage', [eventsKey, hash]);
  const events = raw ? codecs.query.System.Events.value.dec(raw) : [];
  const failed = new Set(), done = new Set();
  for (const e of events) {
    if (e.phase.type !== 'ApplyExtrinsic' || e.event.type !== 'System') continue;
    if (e.event.value.type === 'ExtrinsicFailed') failed.add(e.phase.value);
    if (e.event.value.type === 'ExtrinsicSuccess') done.add(e.phase.value);
  }
  return { hash, txs: blk.block.extrinsics.map((x, i) => ({ hash: txHash(x), ok: done.has(i) && !failed.has(i), failed: failed.has(i) })) };
}

// ---------- accounts ----------
const derive = sr25519CreateDerive(entropyToMiniSecret(mnemonicToEntropy(DEV_PHRASE)));
const mk = (p) => { const kp = derive(p); return { path: p, signer: getTxCreator(kp.publicKey, 'Sr25519', kp.sign), address: AccountId().dec(kp.publicKey) }; };
const seed = mk('//Alice');
const runTag = Date.now().toString(36);   // fresh accounts per run, derived from the seed
const accts = Array.from({ length: ACCOUNTS }, (_, i) => ({ ...mk(`//Alice//ewx-stress//${runTag}//${i}`), nonce: 0, halted: null, queue: Promise.resolve() }));

// ---------- setup ----------
let minStake, startBlock;
try {
  log(`connecting ${ENDPOINT}`);
  const chain = await withTimeout(client._request('system_chain', []), 60_000, 'system_chain');
  minStake = await withTimeout(api.constants.LiquidStaking.MinStakingAmount(), 60_000, 'MinStakingAmount');
  log(`chain "${chain}", MinStakingAmount=${minStake}`);
  const need = minStake * BigInt(Math.ceil(TOTAL / ACCOUNTS)) + FUND / 10n;
  if (FUND < need) throw new Error(`funding ${FUND} < need ${need}`);
  const seedNonce = (await api.query.System.Account.getValue(seed.address, { at: 'finalized' })).nonce;
  const fh = await finalizedHead();
  const sends = accts.map(async (a, i) => {
    const bytes = hex(await api.tx.Balances.transfer_keep_alive({ dest: { type: 'Id', value: a.address }, value: FUND })
      .create(seed.signer, { nonce: seedNonce + i, mortality: { mortal: true, period: PERIOD, at: fh } }));
    await withTimeout(client._request('author_submitExtrinsic', [bytes]), 120_000, 'fund submit');
    return txHash(bytes);
  });
  const fundHashes = new Set(await Promise.all(sends));
  log(`funding txs submitted from ${seed.address}`);
  // wait until all 4 funding txs are in finalized blocks
  for (let n = fh.number + 1, deadline = Date.now() + 300_000; fundHashes.size; ) {
    if (Date.now() > deadline) throw new Error('funding not finalized in 300 s');
    const head = await finalizedHead();
    for (; n <= head.number; n++) for (const t of (await blockAt(n)).txs) if (fundHashes.delete(t.hash) && !t.ok) throw new Error('funding transfer failed in block');
    if (fundHashes.size) await sleep(1000);
  }
  for (const a of accts) {
    const acc = await api.query.System.Account.getValue(a.address, { at: 'finalized' });
    if (acc.data.free < need) throw new Error(`account ${a.address} free=${acc.data.free} < ${need}`);
    a.nonce = acc.nonce;
  }
  startBlock = (await finalizedHead()).number;
  log(`setup done: ${ACCOUNTS} accounts funded with ${FUND / 10n ** 18n} EWT each; start block #${startBlock}`);
} catch (e) {
  log('SETUP FAILED:', e?.stack ?? e);
  logf.end(); client.destroy(); process.exit(3);
}

// ---------- load ----------
// Any crash from here on is a tool problem, never a chain verdict.
process.removeAllListeners('uncaughtException'); process.removeAllListeners('unhandledRejection');
process.on('uncaughtException', (e) => { log('TOOL CRASH:', e?.stack ?? e); process.exit(4); });
process.on('unhandledRejection', (e) => { log('TOOL CRASH:', e?.stack ?? e); process.exit(4); });
const txs = [];           // { i, signer, nonce, bytes, hash, birth, firstSubmitMs, accepted, rejected, inBlock, ok, finalizedMs }
const byHash = new Map();
const blockSeenMs = new Map();      // block number → wall-clock ms when we first saw it finalized
let scanned = startBlock;           // last finalized block number scanned
async function scan() {             // scan newly finalized blocks; also records when each was seen
  const head = await finalizedHead();
  for (let n = scanned + 1; n <= head.number; n++) {
    const b = await blockAt(n);
    if (!blockSeenMs.has(n)) blockSeenMs.set(n, Date.now());
    for (const t of b.txs) {
      const x = byHash.get(t.hash);
      if (x && !x.inBlock) { x.inBlock = { number: n, hash: b.hash }; x.ok = t.ok; x.finalizedMs = blockSeenMs.get(n); }
    }
    scanned = n;
  }
  return head;
}
let scanning = true;
const scanLoop = (async () => { while (scanning) { try { await scan(); } catch (e) { log('scan error (tool):', e.message); } await sleep(1000); } })();

const VALIDITY = /\b(1010|1011|1012|1013|1014)\b|Invalid Transaction|Unknown Transaction|Priority is too low|Transaction is outdated|already imported|Temporarily banned/i;
const ALREADY = /already imported|Transaction is outdated|\bStale\b|Temporarily banned/i;   // our identical bytes are already known/included

// Submit identical bytes until the node accepts them (or rejects with a validity error).
async function submit(x) {
  for (let attempt = 0; ; attempt++) {
    if (attempt === 0) x.firstSubmitMs = Date.now(); else tool.resends++;
    try {
      await withTimeout(Promise.race([client._request('author_submitExtrinsic', [x.bytes]), dropped]), 15_000, 'submit');
      x.accepted = true; return;
    } catch (e) {
      const msg = String(e?.message ?? e) + ' ' + JSON.stringify(e?.data ?? '') + ' ' + (e?.code ?? '');
      if (VALIDITY.test(msg) && !/timeout/.test(msg)) {
        if (ALREADY.test(msg)) { x.accepted = true; log(`tx ${x.i} resend reported "${msg.trim()}" → already known`); return; }
        x.rejected = msg.trim(); return;
      }
      tool.sendErrors++;
      log(`tx ${x.i} send error (tool, will resend identical bytes): ${msg.trim()}`);
      await sleep(Math.min(5000, 500 * (attempt + 1)));
      if (x.inBlock) { x.accepted = true; return; }
    }
  }
}

async function signNext(a, i) {
  const call = api.tx.LiquidStaking.stake({ amount: minStake });
  for (let tries = 0; ; tries++) {
    try {
      const at = await finalizedHead();
      const bytes = hex(await withTimeout(call.create(a.signer, { nonce: a.nonce, mortality: { mortal: true, period: PERIOD, at } }), 30_000, 'sign'));
      return { bytes, birth: at.number };
    } catch (e) { tool.rpcRetries++; log(`tx ${i} sign retry (tool): ${e.message}`); await sleep(1000); }
  }
}

const loadStart = Date.now();
log(`load: ${TOTAL} × LiquidStaking.stake(${minStake}) at ${RATE}/s for ${DURATION}s`);
for (let i = 0; i < TOTAL; i++) {
  const due = loadStart + i * INTERVAL;
  if (due > Date.now()) await sleep(due - Date.now());
  const a = accts[i % ACCOUNTS];
  // per-account queue: a nonce is only signed after the previous nonce was accepted by the node (no gaps)
  a.queue = a.queue.then(async () => {
    if (a.halted) return;
    const { bytes, birth } = await signNext(a, i);
    const x = { i, signer: a.address, nonce: a.nonce, bytes, hash: txHash(bytes), birth, firstSubmitMs: null, accepted: false };
    a.nonce++;
    txs.push(x); byHash.set(x.hash, x);
    await submit(x);
    if (x.rejected) { a.halted = x.rejected; log(`tx ${i} REJECTED by node: ${x.rejected}; halting signer ${a.address}`); }
  });
}
await Promise.all(accts.map((a) => a.queue));
const loadEnd = Date.now();
log(`load done: ${txs.length} sent in ${((loadEnd - loadStart) / 1000).toFixed(1)}s`);

// ---------- reconcile ----------
const nonceOf = async (addr) => (await withTimeout(api.query.System.Account.getValue(addr, { at: 'finalized' }), 30_000, 'nonce')).nonce;
let head;
const graceEnd = Date.now() + GRACE_MS;
while (true) {
  head = await scan();
  const pending = txs.filter((x) => !x.inBlock && !x.rejected);
  if (!pending.length) break;
  // proven lost? signer's finalized nonce passed it, or mortality expired, and not in any finalized block
  let allProven = true;
  for (const x of pending) {
    const passed = (await nonceOf(x.signer)) > x.nonce, expired = head.number >= x.birth + PERIOD;
    if (!(passed || expired)) allProven = false;
  }
  if (allProven || Date.now() > graceEnd) break;
  await sleep(2000);
}
scanning = false; await scanLoop;
head = await scan();   // final pass over every finalized block up to head

const counts = { 'finalized-ok': 0, 'failed-in-block': 0, lost: 0, unresolved: 0, rejected: 0 };
const lines = [];
for (const x of txs) {
  let category, proof;
  if (x.inBlock) category = x.ok ? 'finalized-ok' : 'failed-in-block';
  else if (x.rejected) category = 'rejected';
  else {
    const fn = await nonceOf(x.signer), expired = head.number >= x.birth + PERIOD;
    if (fn > x.nonce || expired) { category = 'lost'; proof = { finalizedNonce: fn, mortalUntil: x.birth + PERIOD, finalizedHead: head.number, scannedFrom: startBlock + 1 }; }
    else category = 'unresolved';
  }
  counts[category]++;
  x.category = category;
  lines.push(JSON.stringify({
    hash: x.hash, category, signer: x.signer, nonce: x.nonce, firstSubmitMs: x.firstSubmitMs,
    finalizedMs: x.finalizedMs ?? null, latencyMs: x.finalizedMs ? x.finalizedMs - x.firstSubmitMs : null,
    block: x.inBlock?.number ?? null, blockHash: x.inBlock?.hash ?? null, mortalBirth: x.birth, mortalPeriod: PERIOD,
    ...(x.rejected ? { rejectReason: x.rejected } : {}), ...(proof ? { lostProof: proof } : {}),
  }));
}
fs.writeFileSync(path.join(OUT, 'txs.jsonl'), lines.join('\n') + (lines.length ? '\n' : ''));

const lat = txs.filter((x) => x.inBlock).map((x) => x.finalizedMs - x.firstSubmitMs).sort((a, b) => a - b);
const p95 = lat.length ? lat[Math.min(lat.length - 1, Math.ceil(0.95 * lat.length) - 1)] : null;
const inWindow = txs.filter((x) => x.firstSubmitMs != null && x.firstSubmitMs <= loadStart + DURATION * 1000 + 2000).length;
const delivered = inWindow >= Math.ceil(0.95 * TOTAL);
const sent = txs.length;
let verdict, code, reasons = [];
if ((p95 != null && p95 >= P95_LIMIT_MS) || counts.lost > 0) {
  verdict = 'FAIL'; code = 1;
  if (p95 != null && p95 >= P95_LIMIT_MS) reasons.push(`p95 ${p95} ms ≥ ${P95_LIMIT_MS} ms`);
  if (counts.lost) reasons.push(`${counts.lost} proven lost`);
} else if (!delivered || counts.unresolved > 0 || counts.rejected > 0) {
  verdict = "CAN'T TELL"; code = 4;
  if (!delivered) reasons.push(`load not delivered: ${inWindow}/${TOTAL} first-submitted within ${DURATION + 2}s (need ${Math.ceil(0.95 * TOTAL)})`);
  if (counts.unresolved) reasons.push(`${counts.unresolved} unresolved`);
  if (counts.rejected) reasons.push(`${counts.rejected} rejected at submit`);
} else { verdict = 'PASS'; code = 0; }

const summary = {
  chain: 'Energy Web X (Chopsticks fork)', endpoint: ENDPOINT, call: 'LiquidStaking.stake', amount: String(minStake),
  target: { rate: RATE, durationS: DURATION, total: TOTAL, accounts: ACCOUNTS },
  sent, ...counts, firstSubmittedInWindow: inWindow,
  deliveredRate: +(inWindow / DURATION).toFixed(4), sendSpanS: +((loadEnd - loadStart) / 1000).toFixed(1),
  latencyMs: { p50: lat.length ? lat[Math.floor(lat.length / 2)] : null, p95, max: lat.at(-1) ?? null, n: lat.length },
  blocks: { start: startBlock, finalizedHead: head.number },
  tool, verdict, exitCode: code, reasons,
};
fs.writeFileSync(path.join(OUT, 'summary.json'), JSON.stringify(summary, null, 2) + '\n');
log(`sent=${sent} finalized-ok=${counts['finalized-ok']} failed-in-block=${counts['failed-in-block']} lost=${counts.lost} unresolved=${counts.unresolved} rejected=${counts.rejected}`);
log(`delivered rate=${summary.deliveredRate}/s (${inWindow}/${TOTAL} in window), latency p50=${summary.latencyMs.p50}ms p95=${p95}ms max=${summary.latencyMs.max}ms`);
log(`tool-side (not chain problems): reconnects=${tool.reconnects} sendErrors=${tool.sendErrors} resends=${tool.resends} rpcRetries=${tool.rpcRetries}`);
log(`VERDICT: ${verdict}${reasons.length ? ' — ' + reasons.join('; ') : ''} (exit ${code})`);
log(`files: ${path.join(OUT, 'txs.jsonl')} ${path.join(OUT, 'summary.json')}`);
await new Promise((r) => logf.end(r));
client.destroy();
process.exit(code);
