import gzip
import hashlib
import importlib.util
import io
import os
import subprocess
import sys
import tarfile
import tempfile
import unittest
import unittest.mock
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


def _archive(entries, fmt=tarfile.USTAR_FORMAT, pax=None, cut=0, tail=b"", plain=None):
    if plain is not None:
        return plain
    buf = io.BytesIO()
    with tarfile.open(fileobj=buf, mode="w", format=fmt) as tar:
        for name, kind, data, mode, link in entries:
            info = tarfile.TarInfo(name)
            info.type = kind
            info.mode = mode
            info.uid = 0
            info.gid = 0
            info.mtime = 0
            info.linkname = link or ""
            if kind in (tarfile.CHRTYPE, tarfile.BLKTYPE):
                info.devmajor, info.devminor = 1, 3
            if pax:
                info.pax_headers = dict(pax)
            payload = data or b""
            info.size = len(payload) if kind in (tarfile.REGTYPE, tarfile.AREGTYPE) else 0
            tar.addfile(info, io.BytesIO(payload) if info.size else None)
    raw = buf.getvalue()
    if cut:
        raw = raw[:cut]
    return gzip.compress(raw + tail)


def _unpack(entries, expect_success=False, **kw):
    with tempfile.TemporaryDirectory() as raw:
        os.chmod(raw, 0o700)
        base = Path(raw)
        outside = base / "outside"
        outside.write_bytes(b"HOST")
        snap = base / "snapshot"
        snap.mkdir(mode=0o700)
        os.chmod(snap, 0o700)
        fixed = []
        for name, kind, data, mode, link in entries:
            if name == "ABS":
                name = str(outside)
            fixed.append((name, kind, data, mode, link))
        wall = kw.pop("wall", None)
        gz = _archive(fixed, **kw)
        (snap / NAME).write_bytes(gz)
        fd = os.open(snap, os.O_RDONLY)
        try:
            try:
                verify = (fd, NAME, hashlib.sha256(gz).hexdigest(), len(gz))
                if wall is None:
                    V._unpack_verified(*verify)
                else:
                    with unittest.mock.patch.object(V, "UNPACK_WALL_SECONDS", wall):
                        V._unpack_verified(*verify)
            except (V.BundleError, OSError):
                assert not (snap / "rootfs").exists()
                assert not (snap / ".rootfs-partial").exists()
                assert outside.read_bytes() == b"HOST"
                raise
            if not expect_success:
                raise AssertionError
            root = snap / "rootfs"
            proof = root / "etc" / "proof"
            assert proof.read_bytes() == b"proof"
            for rel, mode in (("etc", 0o700), ("etc/proof", 0o600)):
                st = os.stat(root / rel)
                assert st.st_mode & 0o777 == mode
                assert st.st_uid == os.getuid()
        finally:
            os.close(fd)
            assert outside.read_bytes() == b"HOST"


_OK = (
    (".", tarfile.DIRTYPE, b"", 0o755, None),
    ("./etc/proof", tarfile.REGTYPE, b"proof", 0o644, None),
    ("etc", tarfile.DIRTYPE, b"", 0o755, None),
)


class UnpackTests(unittest.TestCase):
    def test_normal_members(self):
        _unpack(_OK, expect_success=True)

    def test_bad_table(self):
        reg, dire = tarfile.REGTYPE, tarfile.DIRTYPE
        cases = {
            "abs": (("ABS", reg, b"x", 0o644, None),),
            "dotdot": (("../../outside", reg, b"x", 0o644, None),),
            "dotdot-mid": (("a/../outside", reg, b"x", 0o644, None),),
            "slash": (("etc\\proof", reg, b"x", 0o644, None),),
            "dup": (("a", reg, b"a", 0o644, None), ("./a", reg, b"b", 0o644, None)),
            "file-parent": (("a", reg, b"a", 0o644, None), ("a/b", reg, b"b", 0o644, None)),
            "root-dup": ((".", dire, b"", 0o755, None), (".", dire, b"", 0o755, None)),
            "root-reg": ((".", reg, b"x", 0o644, None),),
            "symlink": (
                ("jump", tarfile.SYMTYPE, b"", 0o777, "../../"),
                ("jump/outside", reg, b"x", 0o644, None),
            ),
            "hardlink": (("a", reg, b"a", 0o644, None), ("b", tarfile.LNKTYPE, b"", 0o644, "a")),
            "chr": (("d", tarfile.CHRTYPE, b"", 0o644, None),),
            "blk": (("d", tarfile.BLKTYPE, b"", 0o644, None),),
            "fifo": (("d", tarfile.FIFOTYPE, b"", 0o644, None),),
            "sock": (("d", b"s", b"", 0o644, None),),
            "unknown": (("d", b"Z", b"", 0o644, None),),
            "sparse": (("d", tarfile.GNUTYPE_SPARSE, b"", 0o644, None),),
            "setuid": (("./etc/proof", reg, b"proof", 0o4755, None),),
            "setgid": (("./etc/proof", reg, b"proof", 0o2755, None),),
        }
        for label, entries in cases.items():
            with self.subTest(label=label):
                with self.assertRaises((V.BundleError, OSError)):
                    _unpack(entries)

    def test_pax_and_framing(self):
        reg = tarfile.REGTYPE
        member = (("./etc/proof", reg, b"proof", 0o644, None),)
        frames = {
            "pax-cap": dict(entries=member, fmt=tarfile.PAX_FORMAT, pax={"SCHILY.xattr.security.capability": "\x00"}),
            "bad-gzip": dict(entries=member, plain=b"not-gzip"),
            "trunc-tar": dict(entries=member, cut=100),
            "tail": dict(entries=_OK, tail=b"\x01" * 512),
        }
        for label, kw in frames.items():
            with self.subTest(label=label):
                with self.assertRaises((V.BundleError, OSError)):
                    entries = kw["entries"]
                    rest = {k: v for k, v in kw.items() if k != "entries"}
                    _unpack(entries, **rest)

    def test_limits(self):
        nested = (("a/b/c", tarfile.REGTYPE, b"z", 0o644, None),)
        tight = (
            ("MAX_FILE", 2, _OK),
            ("MAX_EXPANDED", 512, _OK),
            ("MAX_RATIO", 1, _OK),
            ("MAX_ENTRIES", 1, _OK),
            ("MAX_ENTRIES", 2, nested),
        )
        for name, value, entries in tight:
            with self.subTest(name=name, value=value):
                with unittest.mock.patch.object(V, name, value):
                    with self.assertRaises((V.BundleError, OSError)):
                        _unpack(entries)

    def test_worker_timeout_reaped(self):
        import time

        def sleep2(*_a, **_k):
            time.sleep(2)

        started = time.monotonic()
        with unittest.mock.patch.object(V, "_unpack_stream", sleep2):
            with self.assertRaises(V.BundleError):
                _unpack(_OK, wall=.05)
        self.assertLess(time.monotonic() - started, 1)
        with self.assertRaises(ChildProcessError):
            os.waitpid(-1, os.WNOHANG)


if __name__ == "__main__":
    unittest.main(verbosity=2)
