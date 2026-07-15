# JMeter interchange

Polkameter borrows JMeter's test-plan vocabulary and can exchange a structural `.jmx` companion file, but it is not a general JMeter execution engine.

## Export

The desktop's **Export JMX** command writes a JMeter document containing the test-plan name, thread groups, user count, ramp time, loop count, and a JTL collector. A ramp arrival becomes `ThreadGroup.ramp_time` rounded up to seconds; Burst and Poisson have no equivalent ramp time and export as zero.

The `.polkameter.json` scenario remains authoritative because a JMX file cannot carry the target pallet, call, dynamic SCALE arguments, completion boundary, signer policy, or Polkameter-specific scheduling semantics.

## Inspect

The desktop's **Inspect JMX** command parses a JMX file and reports discovered thread groups, user counts, ramp seconds, loop counts, collectors, and diagnostics. It does not execute imported JMeter samplers.

Unsupported or non-Substrate structures—including HTTP, Java, JSR223, generic, and loop controllers—are diagnosed as structural context that should be preserved beside a Polkameter scenario. This keeps importing safe: opening a `.jmx` never runs arbitrary JMeter behavior.

## Migration pattern

1. Keep the original `.jmx` as documentation of a legacy plan.
2. Inspect it in Polkameter to recover group shape and collectors.
3. Create the `.polkameter.json` equivalent, including explicit pallet/call/arguments and signer policy.
4. Preflight it against the actual chain before running.
