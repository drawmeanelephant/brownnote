#!/usr/bin/env python3
"""Regression checks for prepare.py's retried fetch."""
import http.client
import io
from pathlib import Path
import tempfile
import unittest
import urllib.error
from unittest import mock

import prepare


class BrokenBody:
    """A response whose read() drops mid-transfer."""

    def __init__(self, error):
        self.error = error

    def __enter__(self):
        return self

    def __exit__(self, *args):
        return False

    def read(self):
        raise self.error


class Fetch(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.dest = Path(self.temp.name) / "out.bin"

    def urlopen_returning(self, *outcomes):
        """A fake urlopen yielding each outcome (or raising it) in order."""
        pending = list(outcomes)

        def fake(url, **_kwargs):
            outcome = pending.pop(0)
            if isinstance(outcome, Exception):
                raise outcome
            return outcome

        return mock.patch("urllib.request.urlopen", side_effect=fake)

    def test_writes_the_body_on_the_first_try(self):
        with self.urlopen_returning(io.BytesIO(b"data")) as opened:
            prepare.fetch("https://example.test/x", self.dest, delay=0)
        self.assertEqual(self.dest.read_bytes(), b"data")
        self.assertEqual(opened.call_count, 1)

    def test_recovers_after_a_transient_failure(self):
        with self.urlopen_returning(
            urllib.error.URLError("boom"), io.BytesIO(b"data")
        ) as opened:
            prepare.fetch("https://example.test/x", self.dest, delay=0)
        self.assertEqual(self.dest.read_bytes(), b"data")
        self.assertEqual(opened.call_count, 2)

    def test_recovers_after_a_mid_read_drop(self):
        dropped = BrokenBody(ConnectionResetError("reset by peer"))
        truncated = BrokenBody(http.client.IncompleteRead(b"partial"))
        with self.urlopen_returning(dropped, truncated, io.BytesIO(b"data")) as opened:
            prepare.fetch("https://example.test/x", self.dest, delay=0)
        self.assertEqual(self.dest.read_bytes(), b"data")
        self.assertEqual(opened.call_count, 3)

    def test_propagates_after_the_attempt_budget(self):
        with self.urlopen_returning(
            urllib.error.URLError("down"),
            urllib.error.URLError("down"),
            urllib.error.URLError("down"),
        ) as opened:
            with self.assertRaises(urllib.error.URLError):
                prepare.fetch("https://example.test/x", self.dest, attempts=3, delay=0)
        self.assertEqual(opened.call_count, 3)

    def test_the_request_has_a_timeout(self):
        with self.urlopen_returning(io.BytesIO(b"data")) as opened:
            prepare.fetch("https://example.test/x", self.dest, delay=0)
        self.assertGreater(opened.call_args.kwargs["timeout"], 0)


if __name__ == "__main__":
    unittest.main()
