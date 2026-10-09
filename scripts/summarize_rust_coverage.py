# SPDX-License-Identifier: MPL-2.0
"""Summarize LCOV line coverage for production src/, excluding test modules."""

from pathlib import Path
import re
import sys

root = Path(__file__).resolve().parents[1]
results = {}
path = None
excluded = set()
for line in Path(sys.argv[1]).read_text(encoding="utf-8").splitlines():
    if line.startswith("SF:"):
        candidate = Path(line[3:]).resolve()
        path = None
        excluded = set()
        try:
            relative = candidate.relative_to(root)
        except ValueError:
            continue
        if relative.parts[0] != "src" or candidate.name in {
            "tests.rs", "test_support.rs", "integration.rs"
        }:
            continue
        in_tests = False
        for number, source in enumerate(candidate.read_text(encoding="utf-8").splitlines(), 1):
            if re.fullmatch(r"mod tests \{", source):
                in_tests = True
            if in_tests:
                excluded.add(number)
                if source == "}":
                    in_tests = False
        path = str(relative)
        results.setdefault(path, {})
    elif line.startswith("DA:") and path is not None:
        number, count, *_ = line[3:].split(",")
        if int(number) not in excluded:
            results[path][int(number)] = results[path].get(int(number), 0) + int(count)

covered = total = 0
for path, lines in sorted(results.items()):
    hits = sum(count > 0 for count in lines.values())
    covered += hits
    total += len(lines)
    missing = ",".join(str(number) for number, count in sorted(lines.items()) if not count)
    print(f"{path}: {hits}/{len(lines)} lines; missing: {missing or 'none'}")
if not total:
    raise SystemExit("No production coverage found")
print(f"TOTAL: {covered}/{total} ({covered / total:.2%})")
