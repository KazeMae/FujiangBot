import hashlib
import importlib.util
import io
import json
import os
import shutil
import subprocess
import tarfile
import tempfile
import unittest

_HERE = os.path.dirname(os.path.abspath(__file__))
_SPEC = importlib.util.spec_from_file_location(
    "verify_workspace_bundle",
    os.path.join(_HERE, "verify-workspace-bundle.py"),
)
verify = importlib.util.module_from_spec(_SPEC)
_SPEC.loader.exec_module(verify)

_PROD = "843938DF228D22F7B3742BC0D94AA3F0EFE21092"
_ARCHIVE = "ubuntu-base-24.04-base-amd64.tar.gz"
_GPG = "/usr/bin/gpg"
_GPGV = "/usr/bin/gpgv"
_GPGCONF = "/usr/bin/gpgconf"
_DEPS = os.path.isfile(_GPG) and os.access(_GPG, os.X_OK) and os.path.isfile(_GPGV) and os.access(_GPGV, os.X_OK)
_ALPHA = "Fixture Alpha <fixture-alpha@example.invalid>"
_BETA = "Fixture Beta <fixture-beta@example.invalid>"


def _env(home):
    return {"PATH": "/usr/bin:/bin", "HOME": home, "GNUPGHOME": home, "LANG": "C"}


def _run(args, home, timeout):
    return subprocess.run(
        args,
        stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        timeout=timeout,
        env=_env(home),
        check=False,
    )


