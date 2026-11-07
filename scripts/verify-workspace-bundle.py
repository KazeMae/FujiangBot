import datetime
import hashlib
import json
import os
import re
import resource
import shutil
import stat
import subprocess
import sys
import tempfile

OFFICIAL_PRIMARY = "843938DF228D22F7B3742BC0D94AA3F0EFE21092"

class BundleError(Exception):
    pass

def parse_manifest(data: bytes):
    if len(data) > 1048576:
        raise BundleError()
    for b in data:
        if (b < 32 and b != 10) or b > 126:
            raise BundleError()
    text = data.decode("ascii")
    line_re = re.compile(r"([0-9A-Fa-f]{64}) [ *]([A-Za-z0-9][A-Za-z0-9._-]*)")
    cand_re = re.compile(r"ubuntu-base-24\.04(?:\.[0-9]{1,3})?-base-amd64\.tar\.gz")
    seen = set()
    candidates = []
    for line in text.splitlines():
        if line == "":
            continue
        m = line_re.fullmatch(line)
        if m is None:
            raise BundleError()
        digest = m.group(1)
        name = m.group(2)
        if name == "." or name == "..":
            raise BundleError()
        if name in seen:
            raise BundleError()
        seen.add(name)
        if cand_re.fullmatch(name):
            candidates.append((name, digest.lower()))
    if len(candidates) != 1:
        raise BundleError()
    return candidates[0]

def parse_status(data: bytes, returncode: int) -> str:
    if returncode != 0:
        raise BundleError()
    if len(data) > 1048576:
        raise BundleError()
    for b in data:
        if b != 10 and (b < 32 or b > 126):
            raise BundleError()
    text = data.decode("ascii")
    allowed = {
        "NEWSIG",
        "KEY_CONSIDERED",
        "SIG_ID",
        "GOODSIG",
        "TRUST_UNDEFINED",
        "TRUST_NEVER",
        "TRUST_MARGINAL",
        "TRUST_FULLY",
        "TRUST_ULTIMATE",
        "VERIFICATION_COMPLIANCE_MODE",
    }
    found = 0
    for line in text.splitlines():
        if line == "":
            continue
        parts = line.split()
        if len(parts) < 2 or parts[0] != "[GNUPG:]":
            raise BundleError()
        tag = parts[1]
        if tag in allowed:
            continue
        if tag != "VALIDSIG" or (len(parts) != 11 and len(parts) != 12):
            raise BundleError()
        fingerprint = parts[2]
        if re.fullmatch(r"[0-9A-Fa-f]{40}", fingerprint) is None:
            raise BundleError()
        if re.fullmatch(r"[0-9]{4}-[0-9]{2}-[0-9]{2}", parts[3]) is None:
            raise BundleError()
        try:
            datetime.date.fromisoformat(parts[3])
        except ValueError:
            raise BundleError() from None
        if not parts[4].isdigit() or not parts[5].isdigit():
            raise BundleError()
        if parts[6] != "4" or parts[7] != "0" or not parts[8].isdigit():
            raise BundleError()
        if parts[9] not in ("8", "9", "10", "11") or parts[10] != "00":
            raise BundleError()
        if len(parts) == 12:
            primary = parts[11]
            if re.fullmatch(r"[0-9A-Fa-f]{40}", primary) is None:
                raise BundleError()
        else:
            primary = fingerprint
        if primary != OFFICIAL_PRIMARY:
            raise BundleError()
        found += 1
    if found != 1:
        raise BundleError()
    return OFFICIAL_PRIMARY

def open_dir(path):
    if type(path) is not str or path == "" or path[0] != "/":
        raise BundleError()
    parts = path.split("/")
    if parts[0] != "":
        raise BundleError()
    comps = parts[1:]
    for comp in comps:
        if comp == "" or comp == "." or comp == "..":
            raise BundleError()
    flags = os.O_DIRECTORY | os.O_NOFOLLOW | os.O_CLOEXEC
    fd = None
    try:
        fd = os.open("/", flags)
    except OSError:
        raise BundleError()
    for comp in comps:
        nxt = None
        try:
            nxt = os.open(comp, flags, dir_fd=fd)
        except OSError:
            os.close(fd)
            raise BundleError()
        os.close(fd)
        fd = nxt
    return fd

