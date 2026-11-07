import datetime
import fcntl
import gzip
import hashlib
import json
import os
import re
import resource
import shutil
import signal
import stat
import subprocess
import sys
import tarfile
import tempfile
import time

MAX_EXPANDED = 1024 * 1024 * 1024
MAX_RATIO = 200
MAX_ENTRIES = 50000
MAX_FILE = 256 * 1024 * 1024
UNPACK_WALL_SECONDS = 15
_DIR_FLAGS = os.O_DIRECTORY | os.O_NOFOLLOW | os.O_CLOEXEC
_FILE_FLAGS = os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW | os.O_CLOEXEC
_COMP_RE = re.compile(r"[A-Za-z0-9+@=._-]+")

OFFICIAL_PRIMARY = "843938DF228D22F7B3742BC0D94AA3F0EFE21092"

class BundleError(Exception):
    pass

def _tar_path(member):
    if member.type not in (tarfile.REGTYPE, tarfile.AREGTYPE, tarfile.DIRTYPE):
        raise BundleError()
    if member.pax_headers or member.sparse is not None or member.linkname:
        raise BundleError()
    if member.mode & 0o6000:
        raise BundleError()
    size = member.size
    if type(size) is not int or size < 0 or size > MAX_FILE:
        raise BundleError()
    name = member.name
    if type(name) is not str or len(name) > 4096 or "\\" in name or name.startswith("/"):
        raise BundleError()
    if member.type == tarfile.DIRTYPE:
        if size != 0:
            raise BundleError()
        if name in (".", "./"):
            return ()
        if name.endswith("/"):
            name = name[:-1]
    if name.startswith("./"):
        name = name[2:]
    parts = name.split("/")
    for part in parts:
        if len(part) > 255 or part in (".", "..") or _COMP_RE.fullmatch(part) is None:
            raise BundleError()
    return tuple(parts)

def _tar_dir(parent_fd, name, path, created):
    try:
        return os.open(name, _DIR_FLAGS, dir_fd=parent_fd)
    except FileNotFoundError:
        if len(created) >= MAX_ENTRIES:
            raise BundleError()
        os.mkdir(name, 0o700, dir_fd=parent_fd)
        created.add(path)
        return os.open(name, _DIR_FLAGS, dir_fd=parent_fd)

def _tar_write(root_fd, member, parts, created, read):
    if parts == ():
        return
    fd = os.dup(root_fd)
    try:
        for i, name in enumerate(parts[:-1]):
            nxt = _tar_dir(fd, name, parts[: i + 1], created)
            os.close(fd)
            fd = nxt
        if member.type == tarfile.DIRTYPE:
            dfd = _tar_dir(fd, parts[-1], parts, created)
            try:
                return
            finally:
                os.close(dfd)
        if len(created) >= MAX_ENTRIES:
            raise BundleError()
        ffd = os.open(parts[-1], _FILE_FLAGS, 0o600, dir_fd=fd)
        created.add(parts)
        try:
            left = member.size
            while left:
                want = min(65536, left)
                chunk = read(want)
                n = len(chunk)
                if n == 0 or n > want:
                    raise BundleError()
                off = 0
                while off < n:
                    w = os.write(ffd, chunk[off:])
                    if w <= 0:
                        raise BundleError()
                    off += w
                left -= n
            os.fchmod(ffd, 0o700 if member.mode & 0o111 else 0o600)
        finally:
            os.close(ffd)
        pad = (-member.size) % 512
        if pad:
            chunk = read(pad)
            if len(chunk) != pad or chunk != b"\x00" * pad:
                raise BundleError()
    finally:
        os.close(fd)

def _unpack_stream(archive_fd, root_fd, compressed_size):
    raw = os.fdopen(os.dup(archive_fd), "rb")
    try:
        gz = gzip.GzipFile(fileobj=raw)
        try:
            expanded = 0
            limit = compressed_size * MAX_RATIO

            def read(n):
                nonlocal expanded
                if type(n) is not int or n < 0 or n > 65536:
                    raise BundleError()
                chunk = gz.read(n)
                expanded += len(chunk)
                if expanded > MAX_EXPANDED or expanded > limit:
                    raise BundleError()
                return chunk

            seen = set()
            created = set()
            count = 0
            while True:
                header = read(512)
                if len(header) != 512:
                    raise BundleError()
                if header == b"\x00" * 512:
                    second = read(512)
                    if len(second) != 512 or second != b"\x00" * 512:
                        raise BundleError()
                    while True:
                        tail = read(65536)
                        if tail == b"":
                            return
                        if tail != b"\x00" * len(tail):
                            raise BundleError()
                member = tarfile.TarInfo.frombuf(header, "ascii", "strict")
                parts = _tar_path(member)
                if parts in seen:
                    raise BundleError()
                seen.add(parts)
                count += 1
                if count > MAX_ENTRIES:
                    raise BundleError()
                _tar_write(root_fd, member, parts, created, read)
        finally:
            gz.close()
    finally:
        raw.close()

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

