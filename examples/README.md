# Examples: a stress test per chain, with calls mode and without

Each `examples/<chain>/` holds a stress test of one real Polkadot SDK chain, written by an AI agent with PolkaMeter
calls mode (`polkameter calls`). Next to it, `plain-papi/` holds the test another agent wrote for the same task with
plain [polkadot-api](https://papi.how) and no PolkaMeter. Both had to meet the same bar, and independent scorers
checked both on the chain. This is a dated record from November 2026, not maintained examples: live chains upgrade,
so these tests drift. Only `dev-chain/` runs in CI.

## The comparison

Lines are every file in the test's folder, counted by `count-lines.py` (code and config; no blank lines, comments,
lockfiles, `package.json` or generated types).

| Chain | Call | plain-papi | calls mode, prototype | calls mode, 0.5.0 |
|---|---|---|---|---|
| Energy Web X | `LiquidStaking.stake` | 255 | 29 | |
| Acala | `Dex.swap_with_exact_supply` | 379 | 68 | |
| Myriad | `Tipping.send_tip` | 361 | 66 | |
| Canggu | `Payroll.batch_pay` | 336 | 60 | |
| Mythos | `Nfts.mint` | 270 | 55 | |
| Acurast | `Acurast.register` | 293 | 80 | |
| Darwinia | `Assets.transfer_keep_alive` | 300 | 91 | |
| Polimec | `LinearRelease.vested_transfer` | 336 | 109 | |
| Aventus | `TokenManager.transfer` | 309 | 103 | |
| **Median** | | **319.5** | **74** | |
| DeBio | `GeneticData.add_genetic_data` | 330 | 261 | unsupported: old runtime |
| Hyperbridge | `HyperFungibleToken.send` | 365 | 118 | excluded: its fork stops after 3 blocks |

The prototype column is the pre-registered experiment; most of its larger counts are workarounds for prototype bugs
that 0.5.0 fixes. The 0.5.0 column re-measures calls mode on the release with fresh agents and the same scoring.

## How it was checked

- Same task for both: send the chain's call at a fixed rate from test accounts the test funds itself; sign against
  a finalized block; never skip or re-sign a nonce; resend identical bytes; reconcile every transaction against the
  finalized chain; report PASS, FAIL (chain-caused only) or CAN'T TELL.
- Each test was rerun by a separate scorer on a fresh chain, once clean and once with a hidden 8–14 s RPC outage,
  and every transaction was audited on chain. Every test in the table was correct in both reruns.
- Chains ran as Chopsticks copies (0.25 tx/s) except Canggu, which ran its own dev node (5 tx/s).

## What this does not show

- Both agents were told the integrity rules. This measures less code at the same correctness, not correctness for
  someone who doesn't know the rules.
- Calls-mode agents took longer to finish on most chains (median about 1.5×), mostly setup time.
- After an RPC outage, calls mode does not resend the load it missed, so it ends CAN'T TELL where the plain tests
  caught up and passed.
- All tests were written by one AI model, one test per chain per side, at low rates on local copies of the chains.
- Calls mode can't express transactions authorised by a proof instead of a signature (for example the People chain).

## Run one

```sh
cd examples/<chain>
../fork.sh <chain> 9944                                       # acala, acurast, aventus, darwinia, ewx, mythos
npm install && npx papi add <chain> -w ws://127.0.0.1:9944   # generate the chain's types
./run.sh ws://127.0.0.1:9944 out                                  # 0 PASS, 1 FAIL, 4 CAN'T TELL, 3 setup failed
```
