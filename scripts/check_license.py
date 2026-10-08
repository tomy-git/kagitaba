# SPDX-License-Identifier: MPL-2.0
"""Check the project's SPDX headers and MPL-2.0 license metadata without dependencies."""

import hashlib
import os
from pathlib import Path
import stat
import subprocess
import sys
import tomllib

IDENTIFIER = "SPDX-License-Identifier: MPL-2.0"
HEADERS = {
    ".rs": f"// {IDENTIFIER}",
    ".py": f"# {IDENTIFIER}",
    ".sh": f"# {IDENTIFIER}",
    ".toml": f"# {IDENTIFIER}",
    ".yml": f"# {IDENTIFIER}",
    ".yaml": f"# {IDENTIFIER}",
    ".md": f"<!-- {IDENTIFIER} -->",
    ".txt": IDENTIFIER,
}
SPECIAL_HEADERS = {
    ".gitignore": f"# {IDENTIFIER}",
    "Cargo.lock": f"# {IDENTIFIER}",
    "LICENSE": IDENTIFIER,
}
# Seal the existing MPL-2.0 text, verified against https://spdx.org/licenses/MPL-2.0.txt.
# Whitespace changes are permitted; the SPDX metadata line is not part of the digest.
LICENSE_DIGEST = "e8ba82e63ba908724aaee6043943c5a2629b9ebf1af581ea0eea19a713123685"


def regular_file(root: Path, path: Path) -> bool:
    try:
        relative = path.relative_to(root)
        if ".." in relative.parts or any((root / parent).is_symlink() for parent in relative.parents):
            return False
        return stat.S_ISREG(path.lstat().st_mode)
    except (OSError, ValueError):
        return False


def check_header(root: Path, name: str) -> str | None:
    path = root / name
    expected = SPECIAL_HEADERS.get(path.name, HEADERS.get(path.suffix))
    if not regular_file(root, path):
        return f"{name!r}: missing file, symlink, or unsupported non-regular file"
    if expected is None:
        return f"{name!r}: unsupported file format; add a reviewed SPDX header format"
    try:
        with path.open("rb") as source:
            first = source.readline(4096)
            if len(first) == 4096 and not first.endswith(b"\n"):
                return f"{name!r}: header or shebang line exceeds the 4096-byte limit"
            if path.suffix in {".py", ".sh"} and first.startswith(b"#!"):
                first = source.readline(4096)
        actual = first.rstrip(b"\r\n").decode("utf-8")
    except (OSError, UnicodeError):
        return f"{name!r}: cannot read a UTF-8 SPDX header"
    if actual != expected:
        return f"{name!r}: first line must be {expected!r} (immediately after a script shebang, if present)"
    return None


def check_metadata(root: Path) -> list[str]:
    errors = []
    manifest = root / "Cargo.toml"
    if regular_file(root, manifest):
        try:
            with manifest.open("rb") as source:
                package = tomllib.load(source).get("package")
            license_id = package.get("license") if isinstance(package, dict) else None
            if license_id != "MPL-2.0":
                errors.append("Cargo.toml: package.license must be MPL-2.0")
        except (OSError, ValueError):
            errors.append("Cargo.toml: cannot parse license metadata")
    else:
        errors.append("Cargo.toml: a regular manifest file is required")

    license_file = root / "LICENSE"
    if regular_file(root, license_file):
        try:
            text = license_file.read_text(encoding="utf-8")
            body = text.partition("\n")[2]
            digest = hashlib.sha256(" ".join(body.split()).encode("utf-8")).hexdigest()
            if digest != LICENSE_DIGEST:
                errors.append("LICENSE: MPL-2.0 license text does not match the verified copy")
        except (OSError, UnicodeError):
            errors.append("LICENSE: cannot read license text")
    else:
        errors.append("LICENSE: a regular license file is required")
    return errors


def check_repository(root: Path) -> tuple[int, list[str]]:
    result = subprocess.run(
        ["git", "-C", str(root), "ls-files", "--cached", "--others", "--exclude-standard", "-z"],
        check=True,
        capture_output=True,
    )
    names = sorted({os.fsdecode(name) for name in result.stdout.split(b"\0") if name})
    errors = [error for name in names if (error := check_header(root, name)) is not None]
    errors.extend(check_metadata(root))
    return len(names), errors


def main() -> int:
    root = Path(__file__).resolve().parents[1]
    try:
        count, errors = check_repository(root)
    except (OSError, subprocess.CalledProcessError):
        print("ERROR: cannot enumerate repository files with git", file=sys.stderr)
        return 1
    for error in errors:
        print(f"ERROR: {error}", file=sys.stderr)
    if errors:
        return 1
    print(f"PASS: {count} files have MPL-2.0 SPDX headers; Cargo metadata and LICENSE agree")
    return 0


if __name__ == "__main__":
    sys.exit(main())