def copy_file(source_fd, name, dest_fd, limit):
    if type(name) is not str or re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9._-]*", name) is None or name == "." or name == "..":
        raise BundleError()
    if type(limit) is not int or limit < 0:
        raise BundleError()
    src = None
    dst = None
    try:
        try:
            src = os.open(name, os.O_RDONLY | os.O_NONBLOCK | os.O_NOFOLLOW | os.O_CLOEXEC, dir_fd=source_fd)
        except OSError:
            raise BundleError()
        st = os.fstat(src)
        if not stat.S_ISREG(st.st_mode) or st.st_nlink != 1 or st.st_size > limit:
            raise BundleError()
        try:
            dst = os.open(
                name,
                os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW | os.O_CLOEXEC,
                0o600,
                dir_fd=dest_fd,
            )
        except OSError:
            raise BundleError()
        digest = hashlib.sha256()
        total = 0
        while True:
            chunk = os.read(src, 1048576)
            if chunk == b"":
                break
            total += len(chunk)
            if total > limit:
                raise BundleError()
            digest.update(chunk)
            view = memoryview(chunk)
            off = 0
            while off < len(view):
                n = os.write(dst, view[off:])
                if n <= 0:
                    raise BundleError()
                off += n
        return digest.hexdigest(), total
    finally:
        if src is not None:
            os.close(src)
        if dst is not None:
            os.close(dst)

def read_private(dest_fd, name, limit):
    if type(name) is not str or re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9._-]*", name) is None or name == "." or name == "..":
        raise BundleError()
    if type(limit) is not int or limit < 0:
        raise BundleError()
    fd = None
    try:
        try:
            fd = os.open(name, os.O_RDONLY | os.O_NONBLOCK | os.O_NOFOLLOW | os.O_CLOEXEC, dir_fd=dest_fd)
        except OSError:
            raise BundleError()
        st = os.fstat(fd)
        if not stat.S_ISREG(st.st_mode) or st.st_nlink != 1 or st.st_size > limit:
            raise BundleError()
        parts = []
        total = 0
        while True:
            chunk = os.read(fd, 1048576)
            if chunk == b"":
                break
            total += len(chunk)
            if total > limit:
                raise BundleError()
            parts.append(chunk)
        return b"".join(parts)
    finally:
        if fd is not None:
            os.close(fd)

def verify_signature(snapshot_fd):
    if not os.path.isfile("/usr/bin/gpgv") or not os.access("/usr/bin/gpgv", os.X_OK):
        raise BundleError("gpgv unavailable")
    out = None
    try:
        os.mkdir("gnupg", 0o700, dir_fd=snapshot_fd)
        root = "/proc/self/fd/%d" % snapshot_fd
        out = tempfile.TemporaryFile(dir=root + "/gnupg")

        def limits():
            resource.setrlimit(resource.RLIMIT_CPU, (10, 11))
            resource.setrlimit(resource.RLIMIT_AS, (256 * 1024 * 1024, 256 * 1024 * 1024))
            resource.setrlimit(resource.RLIMIT_FSIZE, (1048576, 1048576))
            resource.setrlimit(resource.RLIMIT_CORE, (0, 0))
            resource.setrlimit(resource.RLIMIT_NOFILE, (64, 64))

        proc = subprocess.run(
            [
                "/usr/bin/gpgv",
                "--homedir",
                root + "/gnupg",
                "--keyring",
                root + "/ubuntu-archive-keyring.gpg",
                "--status-fd",
                "1",
                root + "/SHA256SUMS.gpg",
                root + "/SHA256SUMS",
            ],
            stdin=subprocess.DEVNULL,
            stdout=out,
            stderr=subprocess.DEVNULL,
            env={
                "PATH": "/usr/bin:/bin",
                "HOME": root + "/gnupg",
                "GNUPGHOME": root + "/gnupg",
                "LANG": "C",
            },
            timeout=15,
            pass_fds=(snapshot_fd,),
            close_fds=True,
            preexec_fn=limits,
        )
        out.seek(0)
        data = out.read(1048576 + 1)
    except Exception:
        raise BundleError() from None
    finally:
        if out is not None:
            out.close()
    return parse_status(data, proc.returncode)

