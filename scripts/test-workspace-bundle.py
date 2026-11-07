import importlib.util
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
SCRIPT = HERE / "verify-workspace-bundle.py"
SPEC = importlib.util.spec_from_file_location("verify_workspace_bundle", SCRIPT)
V = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(V)

FP = "843938DF228D22F7B3742BC0D94AA3F0EFE21092"
NAME = "ubuntu-base-24.04-base-amd64.tar.gz"
DIGEST = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
SUB = "11" * 20
FIELDS = "2026-11-07 1 0 4 0 1 8 00"
CHILD = r"""
import importlib.util, os, sys
spec = importlib.util.spec_from_file_location("v", sys.argv[1])
m = importlib.util.module_from_spec(spec)
spec.loader.exec_module(m)
source, dest, kind = sys.argv[2:]
sf = os.open(source, os.O_DIRECTORY)
df = os.open(dest, os.O_DIRECTORY)
try:
    try:
        if kind == "copy":
            m.copy_file(sf, "fifo", df, 64)
        else:
            m.read_private(sf, "fifo", 64)
    except m.BundleError:
        raise SystemExit(0)
    raise SystemExit(3)
finally:
    os.close(sf)
    os.close(df)
"""


def line(digest, name=NAME):
    return f"{digest}  {name}\n".encode()


def sig(fpr=FP, fields=FIELDS, primary=None):
    extra = f" {primary}" if primary else ""
    return f"[GNUPG:] VALIDSIG {fpr} {fields}{extra}\n".encode()


