# kagitaba

[English](README.md) | [日本語](README.ja.md)

`kagitaba` is an **experimental**, local-first API key manager CLI for macOS.

It stores API keys in the local macOS **Login Keychain**, then injects only selected keys into commands you explicitly launch.

> ⚠️ Security notice
>
> - This project is experimental and has **not** been independently security-audited.
> - Commands launched via `kagitaba run` (and their child processes) can read the injected keys.
> - `kagitaba` does not isolate secrets from malicious programs running as the same user.
> - Child programs may still leak secrets through their own logs/output/crash reports.

## Features in scope

- `kagitaba set <ENV_NAME>`
  - Secure prompt (no echo) for secret input
  - No secret argument accepted on the command line
  - Confirmation before replacing existing entries
  - A name registered concurrently during input also requires replacement confirmation
- `kagitaba status [ENV_NAME]`
  - Shows registration status / key names only
  - Never prints secret values
- `kagitaba run --key <ENV_NAME> [--key <ENV_NAME>...] -- <PROGRAM> [ARGS...]`
  - Loads only selected keys
  - Executes child process directly (no shell command-string evaluation)
  - Preserves child exit code
- `kagitaba delete <ENV_NAME>`
  - Confirmation before deletion
  - Deletes only kagitaba-owned entries

## Installation

With [mise](https://mise.jdx.dev/) installed:

```bash
mise trust mise.toml
mise install
mise exec -- cargo build --locked --release
```

The binary is created at `target/release/kagitaba`.

`mise.toml` pins Rust and installs Cargo, rustfmt, and Clippy. Rustup and Cargo
store their toolchains and dependency caches in this checkout's ignored `.local/`
directory. Shell startup files and global Rust defaults are not changed. Mise
also keeps its own install tracking, trust records, and caches outside the checkout.
Use `mise exec --` for development commands so the project-specific paths apply.
An existing compatible Rust installation can also build this project directly.
Mise's shared Rust install tracking links to this checkout. After moving the
checkout, or when using another checkout with the same Rust version, run
`mise install` there again. Each checkout retains its own Cargo and Rustup files.

## Usage

Set a key:

```bash
kagitaba set OPENAI_API_KEY
```

Check one key:

```bash
kagitaba status OPENAI_API_KEY
```

List registered key names:

```bash
kagitaba status
```

Run a command with selected keys:

```bash
kagitaba run --key OPENAI_API_KEY --key ANTHROPIC_API_KEY -- curl https://example.com
```

Delete a key:

```bash
kagitaba delete OPENAI_API_KEY
```

## Architecture

The code is split so each layer can be tested independently:

- `src/cli.rs`: CLI parsing and command structure
- `src/store.rs`: credential-store interface and platform implementations
  - macOS implementation uses `security-framework` bindings with a dedicated service namespace
  - non-macOS builds use a non-functional placeholder backend so tests can run with mocks
- `src/process.rs`: child-process execution (program + args + scoped env injection)
- `src/app.rs`: command orchestration, prompts, validation, and user-facing behavior

## Keychain and platform behavior

- Storage targets the local Login Keychain and a dedicated `kagitaba` service namespace.
- `kagitaba` does not change keychain lock policy, automatic locking, or lock-on-sleep settings.
- Expected failure cases (missing key, denied access, locked keychain, process launch failure) are returned as errors without exposing secret values.
- Invalid command-line arguments are rejected without echoing their values. Help and version output remain available.
- New entries are added without replacing existing items. Confirmed replacement updates an existing item only; if it disappears, the operation fails instead of recreating it.
- Key names are queried from Keychain item attributes, without reading secret values or maintaining a separate index. The legacy index item is ignored.
- Child commands inherit the usual parent environment, with selected keys added or replaced. Existing environment variables are not filtered.
- Secret buffers owned by the application are zeroized on drop. Rust's process API and the operating system may retain additional copies; complete memory erasure is not guaranteed.

### Signal handling

`kagitaba run` returns the child status code. On Unix signal termination, it returns `128 + signal` (shell-style).

## Code-signing and upgrades

Replacing or upgrading the binary can trigger new Keychain access prompts because Keychain access decisions may be tied to binary identity/signature and path.

For stable behavior in managed environments, use consistent binary signing and deployment practices.

## Development

```bash
mise exec -- cargo fmt --all -- --check
mise exec -- cargo clippy --locked --all-targets -- -D warnings
mise exec -- cargo test --locked --all-targets
```

Default tests use synthetic credentials and mocks and never access the Login Keychain.
They cover confirmation and failure paths with recording fakes, Keychain query
construction and name extraction, and binary-level argument-error output.

## License

This repository keeps the existing license unchanged.
