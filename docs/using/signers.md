# Signers and security

Scenarios and artifacts are designed to be shareable. Secrets are intentionally outside that contract.

## Secret-handling rules

- Saved scenarios contain a profile alias and a redacted `baseSuri`, never an actual SURI.
- The loader rejects scenario files that contain a non-redacted SURI.
- Artifact validation rejects files with unredacted signer material, including familiar development SURI values.
- The desktop stores a profile in the operating-system credential vault and resolves it only just before preflight or execution.
- The CLI accepts `--signer-profile <name>` or `--signer-env <VARIABLE>`. CI should normally use `--signer-env` with its protected secret store.
- A remote client sends a redacted scenario. The remote agent resolves its own profile or `POLKAMETER_AGENT_SURI` locally.

## Virtual-user derivation

Polkameter derives an account for each virtual user using the configured derivation path, the run ID, and the user index. With the default path, the derivation root has the form:

```text
//polkameter//run-<run-id>
```

The same run ID produces the same derived accounts; different run IDs produce different ranges. Groups do not share user indexes. This avoids a single signer nonce becoming the bottleneck during a multi-user test.

## Readiness and funding

Before submitting, the runner checks that derived accounts are ready. On a local dev chain, the optional `funding` helper can fund them through bounded `Utility.batch_all` calls. This helper is intentionally constrained:

- The RPC must be loopback `ws://127.0.0.1`, `ws://localhost`, or `ws://[::1]`.
- The resolved signer must be a development SURI beginning with `//`.
- `amount` must be a positive decimal balance.
- `batchSize` must be 1–100 and funding finality timeout at least one second.

It cannot be used to fund a remote chain. Fund real test accounts through your controlled operational process instead.

## Recommended practice

Use a dedicated, least-privileged test account; keep its funding bounded; rotate CI secrets; and restrict access to stress hosts. Review the exact scenario and target endpoint before each production-adjacent test. A Polkameter run sends real extrinsics and may incur fees or degrade a shared network.
