"""Checks the PDFs that Folio writes for the test corpus in tests/pdf/corpus
(SPEC §11.1, issue #32): each must pass veraPDF's PDF/UA-1 profile, and the
text that `pdftotext` extracts from it must be the document's text.

Usage:
    python tests/pdf/corpus.py [--verapdf CMD] [--pdftotext CMD] [--out DIR]

The script builds the corpus with `cargo test -p folio --test pdf`, which
writes each document's PDF and the text a reader should extract (its
paragraphs, one per line) into DIR (default: a temporary directory). CMD is
the veraPDF command line (default: `verapdf` on PATH) and the `pdftotext`
command (default: `pdftotext`, from Poppler). Standard library only. Exits
with 1 if any document fails.

Text is compared as a reader would copy it: a soft hyphen (U+00AD) at the
end of a line, or on a line of its own (where Poppler puts the actual text of
a hyphen that a break inserted), joins the lines around it; a hyphen after a
letter at the end of a line joins it to the next without a space (the line
broke at an explicit hyphen); any other run of white space, including line
and page breaks, is one space.
"""

import argparse
import os
import re
import shlex
import shutil
import subprocess
import sys
import tempfile
import xml.etree.ElementTree as ET

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(os.path.dirname(HERE))
SHY = "\u00ad"  # soft hyphen


def build(out):
    env = dict(os.environ, FOLIO_PDF_OUT=out)
    subprocess.run(
        ["cargo", "test", "-p", "folio", "--test", "pdf", "--locked", "--quiet"],
        cwd=ROOT, env=env, check=True)


def verapdf(command, path):
    """Returns (compliant, failed rules)."""
    result = subprocess.run(command + ["-f", "ua1", "--format", "xml", path],
                            capture_output=True, check=False)
    try:
        root = ET.fromstring(result.stdout)
        report = next(root.iter("validationReport"))
    except (ET.ParseError, StopIteration):
        sys.exit(f"veraPDF gave no report for {path}:\n"
                 + result.stderr.decode("utf-8", "replace"))
    failed = sorted({
        f"{rule.get('clause')}-{rule.get('testNumber')}"
        for rule in report.iter("rule") if rule.get("status") == "failed"
    })
    return report.get("isCompliant") == "true", failed


def extract(command, path):
    """The text pdftotext extracts, in content order, as a reader copies it."""
    result = subprocess.run(command + ["-raw", "-enc", "UTF-8", path, "-"],
                            capture_output=True, check=True)
    text = result.stdout.decode("utf-8")
    lines = text.replace("\f", "\n").split("\n")
    out = ""
    for line in lines:
        line = line.rstrip()
        if line.strip() == SHY:
            # Poppler puts the actual text of a hyphen at a break on a line
            # of its own.
            out = out.rstrip() + SHY
        elif out.endswith(SHY):
            if line.strip():
                out = out[:-1] + line.lstrip()
        elif re.search(r"\w-$", out):
            out += line.lstrip()
        else:
            out += " " + line
    return normalise(out.replace(SHY, ""))


def normalise(text):
    return " ".join(text.split())


def main():
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("--verapdf", default="verapdf")
    parser.add_argument("--pdftotext", default="pdftotext")
    parser.add_argument("--out", help="where to write the PDFs (kept)")
    args = parser.parse_args()
    verapdf_cmd = shlex.split(args.verapdf)
    if not shutil.which(verapdf_cmd[0]):
        sys.exit(f"veraPDF not found: {args.verapdf}")
    pdftotext_cmd = shlex.split(args.pdftotext)
    if not shutil.which(pdftotext_cmd[0]):
        sys.exit(f"pdftotext not found: {args.pdftotext}")

    with tempfile.TemporaryDirectory() as tmp:
        out = args.out or tmp
        build(out)
        names = sorted(f[:-4] for f in os.listdir(out) if f.endswith(".pdf"))
        if not names:
            sys.exit("the build wrote no PDFs")
        failures = 0
        for name in names:
            pdf = os.path.join(out, f"{name}.pdf")
            compliant, failed = verapdf(verapdf_cmd, pdf)
            with open(os.path.join(out, f"{name}.txt"), encoding="utf-8") as f:
                expected = normalise(f.read())
            extracted = extract(pdftotext_cmd, pdf)
            same = extracted == expected
            ok = compliant and same
            failures += not ok
            status = "ok  " if ok else "FAIL"
            ua = "PDF/UA-1 pass" if compliant else f"PDF/UA-1 FAIL {failed}"
            text = "text round-trips" if same else "text DIFFERS"
            print(f"{status} {name}: {ua}; {text}")
            if not same:
                at = next((i for i, (a, b) in enumerate(zip(expected, extracted))
                           if a != b), min(len(expected), len(extracted)))
                print(f"     expected: ...{expected[max(0, at - 40):at + 40]!r}")
                print(f"     extracted: ...{extracted[max(0, at - 40):at + 40]!r}")
    sys.exit(1 if failures else 0)


if __name__ == "__main__":
    main()