class BundleTests(unittest.TestCase):
    def test_parse_manifest_accepts(self):
        unrelated = line(DIGEST) + line(DIGEST, "other-base.tar.gz")
        for raw in (line(DIGEST), line(DIGEST.upper()), unrelated):
            with self.subTest(raw=raw):
                archive, digest = V.parse_manifest(raw)
                self.assertEqual(archive, NAME)
                self.assertEqual(digest.lower(), DIGEST)

    def test_parse_manifest_rejects(self):
        other = "ubuntu-base-24.04.1-base-amd64.tar.gz"
        cases = {
            "duplicate": line(DIGEST) + line(DIGEST),
            "multi": line(DIGEST) + line(DIGEST, other),
            "digest": line("zz" * 32),
            "dotdot": line(DIGEST, "../" + NAME),
            "abs": line(DIGEST, "/" + NAME),
            "nested": line(DIGEST, "sub/" + NAME),
            "crlf": line(DIGEST)[:-1] + b"\r\n",
            "nonascii": f"{DIGEST}  {NAME[:-3]}\u00e9gz\n".encode(),
            "huge": b"a" * (1024 * 1024 + 1),
        }
        for label, raw in cases.items():
            with self.subTest(label=label):
                with self.assertRaises(V.BundleError):
                    V.parse_manifest(raw)

    def test_parse_status_accepts(self):
        samples = (sig(), sig(primary=FP), sig(SUB, primary=FP))
        for raw in samples:
            with self.subTest(raw=raw):
                self.assertEqual(V.parse_status(raw, 0).upper(), FP)

    def test_parse_status_rejects(self):
        weak = FIELDS.replace(" 8 ", " 2 ")
        cases = {
            "rc": (sig(), 1),
            "missing": (b"[GNUPG:] GOODSIG AA " + NAME.encode() + b"\n", 0),
            "primary": (sig(SUB, primary="22" * 20), 0),
            "failure": (b"[GNUPG:] FAILURE gpg 1\n" + sig(), 0),
            "weak": (sig(fields=weak), 0),
            "dup": (sig() + sig(), 0),
        }
        for label, (raw, rc) in cases.items():
            with self.subTest(label=label):
                with self.assertRaises(V.BundleError):
                    V.parse_status(raw, rc)

    def test_copy_file_abc(self):
        with tempfile.TemporaryDirectory() as td:
            src = Path(td) / "src"
            dst = Path(td) / "dst"
            src.mkdir()
            dst.mkdir()
            (src / "abc").write_bytes(b"abc")
            sf = os.open(src, os.O_DIRECTORY)
            df = os.open(dst, os.O_DIRECTORY)
            try:
                self.assertEqual(V.copy_file(sf, "abc", df, 1024), (DIGEST, 3))
                self.assertEqual((dst / "abc").read_bytes(), b"abc")
                self.assertEqual(V.read_private(df, "abc", 1024), b"abc")
                with self.assertRaises(V.BundleError):
                    V.read_private(df, "abc", 2)
            finally:
                os.close(sf)
                os.close(df)

    def test_copy_file_rejects(self):
        with tempfile.TemporaryDirectory() as td:
            src = Path(td) / "src"
            dst = Path(td) / "dst"
            src.mkdir()
            dst.mkdir()
            (src / "lim").write_bytes(b"abc")
            (src / "real").write_bytes(b"abc")
            os.link(src / "real", src / "hl")
            os.symlink("real", src / "sym")
            (src / "folder").mkdir()
            sparse = os.open(src / "sparse", os.O_CREAT | os.O_WRONLY, 0o600)
            try:
                os.ftruncate(sparse, 8192)
            finally:
                os.close(sparse)
            sf = os.open(src, os.O_DIRECTORY)
            df = os.open(dst, os.O_DIRECTORY)
            try:
                jobs = (
                    ("limit", "lim", 2),
                    ("hardlink", "hl", 1024),
                    ("symlink", "sym", 1024),
                    ("dir", "folder", 1024),
                    ("unsafe", "..", 1024),
                    ("sparse", "sparse", 1024),
                )
                for label, name, limit in jobs:
                    with self.subTest(label=label):
                        with self.assertRaises(V.BundleError):
                            V.copy_file(sf, name, df, limit)
            finally:
                os.close(sf)
                os.close(df)

    def test_fifo_helpers_do_not_hang(self):
        for kind in ("copy", "read"):
            with self.subTest(kind=kind):
                with tempfile.TemporaryDirectory() as td:
                    src = Path(td) / "src"
                    dst = Path(td) / "dst"
                    src.mkdir()
                    dst.mkdir()
                    os.mkfifo(src / "fifo")
                    done = subprocess.run(
                        [sys.executable, "-c", CHILD, str(SCRIPT), str(src), str(dst), kind],
                        timeout=3,
                        capture_output=True,
                    )
                    self.assertEqual(done.returncode, 0)

    def test_verify_bundle_rejects(self):
        with tempfile.TemporaryDirectory() as td:
            root = Path(td)
            source = root / "src"
            source.mkdir(0o700)
            parent = root / "parent"
            parent.mkdir(0o700)
            target = parent / "out.tar"
            with self.subTest("metadata"):
                with self.assertRaises(V.BundleError):
                    V.verify_bundle(str(source), str(target))
                self.assertFalse(os.path.lexists(target))
            target.write_bytes(b"SENTINEL")
            with self.subTest("sentinel"):
                with self.assertRaises((V.BundleError, OSError)):
                    V.verify_bundle(str(source), str(target))
                self.assertEqual(target.read_bytes(), b"SENTINEL")
            wide = root / "wide"
            wide.mkdir(0o755)
            gone = root / "missing"
            link = root / "link"
            os.symlink(source, link)
            os_cases = {
                "parent755": (str(source), str(wide / "t")),
                "inside": (str(source), str(source / "t")),
                "symlink": (str(link), str(root / "elsewhere")),
                "missing": (str(gone), str(root / "no-out")),
            }
            for label, args in os_cases.items():
                with self.subTest(label=label):
                    with self.assertRaises((V.BundleError, OSError)):
                        V.verify_bundle(*args)
                    self.assertFalse(os.path.lexists(args[1]))

    def test_cli_bad_input_hides_paths(self):
        token = "UNTRUSTED_PATH_TOKEN"
        argv_sets = (
            [sys.executable, str(SCRIPT)],
            [sys.executable, str(SCRIPT), token, token],
            [sys.executable, str(SCRIPT), "/" + token, token],
            [sys.executable, str(SCRIPT), token, "/" + token, "extra"],
        )
        for argv in argv_sets:
            with self.subTest(argv=argv):
                done = subprocess.run(argv, timeout=3, capture_output=True)
                self.assertEqual(done.returncode, 1)
                self.assertEqual(done.stdout, b"")
                self.assertEqual(done.stderr, b"error\n")
                self.assertNotIn(token.encode(), done.stderr)

    def test_cli_fifo_fails_fast(self):
        with tempfile.TemporaryDirectory() as td:
            source = Path(td) / "src"
            parent = Path(td) / "parent"
            source.mkdir()
            source.chmod(0o700)
            parent.mkdir()
            parent.chmod(0o700)
            os.mkfifo(source / "SHA256SUMS")
            target = parent / "out.tar"
            done = subprocess.run(
                [sys.executable, str(SCRIPT), str(source), str(target)],
                timeout=3,
                capture_output=True,
            )
            self.assertEqual(done.returncode, 1)
            self.assertFalse(os.path.lexists(target))

    def test_cli_missing_gpgv_no_receipt(self):
        if os.path.isfile("/usr/bin/gpgv") and os.access("/usr/bin/gpgv", os.X_OK):
            self.skipTest("gpgv present")
        with tempfile.TemporaryDirectory() as td:
            source = Path(td) / "src"
            parent = Path(td) / "parent"
            source.mkdir()
            source.chmod(0o700)
            parent.mkdir()
            parent.chmod(0o700)
            for name in ("SHA256SUMS", "SHA256SUMS.gpg", "ubuntu-archive-keyring.gpg"):
                (source / name).write_bytes(b"x")
            target = parent / "out.tar"
            done = subprocess.run(
                [sys.executable, str(SCRIPT), str(source), str(target)],
                timeout=3,
                capture_output=True,
            )
            self.assertEqual(done.returncode, 1)
            self.assertEqual(done.stdout, b"")
            self.assertFalse(os.path.lexists(target))
            self.assertFalse(os.path.lexists(target / "receipt.json"))


if __name__ == "__main__":
    unittest.main(verbosity=2)
