#!/usr/bin/env python3
"""Compare a `cargo test --no-fail-fast` log with the expected-failures list.

    python tools/ci/compare_tests.py test-output.txt .github/windows-ci/expected-failures.txt

Exits 1 when a test outside the list failed or no test ran (e.g. the build broke). Listed tests
that now pass are reported so the list can shrink. Writes a summary to $GITHUB_STEP_SUMMARY."""
import os
import re
import sys

ANSI = re.compile(r"\x1b\[[0-9;]*m")


def parse(log):
    failed, passed, results = set(), set(), 0
    for raw in log.splitlines():
        line = ANSI.sub("", raw).rstrip("\r")
        m = re.match(r"^test (.+) \.\.\. (ok|FAILED|ignored)$", line)
        if m:
            (failed if m.group(2) == "FAILED" else passed).add(m.group(1))
        elif line.startswith("test result:"):
            results += 1
    return failed, passed, results


def expected(path):
    with open(path, encoding="utf-8") as f:
        return {l.strip() for l in f if l.strip() and not l.lstrip().startswith("#")}


def main():
    log_path, list_path = sys.argv[1:3]
    with open(log_path, encoding="utf-8", errors="replace") as f:
        failed, passed, results = parse(f.read())
    allowed = expected(list_path)
    unexpected = sorted(failed - allowed)
    fixed = sorted(allowed & passed)
    lines = [
        "## Full test suite on Windows",
        f"{len(passed)} passed, {len(failed)} failed ({len(failed & allowed)} expected: data not in the repo).",
    ]
    if results == 0:
        lines.append("\n**No test results: the test build failed.**")
    if unexpected:
        lines += ["", "### Unexpected failures", "```", *unexpected, "```"]
    if fixed:
        lines += ["", "### Listed as expected but now passing (remove from expected-failures.txt)", "```", *fixed, "```"]
    text = "\n".join(lines) + "\n"
    print(text)
    summary = os.environ.get("GITHUB_STEP_SUMMARY")
    if summary:
        with open(summary, "a", encoding="utf-8") as f:
            f.write(text)
    return 1 if unexpected or results == 0 else 0


if __name__ == "__main__":
    sys.exit(main())
