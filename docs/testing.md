<!-- SPDX-License-Identifier: MPL-2.0 -->

# Testing and coverage

This is the 2026-10-09 measurement snapshot for Issue #4 on macOS arm64,
Rust 1.99.0, cargo-llvm-cov 0.9.1, Python 3.14.3, and coverage.py 7.16.2.
The normal Rust suite passed 56 tests (52 library tests and 4 binary tests),
with one isolated-Keychain test ignored. That explicit test separately passed
and checked that its temporary directory was removed. Python passed 21 tests.

| Measurement | Lines | Branches |
| --- | --- | --- |
| Rust production code, normal tests | 269/362 (74.31%) | Not measured on stable Rust |
| Rust production code, normal + isolated Keychain | 339/362 (93.65%) | Not measured on stable Rust |
| Python license checker, including CLI subprocesses | 85/85 (100%) | 26/26 (100%) |

Rust is measured from LCOV `DA` records under `src/`. Test files, test-support
files, the isolated-Keychain test file, and inline top-level `mod tests` blocks
are excluded by `scripts/summarize_rust_coverage.py`. Dependencies, examples,
build scripts, and binary integration test code are outside this production
denominator. These figures describe the macOS snapshot; Linux-only code and
tests have a different denominator. The refactored OS boundary also changes the
source denominator compared with the original Issue measurement.

Python measures only `scripts/check_license.py`. The coverage configuration
also instruments Python subprocesses and maps the unchanged test copies of the
checker back to the same source. It excludes tests and fake Git executables.

## Reproduce

Install measurement tools inside the ignored `.local/` directory:

```bash
mise exec -- cargo install cargo-llvm-cov --version 0.9.1 --locked --root .local/tools
uv venv .local/coverage
UV_CACHE_DIR="$PWD/.local/uv-cache" uv pip install --python .local/coverage/bin/python coverage==7.16.2
mkdir -p .local/rust-coverage .local/python-coverage
```

Measure the normal Rust suite first, then add the explicitly isolated macOS
Keychain test without cleaning the first run's profiles:

```bash
mise exec -- .local/tools/bin/cargo-llvm-cov llvm-cov --locked --all-targets --lcov --output-path .local/rust-coverage/normal.lcov
python3 -B scripts/summarize_rust_coverage.py .local/rust-coverage/normal.lcov
mise exec -- .local/tools/bin/cargo-llvm-cov llvm-cov --locked --no-clean --all-targets --lcov --output-path .local/rust-coverage/with-isolated-keychain.lcov -- --ignored --exact store::keychain::integration::isolated_keychain_round_trip --test-threads=1
python3 -B scripts/summarize_rust_coverage.py .local/rust-coverage/with-isolated-keychain.lcov
```

The second command that runs Rust tests creates and deletes a temporary macOS
Keychain. Skip it on Linux. Normal test commands never access real Keychains.

```bash
.local/coverage/bin/python -B -m coverage run --rcfile scripts/coverage.toml -m unittest discover -s scripts/tests -v
.local/coverage/bin/python -B -m coverage combine --rcfile scripts/coverage.toml
.local/coverage/bin/python -B -m coverage report --rcfile scripts/coverage.toml -m
```

There is no CI coverage threshold. A percentage does not prove every error path
or external interaction has been tested.

## Remaining paths

- `StdioPrompter`'s real stdin/stdout wiring and `rpassword`'s hidden TTY input
  are not driven by the normal suite. The shared confirmation implementation
  is tested for y/yes, rejection, EOF, and read/write/flush errors; App tests
  inject secret-input errors without logging values. Real terminal input needs
  an explicit interactive test with synthetic data.
- `NativeApi::open` deliberately selects the Login Keychain in production and
  is not executed by the isolated-Keychain test. The native methods receive an
  explicit disposable Keychain instead. OS-boundary fakes verify opening
  failures and all operation-specific error handling. No real access-denial,
  cancellation, or locked Login Keychain state is induced.
- The Unix `ExitStatus` fallback when neither exit code nor signal exists is
  not reachable with an ordinary terminated Unix child. Exit and signal paths
  are tested. Non-Unix fallback needs a suitable platform-specific test.
- One line of main's successful-command wiring is unexecuted by the CLI error
  tests; App-level success paths are tested with mocks. No success command is
  run against the real Login Keychain solely to raise coverage.

Temporary-Keychain cleanup runs during unwinding as well as success. Forced
process termination cannot run a destructor; an interrupted explicit test may
leave its dedicated `kagitaba-keychain-test-*` temporary directory and disposable
Keychain. Remove only that test Keychain with `security delete-keychain` and then
its dedicated directory. Cleanup failures are reported without synthetic values.