def _sealed_archive(snapshot_fd, name, digest, compressed_size):
    cap = 512 * 1024 * 1024
    if type(compressed_size) is not int or compressed_size <= 0 or compressed_size > cap:
        raise BundleError()
    src = None
    dst = None
    try:
        src = os.open(
            name,
            os.O_RDONLY | os.O_NONBLOCK | os.O_NOFOLLOW | os.O_CLOEXEC,
            dir_fd=snapshot_fd,
        )
        st = os.fstat(src)
        if not stat.S_ISREG(st.st_mode) or st.st_nlink != 1 or st.st_size != compressed_size:
            raise BundleError()
        dst = os.memfd_create("a", os.MFD_CLOEXEC | os.MFD_ALLOW_SEALING)
        left = compressed_size
        step = 1024 * 1024
        while left:
            block = os.read(src, step if left > step else left)
            if not block:
                raise BundleError()
            view = memoryview(block)
            while len(view):
                n = os.write(dst, view)
                if n < 1:
                    raise BundleError()
                view = view[n:]
            left -= len(block)
        if os.fstat(dst).st_size != compressed_size:
            raise BundleError()
        fcntl.fcntl(
            dst,
            fcntl.F_ADD_SEALS,
            fcntl.F_SEAL_WRITE | fcntl.F_SEAL_GROW | fcntl.F_SEAL_SHRINK | fcntl.F_SEAL_SEAL,
        )
        os.lseek(dst, 0, os.SEEK_SET)
        hashed = hashlib.sha256()
        left = compressed_size
        while left:
            block = os.read(dst, step if left > step else left)
            if not block:
                raise BundleError()
            hashed.update(block)
            left -= len(block)
        if hashed.hexdigest() != digest:
            raise BundleError()
        os.lseek(dst, 0, os.SEEK_SET)
        out = dst
        dst = None
        return out
    finally:
        if src is not None:
            os.close(src)
        if dst is not None:
            os.close(dst)

def _unpack_verified(snapshot_fd, name, digest, compressed_size):
    archive_fd = None
    stage_fd = None
    made = False
    pid = None
    reaped = False
    status = None
    timed_out = False
    try:
        archive_fd = _sealed_archive(snapshot_fd, name, digest, compressed_size)
        try:
            os.stat("rootfs", dir_fd=snapshot_fd, follow_symlinks=False)
        except FileNotFoundError:
            pass
        else:
            raise BundleError()
        os.mkdir(".rootfs-partial", 0o700, dir_fd=snapshot_fd)
        made = True
        stage_fd = os.open(".rootfs-partial", _DIR_FLAGS, dir_fd=snapshot_fd)
        pid = os.fork()
        if pid == 0:
            try:
                resource.setrlimit(resource.RLIMIT_CPU, (10, 11))
                resource.setrlimit(resource.RLIMIT_AS, (512 * 1024 * 1024, 512 * 1024 * 1024))
                resource.setrlimit(resource.RLIMIT_CORE, (0, 0))
                resource.setrlimit(resource.RLIMIT_NOFILE, (64, 64))
                resource.setrlimit(resource.RLIMIT_FSIZE, (256 * 1024 * 1024, 256 * 1024 * 1024))
                os.umask(0o077)
                _unpack_stream(archive_fd, stage_fd, compressed_size)
            except BaseException:
                os._exit(1)
            os._exit(0)
        deadline = time.monotonic() + UNPACK_WALL_SECONDS
        while True:
            got, status = os.waitpid(pid, os.WNOHANG)
            if got == pid:
                reaped = True
                break
            if time.monotonic() >= deadline:
                timed_out = True
                os.kill(pid, signal.SIGKILL)
                got, status = os.waitpid(pid, 0)
                reaped = True
                break
            time.sleep(0.05)
        if timed_out or not reaped or not os.WIFEXITED(status) or os.WEXITSTATUS(status) != 0:
            raise BundleError()
        os.rename(".rootfs-partial", "rootfs", src_dir_fd=snapshot_fd, dst_dir_fd=snapshot_fd)
        made = False
    finally:
        try:
            if pid is not None and not reaped:
                try:
                    os.kill(pid, signal.SIGKILL)
                except OSError:
                    pass
                try:
                    os.waitpid(pid, 0)
                except ChildProcessError:
                    pass
        finally:
            try:
                if stage_fd is not None:
                    os.close(stage_fd)
            finally:
                try:
                    if archive_fd is not None:
                        os.close(archive_fd)
                finally:
                    if made:
                        shutil.rmtree(".rootfs-partial", dir_fd=snapshot_fd)

def verify_bundle(source, target, unpack=False):
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
        if unpack:
            _unpack_verified(snapshot_fd, archive_name, digest, total)
        receipt = {
            "archive": archive_name,
            "sha256": got,
            "bytes": total,
            "primary": primary,
            "extracted": bool(unpack),
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
    unpack = False
    if len(sys.argv) == 4 and sys.argv[1] == "--unpack":
        source = sys.argv[2]
        target = sys.argv[3]
        unpack = True
    elif len(sys.argv) == 3:
        source = sys.argv[1]
        target = sys.argv[2]
    else:
        try:
            os.write(2, b"error\n")
        except Exception:
            pass
        return 1
    try:
        receipt = verify_bundle(source, target, unpack)
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
