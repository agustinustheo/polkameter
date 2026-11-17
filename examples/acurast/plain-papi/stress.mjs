// Acurast.register stress test (PAPI 3.3.0).
// usage: node stress.mjs <ws-endpoint> <output-dir>
// Exit: 0 PASS, 1 FAIL (chain missed a threshold), 4 CAN'T TELL, 3 setup failed before the load.
//
// Categories (they sum to `sent`):
//   finalized-ok     in a finalized block, ExtrinsicSuccess
//   failed-in-block  in a finalized block, ExtrinsicFailed
//   lost             in no finalized block (all finalized blocks since load start were scanned) AND either the
//                    signer's finalized nonce passed it or its mortality expired
//   rejected         (extra) the node refused the bytes at submit with a definitive validity error; the tx never
//                    entered the pool, so it is neither a chain loss nor in a block. Counts as CAN'T TELL.
//   unresolved       none of the above when the test stopped waiting
import fs from 'node:fs';
import path from 'node:path';
import { createClient, Enum, AccountId, getTypedCodecs } from 'polkadot-api';
import { getWsProvider } from 'polkadot-api/ws';
import { getTxCreator } from 'polkadot-api/tx-creator';
import { sr25519CreateDerive } from '@polkadot-labs/hdkd';
import { DEV_PHRASE, entropyToMiniSecret, mnemonicToEntropy } from '@polkadot-labs/hdkd-helpers';
import { blake2b } from '@noble/hashes/blake2.js';
import { acurast } from '@polkadot-api/descriptors';

const [WS, OUT] = process.argv.slice(2);
if (!WS || !OUT) { console.error('usage: node stress.mjs <ws-endpoint> <output-dir>'); process.exit(3); }
fs.mkdirSync(OUT, { recursive: true });

// ---- load parameters --------------------------------------------------------------------------------------------
const RATE = 0.25, DURATION_S = 200, N_ACCOUNTS = 4;
const TOTAL = Math.round(RATE * DURATION_S);            // 50
const INTERVAL_MS = 1000 / RATE;                         // 4000
const PERIOD = 64;                                       // mortality period (blocks)
const P95_LIMIT_MS = 60_000;
const DRAIN_MS = 180_000;                                // max wait for inclusion after the last first-submit
const FUND = 10n ** 20n;                                 // per test account (12 decimals → 1e8 ACU)

// ---- helpers ----------------------------------------------------------------------------------------------------
const logFile = fs.createWriteStream(path.join(OUT, 'run.log'));
const T0 = Date.now();
const log = (...a) => {
  const line = `[${((Date.now() - T0) / 1000).toFixed(1).padStart(6)}s] ` + a.map((x) => typeof x === 'string' ? x : JSON.stringify(x, big)).join(' ');
  console.log(line); logFile.write(line + '\n');
};
const big = (_k, v) => typeof v === 'bigint' ? v.toString() : v;
const hex = (u8) => '0x' + Buffer.from(u8).toString('hex');
const unhex = (h) => Uint8Array.from(Buffer.from(h.slice(2), 'hex'));
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const withTimeout = (p, ms, what) => Promise.race([p, sleep(ms).then(() => { throw Object.assign(new Error(`timeout: ${what}`), { transport: true }); })]);
const EVENTS_KEY = '0x26aa394eea5630e07c48ae0c9558cef780d41e5e16056765bc8461851072c9d7'; // System.Events

// ---- connection (PAPI's ws provider reconnects by itself; we log status changes) --------------------------------
let stopAll = false;
let wsStatus = 'connecting', reconnects = 0, everConnected = false;
const provider = getWsProvider(WS, {
  heartbeatTimeout: 20_000,
  onStatusChanged: (s) => {
    const name = ['CONNECTING', 'CONNECTED', 'ERROR', 'CLOSE'][s.type] ?? String(s.type);
    if (name === 'CONNECTED') { if (everConnected) reconnects++; everConnected = true; }
    if (name !== wsStatus) log(`ws ${name}`);
    wsStatus = name;
  },
});
const client = createClient(provider);
const api = client.getTypedApi(acurast);
const rpc = (method, params, ms = 30_000) => withTimeout(client._request(method, params), ms, method);

// ---- accounts ---------------------------------------------------------------------------------------------------
const derive = sr25519CreateDerive(entropyToMiniSecret(mnemonicToEntropy(DEV_PHRASE)));
const mkAccount = (p) => {
  const kp = derive(p);
  return { name: p, pub: kp.publicKey, address: AccountId().dec(kp.publicKey), signer: getTxCreator(kp.publicKey, 'Sr25519', kp.sign),
    nextNonce: null, due: [], unsent: [], dead: false, wake: null };
};
const seed = mkAccount('//Alice');
const accounts = Array.from({ length: N_ACCOUNTS }, (_, i) => mkAccount(`//Alice//stress//${i}`));

