# Contributing

## Development loop

Install dependencies, then run the frontend and Rust checks from [Install and build](install.md). Keep changes scoped, format Rust with the pinned toolchain, and add unit coverage for logic changes. Use the local chain or Zombienet smoke test when changing preflight, signing, execution, artifacts, or remote behavior.

## Documentation contributions

Documentation is mdBook source under `docs/`, configured by `book.toml`. Add a page to `docs/SUMMARY.md`, use relative links, and build before submitting:

```sh
mdbook build
```

The documentation GitHub Actions workflow repeats that build for pull requests. The generated `book/` directory is disposable and is ignored by Git; do not commit it.

## Scenario and security review

Never commit SURI material, API tokens, or real production endpoints in example files. Scenarios should retain `[redacted]` signer sources. If a change broadens signing, remote access, validation, or artifact behavior, document the boundary and exercise it against a disposable chain before review.
