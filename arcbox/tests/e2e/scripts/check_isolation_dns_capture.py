#!/usr/bin/env python3
"""Audit the abxleak-* capture against successful isolation-run logs.

An empty capture is never a pass: each run's direct-host DNS controls must
appear, while none of the guest query names may appear on host UDP/53.
"""
import argparse
import json
import re
import subprocess
from pathlib import Path


def audit(capture_text, run_texts):
    captured = set(re.findall(r"abxleak-[a-z0-9-]+\.invalid", capture_text))
    controls, guest = set(), set()
    for text in run_texts:
        if "test result: ok. 1 passed; 0 failed" not in text:
            raise ValueError("a supplied live run did not pass")
        run_controls = set(re.findall(r"DNS CAPTURE CONTROL (abxleak-[a-z0-9-]+\.invalid)", text))
        run_guest = set(re.findall(r"DNS PROBE (abxleak-[a-z0-9-]+\.invalid)", text))
        if len(run_controls) != 2 or len(run_guest) < 6:
            raise ValueError("live run is missing DNS phases or controls")
        controls |= run_controls
        guest |= run_guest
    missing = controls - captured
    leaked = guest & captured
    result = {
        "passed": not missing and not leaked,
        "controls_expected": len(controls),
        "controls_captured": len(controls & captured),
        "guest_queries_tested": len(guest),
        "guest_query_leaks": sorted(leaked),
        "missing_controls": sorted(missing),
    }
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("capture", type=Path)
    parser.add_argument("logs", type=Path, nargs="+")
    args = parser.parse_args()
    decoded = subprocess.run(
        ["/usr/sbin/tcpdump", "-nn", "-r", str(args.capture),
         "udp port 53 and udp[21:4] = 0x6162786c"],
        capture_output=True, text=True, check=True,
    )
    result = audit(decoded.stdout, [path.read_text() for path in args.logs])
    print(json.dumps(result, indent=2))
    raise SystemExit(0 if result["passed"] else 1)


if __name__ == "__main__":
    main()