// ---- transaction book-keeping -----------------------------------------------------------------------------------
const txs = [];                 // every signed tx (funding + load)
const byHash = new Map();
const blocks = new Map();       // number → { hash, seenMs, prevPollMs }

async function finalizedNonce(acct) {
  const info = await withTimeout(api.query.System.Account.getValue(acct.address, { at: 'finalized' }), 30_000, 'nonce');
  return info.nonce;
}

// Sign one tx for `acct` at its next nonce against the current finalized block (mortal).
async function signNext(acct, mkCall, kind, d) {
  const fin = await withTimeout(client.getFinalizedBlock(), 30_000, 'finalized block');
  const nonce = acct.nextNonce;
  const bytes = await withTimeout(mkCall(d).create(acct.signer, {
    nonce, mortality: { mortal: true, period: PERIOD, at: { hash: fin.hash, number: fin.number } },
  }), 60_000, 'sign');
  // From here on these bytes are THE tx for this nonce; never sign anything else with it.
  acct.nextNonce++;
  const tx = { kind, signer: acct.address, account: acct.name, nonce, bytes: hex(bytes), hash: hex(blake2b(bytes, { dkLen: 32 })),
    birth: fin.number, birthHash: fin.hash, firstSubmitMs: null, ackMs: null, attempts: 0, rejected: null, block: null };
  txs.push(tx); byHash.set(tx.hash, tx); acct.unsent.push(tx);
  return tx;
}

// Submit identical bytes until the node acknowledges or definitively rejects them.
async function submit(tx) {
  for (;;) {
    if (tx.block) return 'acked';                          // seen in a block meanwhile
    tx.attempts++;
    if (tx.firstSubmitMs === null) tx.firstSubmitMs = Date.now();
    try {
      await rpc('author_submitExtrinsic', [tx.bytes], 20_000);
      tx.ackMs = Date.now();
      if (tx.kind === 'fund' || tx.attempts > 1) log(`submitted ${tx.kind} ${tx.account} n=${tx.nonce} ${tx.hash} (attempt ${tx.attempts})`);
      return 'acked';
    } catch (e) {
      // Pool verdicts come back as JSON-RPC errors 1010-1014 (Chopsticks: 1010 '{"invalid":{"stale":null}}').
      // Anything else (timeouts, disconnects, internal errors) is a transport problem: resend the same bytes.
      const msg = String(e?.message ?? e);
      const code = typeof e?.code === 'number' ? e.code : null;
      if (code === 1013) { tx.ackMs ??= Date.now(); return 'acked'; }                      // already imported
      if (code === 1010 && /stale/i.test(msg) && tx.attempts > 1) { tx.ackMs ??= Date.now(); return 'acked'; } // an earlier attempt landed
      if ((code === 1010 && !/future/i.test(msg)) || code === 1011 || code === 1014) {
        tx.rejected = `${code} ${msg}`.slice(0, 300); log(`REJECTED ${tx.account} n=${tx.nonce}: ${tx.rejected}`); return 'rejected';
      }
      if (stopAll) return 'gave-up';
      log(`submit retry ${tx.account} n=${tx.nonce} attempt ${tx.attempts}: ${msg.slice(0, 160)}`);
      await sleep(1000);
    }
  }
}

// Per-account worker: send queued txs strictly in nonce order; sign a new one only when nothing is outstanding.
async function worker(acct, mkCall, kind) {
  while (!acct.dead) {
    if (acct.unsent.length) {
      const r = await submit(acct.unsent[0]);
      if (r === 'gave-up') return;
      acct.unsent.shift();
      if (r === 'rejected') { acct.dead = true; reassign(acct); }
    } else if (acct.due.length) {
      try { await signNext(acct, mkCall, kind, acct.due[0]); acct.due.shift(); }
      catch (e) { log(`sign retry ${acct.name}: ${e.message}`); await sleep(1000); }
    } else if (acct.stop) return;
    else await new Promise((r) => { acct.wake = r; });
  }
}
const kick = (acct) => { const w = acct.wake; acct.wake = null; w?.(); };
function reassign(dead) {
  const live = accounts.filter((a) => !a.dead);
  if (!live.length) return;
  for (const d of dead.due.splice(0)) { const a = live[d % live.length]; a.due.push(d); kick(a); }
}

