<!-- SPDX-License-Identifier: MPL-2.0 -->

# History storage contract

The English and Japanese README describe user commands. This document defines
storage and recovery behavior for the SQLite history introduced in Issue #8.
History management never reads or writes credential values. The `History` trait
accepts only `Record`/`Event` with validated metadata, and is independent of App,
Keychain and future TUI consumers. There is no arbitrary message/debug logger.

## Files and security boundary

`HistoryStore::from_home` selects the fixed platform directory without I/O.
Recording with an absent directory or disabled configuration creates no files.
Configuration updates create the directory, `config` and `history.lock`;
the first enabled record creates `history.sqlite3`. Settings are separate from
the DB so disabling does not require opening or repairing corrupt SQLite content.

Ancestors are opened with descriptor-relative `openat` and `O_NOFOLLOW`. They
must belong to root or the effective user and must not be group/world writable,
except a root-owned sticky directory such as the OS temporary directory. The
final directory must be owned by the effective user with exactly 0700. Every
existing child, including unknown backup and temporary files, must be a regular
file owned by that user, exactly 0600, with one hard link. Unsafe existing paths
are rejected without chmod, chown, deletion or traversal. Symbolic ancestors
such as macOS `/var` must be replaced with their actual path by test fixtures.
Production does not silently canonicalize unsafe user paths.

New files are created descriptor-relatively with 0600 and exclusive creation
where required. SQLite opens the precreated DB read/write, with `NOFOLLOW`,
without `CREATE` or URI interpretation; its inode is checked before/after use.
The bundled Unix SQLite VFS uses no-follow opens and inherits DB permissions for
journal/WAL/SHM. The normal journal mode is `DELETE`; related files are checked
before and after operations. SQLite temporary SQL storage uses memory. Config
updates use a random exclusive 0600 temporary file, file fsync, atomic rename
and directory fsync. The shared lock file is never replaced.

These checks protect against unsafe preexisting files and cooperating concurrent
CLI processes. They do not isolate a malicious process with the same UID, prevent
all same-user rename races, or make this a tamper-resistant audit system. No
custom SQLite VFS or automatic backup is supplied. Protect manually copied files
and backups with the same ownership and permissions. Key names and usage times
are sensitive metadata even though secret values are excluded.

## Schema and transactions

Schema version 1 is stored in `PRAGMA user_version`:

- `events`: operation ID, UTC Unix milliseconds, fixed kind/operation/outcome/error
  classification, optional sanitized program basename, exit code or signal.
- `event_keys`: validated key names linked by foreign key to an event.
- `events_time`: time/id index; each operation ID/kind is unique.

An empty version-0 DB is initialized to version 1 in an immediate transaction.
Version-1 objects must match the expected table and index definitions. Unknown
versions, foreign objects, extra triggers/views and corruption are rejected;
no automatic reset, table drop or database replacement occurs. Future migrations
must explicitly recognize a prior version, preserve its event/key rows and
update the version in the same transaction. This initial release has no older
application schema to migrate. Tests cover initialization, reopen persistence,
foreign/current-extra/future schemas and rollback/preservation.

Values are bound SQL parameters. Each event and its key rows are committed
together with retention pruning. Listing also prunes in an immediate transaction
before applying key/failure filters and the operation-group limit. A correlated
run's two rows therefore cannot be split by a query limit or pruning. Age is
based on a group's newest timestamp. A start pruned during a long-running child
can leave a later end as `end-only`; this does not reconstruct an absent start.

Each connection uses full synchronous writes, foreign keys, disabled trusted
schema and a 250 ms SQLite busy timeout. A shared file lock serializes config
read-modify-write and cooperating DB operations with a separate 250 ms bound,
using nonblocking attempts every 5 ms. A call can wait at both lock layers;
these limits bound contention waits, not filesystem or `VACUUM` execution time.

## Failure and recovery

| Condition | Behavior |
| --- | --- |
| File/SQLite lock contention | Bounded wait, fixed failure; no retry loop in App |
| Disk full or write/init error | Transaction rollback where possible; existing history is not reset |
| Unsafe ownership, mode, links or file type | Reject without repair or following links |
| Corrupt/foreign/future DB | Preserve file, reject DB operation; safe configuration can still disable recording |
| Missing/invalid HOME or configuration | Fixed error; no fallback to a public or current directory |
| Interrupted process | SQLite recovers transactional writes; orphan run starts remain unknown; incomplete config temp files may remain private |
| `clear` deletion commits but `VACUUM` fails | Explicit “history was cleared, but storage reclamation failed”; retry `reclaim` |

App recording errors produce only a fixed warning, including RNG/ID generation
failures. They do not undo a Keychain success, turn its normal success display
into a registration failure, or change a child exit status. A later stdout error
remains an output error; the already recorded credential-operation result is
unchanged. Management commands return fixed history errors because storage is
their requested operation. Raw OS/SQLite diagnostics are never emitted by the
history layer. Original credential/TTY error behavior is outside history logging.

A start is written only from the successful-spawn callback, after selected keys
have been configured on the child. No start is written for credential-read or
spawn failure. The end distinguishes exit code, signal and unknown termination;
a wait failure remains unknown. Crashing between a Keychain success/spawn and a
history write, failed writes, or disabling recording while a child runs can leave
gaps. History cannot prove authentication or API use.

Retention and confirmed clear are logical deletion. Clear then runs `VACUUM`;
`reclaim` can also run independently. These operations cannot erase backup copies,
filesystem snapshots or recoverable storage media. Do not automatically delete a
corrupt DB as a recovery step; first preserve a private copy if its data matters.
Any deliberate replacement or deletion of corrupt history is a separate manual
operation. Keychain entries remain independent.

## Validation

Normal tests use only fake credentials and private temporary DBs. They exercise
mutation success/failure/abort, successful/failed spawn, correlated termination,
unknown/end-only, fixed diagnostics, bounded locks, concurrent connections,
transactional disk-full failure, schemas, retention and clear/reclaim. Public
storage tests independently verify permissions, link rejection and preservation.
Foreign-owner policy is tested with a metadata double, without privileged chown.
A live private SQLite journal is checked for 0600. Unsafe WAL/SHM files are
rejected before opening SQLite, even though normal writes use DELETE journals.

An App test launches a synthetic child with dummy credentials, arguments,
environment and output sent to separate temporary files, then scans DB/config
and remaining related files, history output and warnings for sentinel absence.
Binary CLI tests use an isolated HOME, check no history files before opt-in,
exercise management persistence and ensure no Login Keychain is opened. No
normal test accesses the real Login Keychain. Fmt, Clippy, Rust tests, Python
license-checker tests, SPDX checks and independent general/security reviews are
required before merge; Linux CI additionally checks the portable backend.

The earlier Issue #4 coverage figures in [testing.md](testing.md) remain a dated
snapshot and are not coverage measurements of this new history implementation.
