# CI/CD integration

Use CI to validate scenarios early and to execute controlled load only in an explicitly approved environment. Do not use a broad branch trigger to load a shared or production network.

## Recommended pipeline shape

1. **Lint/build/test** the application and structurally validate committed scenarios.
2. **Preflight** the intended target with a protected, dedicated test account.
3. **Run** only from an approved manual, scheduled, or environment-gated job.
4. **Validate and upload** the complete artifact directory even if the run failed.

## Minimal GitHub Actions example

```yaml
name: Polkameter scenario

on:
  workflow_dispatch:
    inputs:
      scenario:
        description: Scenario path
        required: true
        default: scenarios/transfer.polkameter.xml

jobs:
  validate:
    runs-on: ubuntu-22.04
    steps:
      - uses: actions/checkout@v4
      - uses: actions/download-artifact@v4 # Replace with your released CLI download step.
      - name: Validate XML structure
        run: xmllint --noout --schema schemas/polkameter-plan-v1.xsd "${{ inputs.scenario }}"
      - run: polkameter validate "${{ inputs.scenario }}" --format json

  run:
    needs: validate
    runs-on: ubuntu-22.04
    environment: load-test # Protect this environment with reviewer approval and secrets.
    steps:
      - uses: actions/checkout@v4
      - name: Preflight and run
        env:
          POLKAMETER_SURI: ${{ secrets.POLKAMETER_SURI }}
        run: |
          polkameter preflight "${{ inputs.scenario }}" --signer-env POLKAMETER_SURI --format json
          polkameter run "${{ inputs.scenario }}" --signer-env POLKAMETER_SURI --output target/polkameter-runs --format json
      - name: Validate artifacts
        if: always()
        run: |
          for run in target/polkameter-runs/run-*; do
            [ -d "$run" ] && polkameter report "$run" --format json
          done
      - uses: actions/upload-artifact@v4
        if: always()
        with:
          name: polkameter-runs
          path: target/polkameter-runs
          if-no-files-found: ignore
```

The download step is intentionally left to your release distribution method. Pin the exact Polkameter version used by the run, and preserve its checksum or release attestation with the results.

## Remote-agent CI pattern

For a dedicated stress machine, keep the agent and signer there. The CI job stores only a bearer token, tunnels to the loopback agent, and runs with `--remote` and `--remote-token-env`. Do not pass `--signer-env` in this mode; the caller must not receive the agent secret.

## Repository CI

The repository's `.github/workflows/ci.yml` runs pnpm unit/build checks, Rust formatting/tests, CLI help/fixture validation/reporting, XSD validation of the XML fixture, a fresh native Zombienet CLI smoke test, and debug desktop builds on Linux, macOS, and Windows. The Zombienet job uploads retained artifacts for seven days. CodeQL, dependency review, and gitleaks run in separate workflows.

## Documentation deployment

This repository's `Documentation` workflow builds this mdBook on pull requests and pushes that change `book.toml`, `docs/`, or the workflow. On a push to `main`, it uploads the rendered `book/` directory and deploys it to GitHub Pages through the protected `github-pages` environment. The published path is `/polkameter/`.

GitHub Pages is configured to use **GitHub Actions**. After the first successful main-branch run, colleagues can read the site at [agustinustheodorus.com/polkameter](https://agustinustheodorus.com/polkameter/).
