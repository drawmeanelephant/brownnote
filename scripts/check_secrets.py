#!/usr/bin/env python3
"""High-signal credential scan for tracked files.

Looks for the shapes that actually leak: cloud keys, GitHub tokens, private
keys, and Slack webhooks.  Placeholders in `.env.example` and documentation
are fine; a real value in the tree is not.

Also refuses a tracked `.env*` file other than the example.

Usage:
  scripts/check_secrets.py            # tracked files
  scripts/check_secrets.py --staged   # staged files (pre-commit)
"""

from __future__ import annotations

import re
import subprocess
import sys

PATTERNS = {
    "AWS access key": re.compile(r"AKIA[0-9A-Z]{16}"),
    "GitHub token": re.compile(r"\b(gh[pousr]_[A-Za-z0-9]{30,}|github_pat_[A-Za-z0-9_]{20,})"),
    "OpenAI-style key": re.compile(r"\bsk-[A-Za-z0-9]{32,}"),
    "Slack token": re.compile(r"\bxox[abprs]-[A-Za-z0-9-]{10,}"),
    "private key block": re.compile(r"-----BEGIN (RSA |OPENSSH |EC |DSA )?PRIVATE KEY-----"),
    "generic bearer": re.compile(r"Authorization:\s*Bearer\s+[A-Za-z0-9._-]{20,}"),
}

ALLOW = {
    ".env.example",
    "docs/studio-security.md",
    "SECURITY.md",
}


def files(staged: bool) -> list[str]:
    cmd = (["git", "diff", "--cached", "--name-only", "--diff-filter=ACMR"]
           if staged else ["git", "ls-files"])
    out = subprocess.run(cmd, check=True, capture_output=True, text=True).stdout
    return [line for line in out.splitlines() if line]


def main() -> int:
    staged = "--staged" in sys.argv[1:]
    root = subprocess.run(["git", "rev-parse", "--show-toplevel"],
                          check=True, capture_output=True, text=True).stdout.strip()
    findings: list[str] = []
    for rel in files(staged):
        if rel in ALLOW:
            continue
        if rel.startswith(".env") and rel != ".env.example":
            findings.append(f"{rel}: .env files must stay untracked")
            continue
        path = f"{root}/{rel}"
        try:
            text = open(path, "r", errors="ignore").read()
        except OSError:
            continue
        for label, pattern in PATTERNS.items():
            for match in pattern.finditer(text):
                findings.append(f"{rel}: {label} near {match.group(0)[:12]}...")
    for line in findings:
        print(f"check_secrets: {line}", file=sys.stderr)
    if findings:
        print("check_secrets: FAILED — remove the value and rotate the credential",
              file=sys.stderr)
        return 1
    print(f"check_secrets: ok ({len(files(staged))} files)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