def verify_bundle(source, target):
    if os.geteuid() == 0:
        raise BundleError()
    if type(target) is not str or target == "" or target[0] != "/" or target.endswith("/"):
        raise BundleError()
    if os.path.normpath(target) != target:
        raise BundleError()
    parent = os.path.dirname(target)
    base = os.path.basename(target)
    if re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9._-]*", base) is None or base == "." or base == "..":
        raise BundleError()
    source_fd = None
    parent_fd = None
    snapshot_fd = None
    created = False
    try:
        source_fd = open_dir(source)
        parent_fd = open_dir(parent)
        st_parent = os.fstat(parent_fd)
        if st_parent.st_uid != os.geteuid() or (st_parent.st_mode & 0o077) != 0:
            raise BundleError()
        src_st = os.fstat(source_fd)
        src_key = (src_st.st_dev, src_st.st_ino)
        flags = os.O_DIRECTORY | os.O_NOFOLLOW | os.O_CLOEXEC
        cur = os.dup(parent_fd)
        try:
            seen = set()
            while True:
                st = os.fstat(cur)
                key = (st.st_dev, st.st_ino)
                if key == src_key or key in seen:
                    raise BundleError()
                seen.add(key)
                nxt = os.open("..", flags, dir_fd=cur)
                try:
                    up = os.fstat(nxt)
                    up_key = (up.st_dev, up.st_ino)
                except Exception:
                    os.close(nxt)
                    raise
                if up_key == key:
                    os.close(nxt)
                    break
                prev = cur
                cur = nxt
                os.close(prev)
        finally:
            os.close(cur)
        os.mkdir(base, 0o700, dir_fd=parent_fd)
        created = True
        snapshot_fd = os.open(base, flags, dir_fd=parent_fd)
        copy_file(source_fd, "SHA256SUMS", snapshot_fd, 1048576)
        copy_file(source_fd, "SHA256SUMS.gpg", snapshot_fd, 65536)
        copy_file(source_fd, "ubuntu-archive-keyring.gpg", snapshot_fd, 8388608)
        primary = verify_signature(snapshot_fd)
        archive_name, digest = parse_manifest(read_private(snapshot_fd, "SHA256SUMS", 1048576))
        got, total = copy_file(source_fd, archive_name, snapshot_fd, 536870912)
        if got != digest:
            raise BundleError()
        receipt = {
            "archive": archive_name,
            "sha256": got,
            "bytes": total,
            "primary": primary,
            "extracted": False,
            "execution_accepted": False,
        }
        payload = (json.dumps(receipt, separators=(",", ":")) + "\n").encode("ascii")
        rfd = None
        try:
            rfd = os.open(
                "receipt.json",
                os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW | os.O_CLOEXEC,
                0o600,
                dir_fd=snapshot_fd,
            )
            view = memoryview(payload)
            off = 0
            while off < len(view):
                n = os.write(rfd, view[off:])
                if n <= 0:
                    raise BundleError()
                off += n
        finally:
            if rfd is not None:
                os.close(rfd)
        created = False
        return receipt
    finally:
        try:
            try:
                if snapshot_fd is not None:
                    os.close(snapshot_fd)
                    snapshot_fd = None
            finally:
                if created and parent_fd is not None:
                    shutil.rmtree(base, dir_fd=parent_fd)
        finally:
            try:
                if parent_fd is not None:
                    os.close(parent_fd)
            finally:
                if source_fd is not None:
                    os.close(source_fd)

def main():
    if len(sys.argv) != 3:
        try:
            os.write(2, b"error\n")
        except Exception:
            pass
        return 1
    try:
        receipt = verify_bundle(sys.argv[1], sys.argv[2])
        payload = (json.dumps(receipt, separators=(",", ":")) + "\n").encode("ascii")
        view = memoryview(payload)
        off = 0
        while off < len(view):
            n = os.write(1, view[off:])
            if n <= 0:
                raise OSError()
            off += n
    except Exception:
        try:
            os.write(2, b"error\n")
        except Exception:
            pass
        return 1
    return 0

if __name__ == "__main__":
    sys.exit(main())