// ---- finalized-chain scanner ------------------------------------------------------------------------------------
let codecs, lastScanned = null, lastPollMs = null, scanning = true, scanErrors = 0;
async function scanOnce() {
  const head = await rpc('chain_getFinalizedHead', []);
  const hdr = await rpc('chain_getHeader', [head]);
  const headNum = parseInt(hdr.number, 16);
  const pollMs = Date.now();
  for (let n = lastScanned + 1; n <= headNum; n++) {
    const hash = n === headNum ? head : await rpc('chain_getBlockHash', [n]);
    const blk = await rpc('chain_getBlock', [hash]);
    const exts = blk.block.extrinsics;
    let events = null;
    for (let idx = 0; idx < exts.length; idx++) {
      const tx = byHash.get(hex(blake2b(unhex(exts[idx]), { dkLen: 32 })));
      if (!tx || tx.block) continue;
      if (!events) events = codecs.query.System.Events.value.dec(await rpc('state_getStorage', [EVENTS_KEY, hash]));
      const mine = events.filter((e) => e.phase.type === 'ApplyExtrinsic' && e.phase.value === idx);
      const failed = mine.find((e) => e.event.type === 'System' && e.event.value.type === 'ExtrinsicFailed');
      tx.block = { number: n, hash, idx, seenMs: pollMs, prevPollMs: lastPollMs ?? pollMs };
      tx.ok = !failed;
      tx.error = failed ? JSON.stringify(failed.event.value.value.dispatch_error, big) : undefined;
      if (tx.kind === 'fund') log(`fund tx n=${tx.nonce} in #${n} ok=${tx.ok}`);
      tx.events = mine.map((e) => `${e.event.type}.${e.event.value.type}`).filter((x) => !/^(TransactionPayment|Balances\.(Withdraw|Deposit)|Treasury)/.test(x));
    }
    blocks.set(n, { hash });
    lastScanned = n;
  }
  lastPollMs = pollMs;
}
async function scanner() {
  while (scanning) {
    try { await scanOnce(); } catch (e) { scanErrors++; if (scanErrors % 10 === 1) log(`scanner retry: ${e.message}`); await sleep(1000); continue; }
    await sleep(500);
  }
}

// ---- calls ------------------------------------------------------------------------------------------------------
let jobStart;
const script = new TextEncoder().encode('ipfs://QmUXhYmUwjgWtGpU8hXkCRzhZsUqpThFEzYBRDTDw7HBpX');
const registerCall = () => api.tx.Acurast.register({ registration: {
  script, allowed_sources: undefined, allow_only_verified_sources: false,
  schedule: { duration: 60_000n, start_time: jobStart, end_time: jobStart + 3_600_000n, interval: 600_000n, max_start_delay: 0n },
  memory: 0, network_requests: 0, storage: 0, required_modules: [],
  // PAPI flattens RegistrationExtra{requirements}: `extra` is the requirements object itself.
  extra: { assignment_strategy: Enum('Single', undefined), slots: 1, reward: 10n ** 9n, min_reputation: undefined, processor_version: undefined, runtime: Enum('NodeJS') },
} });

const waitFor = async (pred, ms) => { const end = Date.now() + ms; while (!pred()) { if (Date.now() > end) return false; await sleep(250); } return true; };

// ---- main -------------------------------------------------------------------------------------------------------
let phase = 'setup';
async function setup() {
  log(`connecting to ${WS}`);
  const fin = await withTimeout(client.getFinalizedBlock(), 300_000, 'initial finalized block');
  codecs = await getTypedCodecs(acurast);
  await withTimeout(api.compatibilityToken, 300_000, 'runtime metadata');
  lastScanned = fin.number;
  log(`connected: finalized #${fin.number} ${fin.hash}`);
  scanner();

  // Fund the test accounts from the seed (real transfers through the same signing/submit path).
  seed.nextNonce = await finalizedNonce(seed);
  const free = (await api.query.System.Account.getValue(seed.address, { at: 'finalized' })).data.free;
  log(`seed ${seed.address} nonce ${seed.nextNonce} free ${free}`);
  const transfer = (i) => api.tx.Balances.transfer_keep_alive({ dest: Enum('Id', accounts[i].address), value: FUND });
  seed.due = accounts.map((_, i) => i); seed.stop = true;
  await worker(seed, transfer, 'fund');
  const funding = txs.filter((t) => t.kind === 'fund');
  if (seed.dead || funding.length !== N_ACCOUNTS) throw new Error('funding transfer rejected by the node');
  if (!await waitFor(() => funding.every((t) => t.block), 600_000)) throw new Error('funding not finalized within 600 s');
  const bad = funding.filter((t) => !t.ok);
  if (bad.length) throw new Error(`funding failed in block: ${bad.map((t) => t.error).join('; ')}`);
  log(`funded ${N_ACCOUNTS} accounts in block(s) ${[...new Set(funding.map((t) => t.block.number))].join(',')}`);
  for (const a of accounts) a.nextNonce = await finalizedNonce(a);

  const now = await withTimeout(api.query.Timestamp.Now.getValue({ at: 'finalized' }), 30_000, 'Timestamp.Now');
  jobStart = BigInt(now) + 2n * 3_600_000n; // 2 h after chain time; chain time only moves as blocks are built
  log(`jobs start at chain time ${jobStart} (now ${now})`);
}

