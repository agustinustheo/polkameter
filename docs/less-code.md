# Less code, same correctness

A load test that sends transactions correctly needs code that has nothing to do with your chain: funding accounts, signing against a finalized block, keeping nonces in order, resending after a dropped connection, and checking every transaction on the finalized chain. Calls mode does that part, so your test says only what to send.

To measure the difference, AI agents wrote a stress test for nine real chains twice, to the same bar: once with plain [polkadot-api](https://papi.how) and once with calls mode. Independent scorers reran every test on the chain and audited every transaction. With calls mode, the median test was 43 lines instead of 309, and every test was still correct.

{{#include ../examples/README.md:comparison}}
Each test, with its call file and its plain polkadot-api counterpart, is in [examples/](https://github.com/agustinustheo/polkameter/tree/main/examples). Its README says how to run one.
