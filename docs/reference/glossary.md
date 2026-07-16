# Glossary

- **Artifact bundle:** Redacted directory written per run, containing raw samples, logs, summary, and plots.
- **Arrival model:** The deterministic policy that schedules virtual-user starts: burst, ramp, or Poisson.
- **Completion boundary:** Point at which a sample is considered complete: submitted, in block, or finalized.
- **Derived signer:** A per-virtual-user account deterministically derived from the base signer, run ID, and index.
- **Preflight:** Read-only live validation of target metadata, dynamic call encoding, and signer readiness.
- **Sampler:** One configured pallet call, including arguments, phase, timeout, completion boundary, and assertions.
- **Signer profile:** A non-secret alias whose SURI is held by the local OS credential vault.
- **Thread group:** A workload group with users, concurrency, iterations, arrival model, and ordered samplers.
- **Virtual user:** One derived account executing a transaction sampler schedule.
