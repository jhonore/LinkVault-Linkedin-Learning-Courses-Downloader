# Contributing to LinkedVault

Thanks for helping improve LinkedVault. This guide covers local setup, verification,
and pull request expectations. Architecture and agent/dev invariants live in the
linked docs below — do not invent APIs or duplicate those contracts here.

## Prerequisites

- **Node.js** with npm (Vite 8 expects a current Node; Node 20.19+ or 22.12+ is a
  safe baseline)
- **Rust** toolchain with MSRV **1.77.2** (see `apps/desktop/src-tauri/Cargo.toml`)
- **Microsoft C++ Build Tools** or Visual Studio Build Tools (Windows)
- **Microsoft WebView2 Runtime** (Windows)
- **Xcode Command Line Tools** (macOS only; `rusqlite` builds bundled SQLite and
  `webp` compiles C sources). macOS is development-only — see
  [README.md](README.md#run-locally-macos) for what does and does not work there.

## Run locally

The root `Makefile` is the entry point on every platform. `make help` lists every
target, and `make doctor` checks your toolchain. The root `package.json` forwards
to the same targets, so the `npm run ...` commands below remain valid.

```bash
make install
make dev        # or: make web-dev for a frontend-only Vite preview
```

The npm equivalents, from the repository root:

```powershell
npm --prefix apps\desktop install
npm run dev
```

`npm run dev` launches the Tauri desktop app. For a frontend-only Vite preview
(no native desktop commands):

```powershell
npm run web:dev
```

See the root [README.md](README.md) for PowerShell `npm.ps1` workarounds and
production build / release notes.

## Verify before you open a PR

Run what matches the surface you changed:

| Area | make | npm |
| --- | --- | --- |
| TypeScript / UI | `make verify-no-any`, `make build` | `npm run verify:no-any`, `npm run build` |
| Newspaper / responsive UI | `make verify-ui` | `npm run verify:ui` |
| Rust | `make cargo-clippy`, `make cargo-test` | `npm run cargo:clippy`, `npm run cargo:test` |
| Module layout / providers | `make verify-architecture` | `npm run verify:architecture` |
| Writer, migrations, clipping durability | `make verify-persistence` | `npm run verify:persistence` |

Scripts with no root passthrough are reachable with
`make npm SCRIPT=<name>` (for example `make npm SCRIPT=verify:newspaper-clippings`).

A failure in `verify:no-any` or `build` blocks completion for TypeScript work.

## Architecture and invariants

- **Architecture source of truth:** [docs/architecture/README.md](docs/architecture/README.md)
- **Frontend / Rust ownership:** [docs/architecture/frontend-rust-ownership-boundary.md](docs/architecture/frontend-rust-ownership-boundary.md)
- **Agent and developer invariants:** [AGENTS.md](AGENTS.md)

Do not add a fourth provider scheduler, cross-import providers, or move ADRs out
of `docs/architecture/`.

## Pull request expectations

- Do not commit secrets, cookies, tokens, or local `LinkVaultData` databases.
- Keep provider isolation: `linkedin`, `coursera`, and `newspaper` must not
  import each other’s internals.
- When you change a Tauri command name, argument, or payload, update the typed
  TypeScript adapter in the same change. Do not weaken types with `any`.
- Prefer focused diffs; leave unrelated dirty work and generated build trees alone.
- Follow the [Code of Conduct](CODE_OF_CONDUCT.md).

## Security reports

Do not open public issues for vulnerabilities. See [SECURITY.md](SECURITY.md).
