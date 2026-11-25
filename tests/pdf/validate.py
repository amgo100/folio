"""Validates the PDF fixtures of tests/pdf with veraPDF and checks the results
recorded in tests/pdf/README.md.

Usage:
    python tests/pdf/validate.py [--verapdf CMD] [--dir DIR]

CMD is the veraPDF command line (default: `verapdf`, found on PATH; on
Windows that finds `verapdf.bat`). DIR holds the fixtures (default:
tests/pdf/out). Standard library only. Exits with 1 if any result differs from
the expected one.
"""

import argparse
import os
import shlex
import shutil
import subprocess
import sys
import tempfile
import xml.etree.ElementTree as ET

HERE = os.path.dirname(os.path.abspath(__file__))

# (file, veraPDF profile, expected compliance, rules expected to fail)
# A rule is "clause-test" as veraPDF reports it.
CASES = [
    ("figure-svg-ua1-a2u.pdf", "ua1", True, []),
    ("figure-svg-ua1-a2u.pdf", "2u", True, []),
    ("figure-svg-a4.pdf", "4", True, []),
    # Not identified as PDF/UA or PDF/A (krilla refuses the embedding under
    # a validator); validated anyway to see what the embedded page does.
    ("figure-embed.pdf", "ua1", False, ["5-1", "7.1-1", "7.1-2", "7.1-10"]),
    ("figure-embed.pdf", "2b", False, ["6.6.4-1", "6.2.4.3-2", "6.2.10-2"]),
    ("math-alt-a4.pdf", "4", True, []),
    ("math-alt-a4.pdf", "ua2", True, []),
    ("math-alt-a4.pdf", "wt1a", True, []),
    ("math-se-a4.pdf", "4", True, []),
    ("math-se-a4.pdf", "ua2", True, []),
    ("math-se-a4.pdf", "wt1a", True, []),
    ("math-af-a4.pdf", "4", False, ["6.9-3"]),
    ("math-af-a4.pdf", "ua2", True, []),
    ("math-af-a4.pdf", "wt1a", True, []),
    ("math-af-a4f.pdf", "4f", True, []),
    ("math-af-a4f.pdf", "ua2", True, []),
    ("math-af-a4f.pdf", "wt1a", True, []),
    ("math-both-a4f.pdf", "4f", True, []),
    ("math-both-a4f.pdf", "ua2", True, []),
    ("math-both-a4f.pdf", "wt1a", True, []),
]


def run(verapdf, path, profile):
    """Returns (compliant, failed rules with descriptions)."""
    with tempfile.TemporaryDirectory() as tmp:
        report = os.path.join(tmp, "report.xml")
        with open(report, "wb") as out:
            subprocess.run(verapdf + ["-f", profile, "--format", "xml", path],
                           stdout=out, stderr=subprocess.DEVNULL, check=False)
        root = ET.parse(report).getroot()
    reports = list(root.iter("validationReport"))
    if not reports:
        raise RuntimeError(f"veraPDF gave no report for {path} ({profile})")
    failed = []
    for rule in reports[0].iter("rule"):
        if rule.get("status") == "failed":
            desc = (rule.findtext("description") or "").strip()
            failed.append((f"{rule.get('clause')}-{rule.get('testNumber')}", desc))
    return reports[0].get("isCompliant") == "true", failed


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--verapdf", default="verapdf")
    parser.add_argument("--dir", default=os.path.join(HERE, "out"))
    args = parser.parse_args()

    # Non-POSIX splitting (Windows paths keep their backslashes) also keeps
    # the quotes around a path with spaces, so strip them.
    verapdf = [t.strip('"') for t in shlex.split(args.verapdf, posix=os.name != "nt")]
    found = shutil.which(verapdf[0])
    if not found:
        sys.exit(f"veraPDF not found: {verapdf[0]} (pass --verapdf)")
    verapdf[0] = found

    mismatches = 0
    for name, profile, expected, expected_rules in CASES:
        compliant, failed = run(verapdf, os.path.join(args.dir, name), profile)
        rules = sorted(r for r, _ in failed)
        ok = compliant == expected and rules == sorted(expected_rules)
        mismatches += not ok
        verdict = "pass" if compliant else "fail"
        mark = "" if ok else "   <-- differs from README"
        print(f"{name:24} {profile:5} {verdict}{mark}")
        for rule, desc in failed:
            print(f"    {rule}: {desc[:110]}")
    sys.exit(1 if mismatches else 0)


if __name__ == "__main__":
    main()