@unittest.skipUnless(_DEPS or os.environ.get("REQUIRE_GPGV") == "1", "gpgv or gpg missing")
class WorkspaceBundleGpgvTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        if not _DEPS:
            raise AssertionError("REQUIRE_GPGV=1 but /usr/bin/gpgv or /usr/bin/gpg is missing")
        cls.original_primary = verify.OFFICIAL_PRIMARY
        cls.home_td = tempfile.TemporaryDirectory(prefix="wb-gnupg-")
        cls.home = cls.home_td.name
        os.chmod(cls.home, 0o700)
        with open(os.path.join(cls.home, "gpg-agent.conf"), "w", encoding="ascii") as fh:
            fh.write("allow-loopback-pinentry\n")
        os.chmod(os.path.join(cls.home, "gpg-agent.conf"), 0o600)
        try:
            for uid in (_ALPHA, _BETA):
                proc = _run(
                    [
                        _GPG,
                        "--no-options",
                        "--homedir",
                        cls.home,
                        "--batch",
                        "--pinentry-mode",
                        "loopback",
                        "--passphrase",
                        "",
                        "--quick-generate-key",
                        uid,
                        "ed25519",
                        "sign",
                        "0",
                    ],
                    cls.home,
                    60,
                )
                if proc.returncode != 0:
                    raise AssertionError(proc.stderr.decode("utf-8", "replace"))
            listed = _run([_GPG, "--no-options", "--homedir", cls.home, "--batch", "--with-colons", "--list-keys"], cls.home, 20)
            if listed.returncode != 0:
                raise AssertionError(listed.stderr.decode("utf-8", "replace"))
            fps = {}
            current = None
            for line in listed.stdout.decode("ascii").splitlines():
                parts = line.split(":")
                if parts[0] == "fpr" and current is None:
                    current = parts[9]
                elif parts[0] == "uid" and current:
                    fps[parts[9]] = current
                    current = None
            cls.alpha = fps[_ALPHA]
            cls.beta = fps[_BETA]
            cls.ring = os.path.join(cls.home, "pubring.gpg")
            exported = _run(
                [_GPG, "--no-options", "--homedir", cls.home, "--batch", "--export", "--output", cls.ring],
                cls.home,
                20,
            )
            if exported.returncode != 0 or not os.path.isfile(cls.ring):
                raise AssertionError(exported.stderr.decode("utf-8", "replace"))
        except Exception:
            cls._kill()
            cls.home_td.cleanup()
            raise

    @classmethod
    def _kill(cls):
        if os.path.isfile(_GPGCONF) and os.access(_GPGCONF, os.X_OK):
            _run([_GPGCONF, "--homedir", cls.home, "--kill", "gpg-agent"], cls.home, 20)

    @classmethod
    def tearDownClass(cls):
        try:
            cls._kill()
        finally:
            cls.home_td.cleanup()
        verify.OFFICIAL_PRIMARY = cls.original_primary

    def setUp(self):
        verify.OFFICIAL_PRIMARY = self.alpha
        self.src_td = tempfile.TemporaryDirectory(prefix="wb-src-")
        self.dst_td = tempfile.TemporaryDirectory(prefix="wb-dst-")
        os.chmod(self.src_td.name, 0o700)
        os.chmod(self.dst_td.name, 0o700)
        self.source = self.src_td.name
        self.target = os.path.join(self.dst_td.name, "private-target")
        self.payload = b"fixture-archive-not-a-rootfs\n"
        self.digest = hashlib.sha256(self.payload).hexdigest()

    def tearDown(self):
        verify.OFFICIAL_PRIMARY = self.original_primary
        self.src_td.cleanup()
        self.dst_td.cleanup()

    def _sign(self, manifest, signer):
        sums = os.path.join(self.home, "SHA256SUMS")
        sig = os.path.join(self.home, "SHA256SUMS.gpg")
        with open(sums, "wb") as fh:
            fh.write(manifest)
        proc = _run(
            [
                _GPG,
                "--no-options",
                "--homedir",
                self.home,
                "--batch",
                "--pinentry-mode",
                "loopback",
                "--passphrase",
                "",
                "--local-user",
                signer,
                "--detach-sign",
                "--yes",
                "--output",
                sig,
                sums,
            ],
            self.home,
            20,
        )
        if proc.returncode != 0:
            self.fail(proc.stderr.decode("utf-8", "replace"))
        with open(sig, "rb") as fh:
            return fh.read()

    def _manifest(self, digest=None):
        return (("%s *%s\n" % (self.digest if digest is None else digest, _ARCHIVE)).encode("ascii"))

    def _fill(self, signer=None, manifest=None, payload=None, signature=None, keyring=True):
        signer = self.alpha if signer is None else signer
        manifest = self._manifest() if manifest is None else manifest
        payload = self.payload if payload is None else payload
        signature = self._sign(manifest, signer) if signature is None else signature
        with open(os.path.join(self.source, "SHA256SUMS"), "wb") as fh:
            fh.write(manifest)
        with open(os.path.join(self.source, "SHA256SUMS.gpg"), "wb") as fh:
            fh.write(signature)
        with open(os.path.join(self.source, _ARCHIVE), "wb") as fh:
            fh.write(payload)
        if keyring:
            shutil.copy(self.ring, os.path.join(self.source, "ubuntu-archive-keyring.gpg"))

    def _assert_no_residue(self):
        self.assertFalse(os.path.lexists(self.target))
        self.assertEqual(set(os.listdir(self.dst_td.name)), set())

    def _assert_receipt(self, rec):
        self.assertEqual(rec["archive"], _ARCHIVE)
        self.assertEqual(rec["sha256"], self.digest)
        self.assertEqual(rec["bytes"], len(self.payload))
        self.assertEqual(rec["primary"], self.alpha)
        self.assertIs(rec["extracted"], False)
        self.assertIs(rec["execution_accepted"], False)

    def test_production_primary_constant(self):
        self.assertEqual(self.original_primary, _PROD)
        self.assertEqual(_PROD, "843938DF228D22F7B3742BC0D94AA3F0EFE21092")

    def test_gpgv_accepts_fixture_and_snapshot_is_stable(self):
        self._fill()
        rec = verify.verify_bundle(self.source, self.target)
        with open(os.path.join(self.source, _ARCHIVE), "wb") as fh:
            fh.write(b"source-mutated-after-snapshot\n")
        with open(os.path.join(self.source, "SHA256SUMS"), "wb") as fh:
            fh.write(b"ff" * 32 + b" *" + _ARCHIVE.encode("ascii") + b"\n")
        self._assert_receipt(rec)
        self.assertFalse(rec["extracted"])
        self.assertFalse(rec["execution_accepted"])
        with open(os.path.join(self.target, _ARCHIVE), "rb") as fh:
            self.assertEqual(fh.read(), self.payload)
        with open(os.path.join(self.target, "SHA256SUMS"), "rb") as fh:
            self.assertEqual(fh.read(), self._manifest())
        with open(os.path.join(self.target, "receipt.json"), encoding="utf-8") as fh:
            self.assertEqual(json.load(fh), rec)

    def test_tampered_manifest_rejected(self):
        self._fill()
        with open(os.path.join(self.source, "SHA256SUMS"), "wb") as fh:
            fh.write(self._manifest("a" * 64))
        with self.assertRaises(verify.BundleError):
            verify.verify_bundle(self.source, self.target)
        self._assert_no_residue()

    def test_tampered_archive_rejected(self):
        self._fill(payload=self.payload + b"tamper")
        with self.assertRaises(verify.BundleError):
            verify.verify_bundle(self.source, self.target)
        self._assert_no_residue()

    def test_bad_signature_rejected(self):
        self._fill()
        path = os.path.join(self.source, "SHA256SUMS.gpg")
        with open(path, "rb") as fh:
            blob = fh.read()
        with open(path, "wb") as fh:
            fh.write(blob[:-1] + bytes([blob[-1] ^ 0xFF]))
        with self.assertRaises(verify.BundleError):
            verify.verify_bundle(self.source, self.target)
        self._assert_no_residue()

    def test_missing_keyring_rejected(self):
        self._fill(keyring=False)
        with self.assertRaises(verify.BundleError):
            verify.verify_bundle(self.source, self.target)
        self._assert_no_residue()

    def test_wrong_signer_rejected_when_ring_has_both(self):
        self._fill(signer=self.beta)
        self.assertEqual(verify.OFFICIAL_PRIMARY, self.alpha)
        with self.assertRaises(verify.BundleError):
            verify.verify_bundle(self.source, self.target)
        self._assert_no_residue()

    def test_unpatched_production_primary_rejects_fixture(self):
        verify.OFFICIAL_PRIMARY = _PROD
        self.assertEqual(verify.OFFICIAL_PRIMARY, "843938DF228D22F7B3742BC0D94AA3F0EFE21092")
        self._fill(signer=self.alpha)
        with self.assertRaises(verify.BundleError):
            verify.verify_bundle(self.source, self.target)
        self._assert_no_residue()

    def _ustar(self, write, fmt=tarfile.USTAR_FORMAT):
        bio = io.BytesIO()
        with tarfile.open(fileobj=bio, mode="w:gz", format=fmt) as tf:
            write(tf)
        self.payload = bio.getvalue()
        self.digest = hashlib.sha256(self.payload).hexdigest()

    def _dot(self):
        info = tarfile.TarInfo(".")
        info.type = tarfile.DIRTYPE
        info.mode = 0o755
        return info

    def test_unpack_extracts_signed_fixture_rootfs(self):
        proof = b"not-ubuntu\n"

        def write(tf):
            tf.addfile(self._dot())
            info = tarfile.TarInfo("./etc/proof")
            info.type = tarfile.REGTYPE
            info.mode = 0o644
            info.size = len(proof)
            tf.addfile(info, io.BytesIO(proof))

        self._ustar(write)
        self._fill()
        rec = verify.verify_bundle(self.source, self.target, unpack=True)
        self.assertEqual(rec["archive"], _ARCHIVE)
        self.assertEqual(rec["sha256"], self.digest)
        self.assertEqual(rec["bytes"], len(self.payload))
        self.assertEqual(rec["primary"], self.alpha)
        self.assertIs(rec["extracted"], True)
        self.assertIs(rec["execution_accepted"], False)
        self.assertEqual(rec["tree_policy"], "workspace-rootfs-runtime-v1")
        root_fd = os.open(os.path.join(self.target, "rootfs"), os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
        try:
            self.assertEqual(rec["tree_sha256"], verify.tree_sha256(root_fd))
        finally:
            os.close(root_fd)
        self.assertFalse(os.path.lexists(os.path.join(self.target, "acceptance.json")))
        proof_path = os.path.join(self.target, "rootfs", "etc", "proof")
        with open(proof_path, "rb") as fh:
            self.assertEqual(fh.read(), proof)
        self.assertFalse(os.path.islink(proof_path))
        self.assertEqual(os.stat(os.path.join(self.target, "rootfs")).st_mode & 0o777, 0o700)
        with open(os.path.join(self.target, "receipt.json"), encoding="utf-8") as fh:
            self.assertEqual(json.load(fh), rec)

    def test_unpack_rechecks_sha_when_snapshot_archive_changes(self):
        proof = b"not-ubuntu\n"

        def write(tf):
            tf.addfile(self._dot())
            info = tarfile.TarInfo("./etc/proof")
            info.type = tarfile.REGTYPE
            info.mode = 0o644
            info.size = len(proof)
            tf.addfile(info, io.BytesIO(proof))

        self._ustar(write)
        self._fill()
        snap = os.path.join(self.dst_td.name, "snap-src")
        rec = verify.verify_bundle(self.source, snap, unpack=False)
        self.assertIs(rec["extracted"], False)
        self.assertIs(rec["execution_accepted"], False)
        arch = os.path.join(snap, _ARCHIVE)
        with open(arch, "ab") as fh:
            fh.write(b"\n")
        nxt = os.path.join(self.dst_td.name, "unpack-dst")
        with self.assertRaises(verify.BundleError):
            verify.verify_bundle(snap, nxt, unpack=True)
        self.assertFalse(os.path.lexists(nxt))
        with open(os.path.join(snap, "receipt.json"), encoding="utf-8") as fh:
            self.assertEqual(json.load(fh), rec)

    def test_unpack_rejects_link_parent_escape(self):
        hold = tempfile.TemporaryDirectory(prefix="wb-sentinel-")
        self.addCleanup(hold.cleanup)
        os.chmod(hold.name, 0o700)
        sentinel = os.path.join(hold.name, "sentinel")
        marker = b"sentinel-intact\n"
        with open(sentinel, "wb") as fh:
            fh.write(marker)

        def write(tf):
            tf.addfile(self._dot())
            sym = tarfile.TarInfo("./up")
            sym.type = tarfile.SYMTYPE
            sym.linkname = ".."
            tf.addfile(sym)
            hard = tarfile.TarInfo("./etc/proof")
            hard.type = tarfile.LNKTYPE
            hard.linkname = "../up/sentinel"
            tf.addfile(hard)

        self._ustar(write)
        self._fill()
        with self.assertRaises(verify.BundleError):
            verify.verify_bundle(self.source, self.target, unpack=True)
        self._assert_no_residue()
        with open(sentinel, "rb") as fh:
            self.assertEqual(fh.read(), marker)

    def test_unpack_pax_synthetic_distro_links(self):
        proof = b"pax-proof\n"
        tool = b"#!/bin/sh\necho tool\n"
        root = self._dot()
        root.pax_headers = {"mtime": "1700000000.0", "atime": "1700000000.5", "ctime": "1700000001.25"}

        def write(tf):
            tf.addfile(root)
            info = tarfile.TarInfo("./etc/proof")
            info.type = tarfile.REGTYPE
            info.mode = 0o644
            info.size = len(proof)
            tf.addfile(info, io.BytesIO(proof))
            exe = tarfile.TarInfo("usr/bin/tool")
            exe.type = tarfile.REGTYPE
            exe.mode = 0o755
            exe.size = len(tool)
            tf.addfile(exe, io.BytesIO(tool))
            bindir = tarfile.TarInfo("bin")
            bindir.type = tarfile.SYMTYPE
            bindir.linkname = "usr/bin"
            tf.addfile(bindir)
            etc_tool = tarfile.TarInfo("etc/tool")
            etc_tool.type = tarfile.SYMTYPE
            etc_tool.linkname = "/bin/tool"
            tf.addfile(etc_tool)
            hard = tarfile.TarInfo("usr/bin/hard")
            hard.type = tarfile.LNKTYPE
            hard.linkname = "usr/bin/tool"
            tf.addfile(hard)

        self._ustar(write, fmt=tarfile.PAX_FORMAT)
        self._fill()
        rec = verify.verify_bundle(self.source, self.target, unpack=True)
        self.assertIs(rec["extracted"], True)
        self.assertIs(rec["execution_accepted"], False)
        self.assertEqual(rec["tree_policy"], "workspace-rootfs-runtime-v1")
        root_fd = os.open(os.path.join(self.target, "rootfs"), os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
        try:
            self.assertEqual(rec["tree_sha256"], verify.tree_sha256(root_fd))
        finally:
            os.close(root_fd)
        base = os.path.join(self.target, "rootfs")
        proof_path = os.path.join(base, "etc", "proof")
        tool_path = os.path.join(base, "usr", "bin", "tool")
        hard_path = os.path.join(base, "usr", "bin", "hard")
        with open(proof_path, "rb") as fh:
            self.assertEqual(fh.read(), proof)
        with open(tool_path, "rb") as fh:
            self.assertEqual(fh.read(), tool)
        self.assertFalse(os.path.islink(proof_path))
        self.assertFalse(os.path.islink(tool_path))
        self.assertEqual(os.lstat(tool_path).st_mode & 0o777, 0o700)
        self.assertEqual(os.readlink(os.path.join(base, "bin")), "usr/bin")
        self.assertEqual(os.readlink(os.path.join(base, "etc", "tool")), "/bin/tool")
        self.assertEqual(os.lstat(tool_path).st_ino, os.lstat(hard_path).st_ino)
        self.assertEqual(os.lstat(tool_path).st_dev, os.lstat(hard_path).st_dev)

    def test_unpack_rejects_symlink_cycle(self):
        proof = b"before-cycle\n"

        def write(tf):
            tf.addfile(self._dot())
            info = tarfile.TarInfo("./etc/proof")
            info.type = tarfile.REGTYPE
            info.mode = 0o644
            info.size = len(proof)
            tf.addfile(info, io.BytesIO(proof))
            a = tarfile.TarInfo("./a")
            a.type = tarfile.SYMTYPE
            a.linkname = "b"
            tf.addfile(a)
            b = tarfile.TarInfo("./b")
            b.type = tarfile.SYMTYPE
            b.linkname = "a"
            tf.addfile(b)

        self._ustar(write)
        self._fill()
        with self.assertRaises(verify.BundleError):
            verify.verify_bundle(self.source, self.target, unpack=True)
        self._assert_no_residue()

    def test_concatenated_valid_signatures_rejected(self):
        manifest = self._manifest()
        first = self._sign(manifest, self.alpha)
        self._fill(signature=first + first)
        with self.assertRaises(verify.BundleError):
            verify.verify_bundle(self.source, self.target)
        self._assert_no_residue()


if __name__ == "__main__":
    unittest.main(verbosity=2)