async function load() {
  phase = 'load';
  const workers = accounts.map((a) => worker(a, registerCall, 'load'));
  const loadStart = Date.now();
  log(`load: ${TOTAL} x Acurast.register at ${RATE} tx/s for ${DURATION_S} s from ${N_ACCOUNTS} accounts`);
  for (let i = 0; i < TOTAL; i++) {
    const at = loadStart + i * INTERVAL_MS;
    if (at > Date.now()) await sleep(at - Date.now());
    const live = accounts.filter((a) => !a.dead);
    if (!live.length) { log('all accounts dead; stopping load'); break; }
    const a = accounts[i % N_ACCOUNTS].dead ? live[i % live.length] : accounts[i % N_ACCOUNTS];
    a.due.push(i); kick(a);
    if (i % 10 === 9) log(`tick ${i + 1}/${TOTAL}: submitted ${txs.filter((t) => t.kind === 'load' && t.firstSubmitMs).length}, in block ${txs.filter((t) => t.kind === 'load' && t.block).length}`);
  }
  // Let catch-up finish (bounded), then stop the workers.
  await waitFor(() => accounts.every((a) => a.dead || (!a.due.length && !a.unsent.length)), 120_000);
  stopAll = true;
  for (const a of accounts) { a.stop = true; kick(a); }
  await Promise.race([Promise.all(workers), sleep(5_000)]);
  return loadStart;
}

