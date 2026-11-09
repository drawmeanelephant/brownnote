#!/usr/bin/env python3
"""HTTP regressions for studio CodeQL alerts 1–17, using disposable files."""
import http.client
import importlib.util
import io
import json
import os
from pathlib import Path
import shutil
import tempfile
import threading
import unittest
from unittest.mock import patch
import urllib.parse
import wave


ROOT = Path(__file__).resolve().parents[1]


def wav_bytes(rate=44100):
    buf = io.BytesIO()
    with wave.open(buf, "wb") as wav:
        wav.setnchannels(1)
        wav.setsampwidth(2)
        wav.setframerate(rate)
        wav.writeframes(b"\x01\x00" * 100)
    return buf.getvalue()


class StudioSecurity(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(prefix="studio-security-")
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name, "workspace")
        self.outside = Path(self.tmp.name, "outside")
        self.root.mkdir()
        self.outside.mkdir()
        spec = importlib.util.spec_from_file_location(
            "studio_security_target", ROOT / "scripts/studio.py")
        self.studio = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(self.studio)
        for key, rel in (("ROOT", "."), ("PATCHES_DIR", "build/patches"),
                         ("USER_DIR", "build/user"), ("LIB_DIR", "build/library"),
                         ("LIB_MANIFEST", "build/library.json"),
                         ("JAM_DIR", "build/jam"),
                         ("SERVE_SENTINEL", ".no-such-file")):
            setattr(self.studio, key, str(self.root / rel))
        for rel in ("build/patches", "build/user", "build/library",
                    "build/presets", "build/tune", "recordings"):
            (self.root / rel).mkdir(parents=True, exist_ok=True)
        # Avoid synthesis for /api/state without replacing any handler.
        for stem in self.studio.STEM_DEFS:
            for key in ("url", "dryUrl"):
                (self.root / stem[key].lstrip("/")).write_bytes(wav_bytes())
        self.server = self.studio.ReusableTCPServer(
            ("127.0.0.1", 0), self.studio.StudioHandler)
        thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        thread.start()
        self.addCleanup(thread.join)
        self.addCleanup(self.server.server_close)
        self.addCleanup(self.server.shutdown)

    def request(self, path, payload=None, body=None, headers=None, method=None):
        conn = http.client.HTTPConnection(*self.server.server_address, timeout=15)
        try:
            hdr = {"X-Studio-Client": "1"}
            if payload is not None:
                body = json.dumps(payload).encode()
                hdr["Content-Type"] = "application/json"
            elif body is not None:
                hdr["Content-Type"] = "audio/wav"
            hdr.update(headers or {})
            conn.request(method or ("POST" if body is not None else "GET"),
                         path, body=body, headers=hdr)
            response = conn.getresponse()
            return response.status, response.read()
        finally:
            conn.close()

    def upload(self, endpoint, name, body=None):
        return self.request(endpoint + "?" + urllib.parse.urlencode({"name": name}),
                            body=wav_bytes() if body is None else body)

    def assert_rejected(self, response):
        self.assertIn(response[0], (400, 404), response)

    def symlink_bucket(self, rel):
        bucket = self.root / rel
        bucket.rmdir()
        bucket.symlink_to(self.outside, target_is_directory=True)

    def test_outside_and_prefix_sibling_paths_are_rejected_before_metadata_access(self):
        for candidate in (self.outside / "marker",
                          self.root.with_name(self.root.name + "-sibling") / "marker"):
            with self.subTest(path=candidate):
                with patch.object(self.studio.os.path, "islink",
                                  side_effect=AssertionError("outside metadata access")):
                    with self.assertRaises(ValueError):
                        self.studio._workspace_path(str(candidate))

    def test_trusted_workspace_alias_keeps_patch_roundtrip_and_rejects_child_links(self):
        alias = Path(self.tmp.name, "workspace-alias")
        alias.symlink_to(self.root, target_is_directory=True)
        with patch.dict(self.studio.__dict__, {
                "ROOT": str(alias), "PATCHES_DIR": str(alias / "build/patches")}):
            self.assertEqual(self.studio._workspace_path(str(alias)), str(alias))
            self.assertEqual(self.request("/api/patch", {
                "name": "saved", "patch": {"v": 1}})[0], 200)
            self.assertEqual(self.request("/api/patch?name=saved")[0], 200)
            self.assertTrue((self.root / "build/patches/saved.json").is_file())
            link = self.root / "build/patches/linked.json"
            marker = self.outside / "marker.json"
            marker.write_text('{"v": 1}')
            link.symlink_to(marker)
            self.assert_rejected(self.request("/api/patch?name=linked"))
            self.assertTrue(link.is_symlink())
            self.assertEqual(marker.read_text(), '{"v": 1}')

    def test_patch_roundtrip_and_invalid_names(self):
        name = "Saved kit + 1"
        self.assertEqual(self.request("/api/patch", {
            "name": name, "patch": {"v": 1, "seq": {"bpm": 116}}})[0], 200)
        url = "/api/patch?" + urllib.parse.urlencode({"name": name})
        code, body = self.request(url)
        self.assertEqual(code, 200)
        self.assertEqual(json.loads(body)["seq"]["bpm"], 116)
        for bad in ("../outside", str(self.outside / "escape"), r"..\outside",
                    "%2e%2e%2fescape", "good\n", "", "a" * 65):
            with self.subTest(name=bad):
                self.assert_rejected(self.request("/api/patch", {
                    "name": bad, "patch": {"v": 1}}))
                self.assert_rejected(self.request("/api/patch?" +
                    urllib.parse.urlencode({"name": bad})))
                self.assert_rejected(self.request("/api/patch", {
                    "name": bad, "delete": True}))
        self.assertEqual(self.request("/api/patch", {
            "name": name, "delete": True})[0], 200)
        self.assertFalse(list(self.outside.iterdir()))

    def test_patch_file_symlink_is_not_read_saved_or_deleted(self):
        target = self.outside / "marker.json"
        marker = b'{"v": 1, "name": "outside-marker"}'
        target.write_bytes(marker)
        link = self.root / "build/patches/linked.json"
        link.symlink_to(target)
        self.assert_rejected(self.request("/api/patch?name=linked"))
        self.assert_rejected(self.request("/api/patch", {
            "name": "linked", "patch": {"v": 1}}))
        self.assert_rejected(self.request("/api/patch", {
            "name": "linked", "delete": True}))
        self.assertEqual(target.read_bytes(), marker)
        self.assertTrue(link.is_symlink())
        self.assertNotIn("linked", [p["name"] for p in
            json.loads(self.request("/api/patches")[1])])

    def test_patch_directory_symlink_cannot_read_write_or_delete(self):
        self.symlink_bucket("build/patches")
        marker = self.outside / "saved.json"
        marker.write_text('{"v": 1}')
        self.assert_rejected(self.request("/api/patch?name=saved"))
        self.assert_rejected(self.request("/api/patch", {
            "name": "new", "patch": {"v": 1}}))
        self.assert_rejected(self.request("/api/patch", {
            "name": "saved", "delete": True}))
        self.assertTrue(marker.exists())
        self.assertFalse((self.outside / "new.json").exists())

    def test_dangling_and_internal_patch_symlinks_are_rejected(self):
        for name, target in (("dangling", self.outside / "missing.json"),
                             ("alias", self.root / "build/patches/real.json")):
            with self.subTest(name=name):
                link = self.root / ("build/patches/" + name + ".json")
                link.symlink_to(target)
                self.assert_rejected(self.request("/api/patch", {
                    "name": name, "patch": {"v": 1}}))
                self.assertTrue(link.is_symlink())
                self.assertFalse(target.exists())

    def test_cyclic_patch_symlinks_are_rejected(self):
        link = self.root / "build/patches/cyclic.json"
        link.symlink_to("cyclic.json")
        self.assert_rejected(self.request("/api/patch", {
            "name": "cyclic", "patch": {"v": 1}}))
        self.assertTrue(link.is_symlink())
        link.unlink()
        bucket = self.root / "build/patches"
        bucket.rmdir()
        bucket.symlink_to("patches", target_is_directory=True)
        self.assert_rejected(self.request("/api/patch", {
            "name": "new", "patch": {"v": 1}}))

    def test_failed_atomic_write_cleans_checked_tempfile(self):
        with patch.object(self.studio.os, "replace", side_effect=OSError("fixture")):
            self.assertEqual(self.request("/api/patch", {
                "name": "failed", "patch": {"v": 1}})[0], 500)
        self.assertFalse(list((self.root / "build/patches").iterdir()))
        self.assertFalse(list(self.outside.iterdir()))

    def test_samples_valid_and_invalid_names(self):
        for endpoint in ("/api/upload-sample", "/api/library/intake"):
            for bad in ("../escape", str(self.outside / "escape"),
                        r"..\escape", "valid\n", "a" * 49):
                with self.subTest(endpoint=endpoint, name=bad):
                    self.assert_rejected(self.upload(endpoint, bad))
            code, body = self.upload(endpoint, "My sample 1")
            self.assertEqual(code, 200, body)
        self.assertEqual(self.request("/build/user/my_sample_1.wav")[0], 200)
        self.assertEqual(self.request("/api/upload-sample", {
            "name": "My sample 1", "delete": True})[0], 200)
        self.assertEqual(self.request("/api/patch", {
            "name": "a" * 64, "patch": {"v": 1}})[0], 200)
        self.assertEqual(self.upload("/api/upload-sample", "a" * 48)[0], 200)
        self.assertFalse(list(self.outside.iterdir()))

    def test_sample_directory_symlink_cannot_write_or_delete(self):
        self.symlink_bucket("build/user")
        marker = self.outside / "saved.wav"
        marker.write_bytes(wav_bytes())
        (self.outside / "saved.json").write_text(json.dumps({
            "id": "user-saved", "name": "outside-label",
            "url": "/build/user/saved.wav"}))
        self.assert_rejected(self.upload("/api/upload-sample", "new"))
        self.assert_rejected(self.request("/api/upload-sample", {
            "name": "saved", "delete": True}))
        self.assertTrue(marker.exists())
        self.assertFalse((self.outside / "new.wav").exists())
        self.assert_rejected(self.request("/api/upload-sample", {
            "id": "user-saved", "delete": True}))
        code, body = self.request("/api/state")
        self.assertEqual(code, 200, body)
        self.assertNotIn(b"outside-label", body)

    def test_id_delete_does_not_follow_backing_wav_symlink(self):
        self.assertEqual(self.upload("/api/upload-sample", "saved")[0], 200)
        wav = self.root / "build/user/saved.wav"
        wav.unlink()
        marker = self.outside / "marker.wav"
        marker.write_bytes(wav_bytes())
        wav.symlink_to(marker)
        self.assert_rejected(self.request("/api/upload-sample", {
            "id": "user-saved", "delete": True}))
        self.assertTrue(wav.is_symlink())
        self.assertTrue((self.root / "build/user/saved.json").exists())
        self.assertEqual(marker.read_bytes(), wav_bytes())

    def test_sample_file_symlinks_cannot_be_replaced(self):
        for suffix in ("wav", "json"):
            with self.subTest(suffix=suffix):
                name = "linked" + suffix
                target = self.outside / ("marker." + suffix)
                target.write_bytes(b"benign-marker")
                link = self.root / ("build/user/" + name + "." + suffix)
                link.symlink_to(target)
                self.assert_rejected(self.upload("/api/upload-sample", name))
                self.assert_rejected(self.request("/api/upload-sample", {
                    "name": name, "delete": True}))
                self.assertTrue(link.is_symlink())
                self.assertEqual(target.read_bytes(), b"benign-marker")
                link.unlink()

    def test_library_symlink_definition_cannot_be_read_or_unlinked(self):
        (self.root / "recordings/raw.wav").write_bytes(wav_bytes(22050))
        target = self.outside / "marker.json"
        target.write_text('{"url":"/recordings/raw.wav"}')
        link = self.root / "build/library/lib-promoted.json"
        link.symlink_to(target)
        self.request("/api/library/rescan", {})
        entries = json.loads(self.request("/api/library")[1])["entries"]
        entry = next(e for e in entries if e["file"] == "recordings/raw.wav")
        self.assert_rejected(self.request("/api/library/promote", {
            "id": entry["id"], "name": "promoted"}))
        self.assert_rejected(self.upload("/api/library/intake", "promoted"))
        self.assertTrue(link.is_symlink())
        self.assertEqual(json.loads(target.read_text())["url"], "/recordings/raw.wav")

    def test_library_directory_symlink_cannot_write(self):
        self.symlink_bucket("build/library")
        self.assert_rejected(self.upload("/api/library/intake", "new"))
        self.assertFalse(list(self.outside.iterdir()))

    def test_library_promote_default_delete_roundtrip(self):
        (self.root / "recordings/raw.wav").write_bytes(wav_bytes(22050))
        self.assertEqual(self.request("/api/library/rescan", {})[0], 200)
        entries = json.loads(self.request("/api/library")[1])["entries"]
        entry = next(e for e in entries if e["file"] == "recordings/raw.wav")
        for bad in ("../escape", str(self.outside), "valid\n"):
            self.assert_rejected(self.request("/api/library/promote", {
                "id": entry["id"], "name": bad}))
        code, body = self.request("/api/library/promote", {
            "id": entry["id"], "name": "Promoted sample"})
        self.assertEqual(code, 200, body)
        sid = json.loads(body)["stem"]["id"]
        self.assertEqual(self.request("/api/library/default", {"id": sid})[0], 200)
        self.assertEqual(self.request("/api/upload-sample", {
            "id": sid, "delete": True})[0], 200)
        self.assertTrue((self.root / "recordings/raw.wav").exists())

    def test_request_gate_and_static_symlink_escape(self):
        self.assertEqual(self.request("/api/patch?name=kit",
            headers={"X-Studio-Client": ""})[0], 403)
        self.assertEqual(self.request("/api/patch?name=kit",
            headers={"Origin": "https://example.invalid"})[0], 403)
        self.assertEqual(self.request("/api/patch?name=kit",
            headers={"Host": "example.invalid"})[0], 403)
        marker = self.outside / "marker.wav"
        marker.write_bytes(wav_bytes())
        (self.root / "build/user/escape.wav").symlink_to(marker)
        self.assertEqual(self.request("/build/user/escape.wav")[0], 404)

    def test_render_rejects_option_names_and_unknown_presets(self):
        scripts = self.root / "scripts"
        scripts.mkdir()
        shutil.copy2(ROOT / "scripts/presets.sh", scripts / "presets.sh")
        for mode in ("fx", "song"):
            for name in ("--help", "-out", "../escape", str(self.outside),
                         "doom\n", "not-a-preset"):
                with self.subTest(mode=mode, name=name):
                    self.assertEqual(self.request("/api/render-preset", {
                        "mode": mode, "name": name})[0], 400)

    def test_render_real_script_preserves_phrase_as_one_argument(self):
        scripts = self.root / "scripts"
        scripts.mkdir()
        shutil.copy2(ROOT / "scripts/presets.sh", scripts / "presets.sh")
        bins = self.root / "test-bin"
        bins.mkdir()
        capture = self.root / "argv.json"
        # Only the external audio tools are stand-ins. presets.sh's real
        # option parser, quoting, and child processes run unchanged.
        synth = bins / "kujamba"
        synth.write_text("""#!/usr/bin/env python3
import json, os, sys, wave
args = sys.argv[1:]
with open(os.environ["STUDIO_TEST_ARGV"], "w") as f:
    json.dump(args, f)
with wave.open(args[args.index("--out") + 1], "wb") as w:
    w.setnchannels(1)
    w.setsampwidth(2)
    w.setframerate(44100)
    w.writeframes(b"\\x00\\x00" * 100)
""")
        encoder = bins / "ffmpeg"
        encoder.write_text("""#!/usr/bin/env python3
import pathlib, sys
pathlib.Path(sys.argv[-1]).write_bytes(b"test-encoded-audio")
""")
        synth.chmod(0o755)
        encoder.chmod(0o755)
        env = {"KUJAMBA": str(synth), "BROWNNOTE": "/usr/bin/true",
               "STUDIO_TEST_ARGV": str(capture),
               "PATH": str(bins) + os.pathsep + os.environ["PATH"]}
        with patch.dict(os.environ, env):
            for phrase in ("--out " + str(self.outside / "option-marker"),
                           "$(touch shell-marker); 'quoted' \"words\"\nnext"):
                code, body = self.request("/api/render-preset", {
                    "mode": "fx", "name": "dry", "phrase": phrase,
                    "seed": -2147483648})
                self.assertEqual(code, 200, body)
                args = json.loads(capture.read_text())
                self.assertEqual(args[args.index("--phrase") + 1], phrase)
                self.assertEqual(args.count("--out"), 1)
                self.assertEqual(args[args.index("--seed") + 1], "-2147483648")
        self.assertFalse(list(self.outside.iterdir()))
        self.assertFalse((self.root / "shell-marker").exists())
        self.assertEqual(self.request("/build/presets/fx_dry.mp3")[0], 200)


if __name__ == "__main__":
    unittest.main(verbosity=2)
