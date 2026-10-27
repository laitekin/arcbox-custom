import unittest

from check_isolation_dns_capture import audit


class CaptureAuditTests(unittest.TestCase):
    def setUp(self):
        self.controls = [f"abxleak-vz-capture-control-{phase}-1.invalid" for phase in ("before", "after")]
        self.guests = [f"abxleak-vz-{phase}-1.invalid" for phase in (
            "gateway-up", "udp-up", "udp-direct-blocked", "gateway-down", "udp-down", "gateway-recovered")]
        self.log = "\n".join(
            [f"DNS CAPTURE CONTROL {name}" for name in self.controls]
            + [f"DNS PROBE {name}" for name in self.guests]
            + ["test result: ok. 1 passed; 0 failed"]
        )

    def test_empty_or_late_capture_never_passes(self):
        self.assertFalse(audit("", [self.log])["passed"])
        self.assertFalse(audit(self.controls[1], [self.log])["passed"])

    def test_controls_without_guest_queries_pass(self):
        result = audit("\n".join(self.controls), [self.log])
        self.assertTrue(result["passed"])
        self.assertEqual(result["guest_queries_tested"], 6)

    def test_one_guest_query_is_a_leak(self):
        result = audit("\n".join(self.controls + self.guests[:1]), [self.log])
        self.assertFalse(result["passed"])
        self.assertEqual(result["guest_query_leaks"], self.guests[:1])

    def test_failed_functional_run_cannot_be_certified(self):
        with self.assertRaises(ValueError):
            audit("\n".join(self.controls), [self.log.replace("ok. 1 passed", "FAILED. 0 passed")])


if __name__ == "__main__":
    unittest.main()