async function reconcile(loadStart) {
  phase = 'reconcile';
  const load = txs.filter((t) => t.kind === 'load' && t.firstSubmitMs);
  const lastSubmit = Math.max(...load.map((t) => t.firstSubmitMs));
  log(`draining: waiting up to ${DRAIN_MS / 1000} s after the last submit for inclusion`);
  await waitFor(() => load.every((t) => t.block || t.rejected) || Date.now() > lastSubmit + DRAIN_MS, DRAIN_MS + 10_000);
  // Final scan up to the current finalized head (must succeed for any "lost" proof).
  let scanned = false;
  for (let i = 0; i < 20 && !scanned; i++) { try { await scanOnce(); scanned = true; } catch (e) { log(`final scan retry: ${e.message}`); await sleep(2000); } }
  scanning = false;
  const finNum = lastScanned;
  const nonces = new Map();
  for (const a of accounts) { try { nonces.set(a.address, await finalizedNonce(a)); } catch (e) { log(`nonce read failed ${a.name}: ${e.message}`); } }

  for (const t of load) {
    if (t.block) t.category = t.ok ? 'finalized-ok' : 'failed-in-block';
    else if (t.rejected) t.category = 'rejected';
    else if (scanned && nonces.has(t.signer) && nonces.get(t.signer) > t.nonce) { t.category = 'lost'; t.proof = `finalized nonce ${nonces.get(t.signer)} > ${t.nonce}, not in blocks up to #${finNum}`; }
    else if (scanned && finNum > t.birth + PERIOD) { t.category = 'lost'; t.proof = `mortality expired (born #${t.birth}, period ${PERIOD}, finalized #${finNum}), not in any block`; }
    else t.category = 'unresolved';
  }

  // ---- report -----------------------------------------------------------------------------------------------
  const jsonl = load.map((t) => JSON.stringify({
    hash: t.hash, category: t.category, signer: t.signer, nonce: t.nonce, firstSubmitMs: t.firstSubmitMs,
    account: t.account, attempts: t.attempts, ackMs: t.ackMs, signedAtBlock: t.birth,
    block: t.block?.number ?? null, blockHash: t.block?.hash ?? null, extrinsicIndex: t.block?.idx ?? null,
    latencyMs: t.block ? t.block.seenMs - t.firstSubmitMs : null, error: t.error ?? t.rejected ?? undefined, proof: t.proof, events: t.events,
  }));
  fs.writeFileSync(path.join(OUT, 'transactions.jsonl'), jsonl.join('\n') + '\n');

  const cats = { 'finalized-ok': 0, 'failed-in-block': 0, lost: 0, rejected: 0, unresolved: 0 };
  for (const t of load) cats[t.category]++;
  const windowEnd = loadStart + DURATION_S * 1000 + 2000;
  const inWindow = load.filter((t) => t.firstSubmitMs <= windowEnd).length;
  const required = Math.ceil(0.95 * TOTAL);
  const inBlock = load.filter((t) => t.block);
  const p95 = (xs) => { if (!xs.length) return null; const s = [...xs].sort((a, b) => a - b); return s[Math.ceil(0.95 * s.length) - 1]; };
  // Headline latency: first submit → first poll that saw the finalized block (upper bound of the true latency).
  const p95Ms = p95(inBlock.map((t) => t.block.seenMs - t.firstSubmitMs));
  // Chain-attributable lower bound: node acknowledged our bytes → last poll before the block was seen.
  // Excludes our own resend delays and scanner outages; FAIL on latency only if even this bound is over the limit.
  const p95LowMs = p95(inBlock.map((t) => Math.max(0, t.block.prevPollMs - (t.ackMs ?? t.firstSubmitMs))));

  const reasons = [];
  let verdict = 'PASS', code = 0;
  if (cats.lost > 0) reasons.push(`${cats.lost} transaction(s) proven lost`);
  if (p95LowMs !== null && p95LowMs >= P95_LIMIT_MS) reasons.push(`p95 latency ${(p95LowMs / 1000).toFixed(1)} s >= ${P95_LIMIT_MS / 1000} s`);
  if (reasons.length) { verdict = 'FAIL'; code = 1; }
  else {
    if (inWindow < required) reasons.push(`load not delivered: ${inWindow}/${TOTAL} first-submitted within ${DURATION_S}+2 s (need ${required})`);
    if (cats.unresolved) reasons.push(`${cats.unresolved} unresolved`);
    if (cats.rejected) reasons.push(`${cats.rejected} rejected at submit (test-side, not a chain verdict)`);
    if (p95Ms !== null && p95Ms >= P95_LIMIT_MS) reasons.push(`p95 ${(p95Ms / 1000).toFixed(1)} s over limit only when including test-side delays`);
    if (!inBlock.length) reasons.push('nothing included');
    if (reasons.length) { verdict = "CAN'T TELL"; code = 4; }
  }
  const summary = {
    chain: 'Acurast (Chopsticks fork)', endpoint: WS, call: 'Acurast.register', rate: RATE, durationS: DURATION_S, accounts: N_ACCOUNTS,
    target: TOTAL, sent: load.length, sentInWindow: inWindow, categories: cats,
    deliveredRateTxPerS: +(inWindow / DURATION_S).toFixed(4),
    includedRateTxPerS: +(inBlock.length / DURATION_S).toFixed(4),
    p95LatencyMs: p95Ms, p95LatencyChainLowerBoundMs: p95LowMs,
    maxLatencyMs: inBlock.length ? Math.max(...inBlock.map((t) => t.block.seenMs - t.firstSubmitMs)) : null,
    blocks: [...new Set(inBlock.map((t) => t.block.number))].length,
    testSide: { reconnects, resends: load.reduce((s, t) => s + t.attempts - 1, 0), scannerErrors: scanErrors },
    finalizedHead: finNum, verdict, exitCode: code, reasons,
  };
  fs.writeFileSync(path.join(OUT, 'summary.json'), JSON.stringify(summary, big, 2) + '\n');
  log('summary', summary);
  log(`VERDICT: ${verdict} (exit ${code})${reasons.length ? ' — ' + reasons.join('; ') : ''}`);
  return code;
}

let code;
try {
  await setup();
} catch (e) {
  log(`SETUP FAILED: ${e.stack ?? e}`);
  fs.writeFileSync(path.join(OUT, 'summary.json'), JSON.stringify({ verdict: 'SETUP FAILED', exitCode: 3, error: String(e.message ?? e) }) + '\n');
  code = 3;
}
if (code === undefined) {
  try { code = await reconcile(await load()); }
  catch (e) { log(`INTERNAL ERROR during ${phase}: ${e.stack ?? e}`); code = 4; }
}
logFile.end();
try { client.destroy(); } catch {}
setTimeout(() => process.exit(code), 200);
