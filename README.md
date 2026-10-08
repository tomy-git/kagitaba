# kagitaba

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

```bash
cargo build --release
```

The binary is created at `target/release/kagitaba`.

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

### Signal handling

`kagitaba run` returns the child status code. On Unix signal termination, it returns `128 + signal` (shell-style).

## Code-signing and upgrades

Replacing or upgrading the binary can trigger new Keychain access prompts because Keychain access decisions may be tied to binary identity/signature and path.

For stable behavior in managed environments, use consistent binary signing and deployment practices.

## Development

```bash
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo test --all-targets
```

## License

This repository keeps the existing license unchanged.
