# Install and build

Polkameter is a Rust/Tauri application with a TypeScript frontend. The repository pins the pnpm version in `package.json`, Rust's minimum version in `src-tauri/Cargo.toml`, and the CI toolchain in `.github/workflows/ci.yml`.

## Develop from source

```sh
corepack pnpm install
corepack pnpm tauri dev
```

`tauri dev` starts Vite on `127.0.0.1:1420` and opens the desktop app. The Tauri configuration invokes `pnpm dev` before development and `pnpm build` before a production build.

To build only the frontend:

```sh
corepack pnpm build
```

To build either Rust executable:

```sh
cargo +1.93.0 build --manifest-path src-tauri/Cargo.toml --bin polkameter
cargo +1.93.0 build --manifest-path src-tauri/Cargo.toml --bin polkameter-desktop
```

The debug CLI is written to `src-tauri/target/debug/polkameter`. Release automation builds a release CLI alongside platform-specific desktop bundles.

## Verify a checkout

```sh
corepack pnpm test
corepack pnpm build
cargo +1.93.0 fmt --manifest-path src-tauri/Cargo.toml -- --check
cargo +1.93.0 test --manifest-path src-tauri/Cargo.toml
cargo +1.93.0 run --manifest-path src-tauri/Cargo.toml --bin polkameter -- --help
```

On Linux, Tauri requires GTK/WebKit and related libraries. The project CI installs `libgtk-3-dev`, `libwebkit2gtk-4.1-dev`, `libayatana-appindicator3-dev`, `librsvg2-dev`, and `patchelf`; install the equivalent packages for your distribution before building the desktop app.

## Build a distributable desktop app

```sh
corepack pnpm tauri build
```

The configured bundle formats are AppImage and Debian packages on Linux, DMG on macOS, and NSIS on Windows. See [Releases and maintenance](operations/releases.md) for the release workflow and provenance artifacts.
