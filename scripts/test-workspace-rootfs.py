#!/usr/bin/env python3

import hashlib
import shutil
import subprocess
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / "scripts" / "prepare-workspace-rootfs.sh"
FP = "843938DF228D22F7B3742BC0D94AA3F0EFE21092"
NAME = "ubuntu-base-24.04.1-base-amd64.tar.gz"
PAYLOAD = b"not-an-official-rootfs"
DIGEST = hashlib.sha256(PAYLOAD).hexdigest()
SUB = "A" * 64


def validsig(sign, primary):
    fields = ["[GNUPG:]", "VALIDSIG", sign, "1", "1", "0", "4", "0", "1", "8", "0"]
    if primary is not None:
        fields.append(primary)
    return " ".join(fields)


def write_fakes(bin_dir: Path, sums: str, status: str, exit_code: int):
    bin_dir.mkdir()
    curl = bin_dir / "curl"
    curl.write_text(
        """#!/bin/sh
out=
url=
while [ $# -gt 0 ]; do
  case $1 in
    -o) out=$2; shift 2 ;;
    --proto|--proto-redir|--connect-timeout|--max-time) shift 2 ;;
    -f|-s|-S|-L|-fsSL) shift ;;
    *) url=$1; shift ;;
  esac
done
case $url in
  *SHA256SUMS.gpg) printf gpg > "$out" ;;
  *SHA256SUMS) cat "$SUMS_FILE" > "$out" ;;
  *ubuntu-archive-keyring.gpg) printf key > "$out" ;;
  *) cat "$TAR_FILE" > "$out" ;;
esac
""",
        encoding="utf-8",
    )
    curl.chmod(0o700)
    gpg = bin_dir / "gpg"
    gpg.write_text(
        f"""#!/bin/sh
case " $* " in
  *" --import "*) exit 0 ;;
esac
printf '%s\\n' "$GPG_STATUS"
exit {exit_code}
""",
        encoding="utf-8",
    )
    gpg.chmod(0o700)
    (bin_dir / "sums").write_text(sums, encoding="utf-8")
    (bin_dir / "tar").write_bytes(PAYLOAD)
    return {
        "PATH": f"{bin_dir}:/usr/bin:/bin",
        "LC_ALL": "C",
        "SUMS_FILE": str(bin_dir / "sums"),
        "TAR_FILE": str(bin_dir / "tar"),
        "GPG_STATUS": status,
    }


def run(env, dest: Path):
    return subprocess.run(
        ["sh", str(SCRIPT), str(dest)],
        env=env,
        text=True,
        capture_output=True,
        timeout=10,
    )


def expect(ok, label, proc):
    if ok != (proc.returncode == 0):
        raise SystemExit(f"{label}\n{proc.returncode}\n{proc.stdout}\n{proc.stderr}")


def main():
    work = Path(tempfile.mkdtemp(prefix="fujiang-rootfs-stub-"))
    try:
        cases = [
            ("plain", f"{DIGEST}  {NAME}\n", validsig(FP, FP), 0, True),
            ("star", f"{DIGEST} *{NAME}\n", validsig(SUB, FP), 0, True),
            ("short", f"{'ab' * 31}  {NAME}\n", validsig(FP, FP), 0, False),
            ("dup", f"{DIGEST}  {NAME}\n{DIGEST}  {NAME}\n", validsig(FP, FP), 0, False),
            ("mismatch", f"{'b' * 64}  {NAME}\n", validsig(FP, FP), 0, False),
            ("bad-primary", f"{DIGEST}  {NAME}\n", validsig(SUB, "B" * 64), 0, False),
            ("gpg-exit", f"{DIGEST}  {NAME}\n", validsig(SUB, FP), 1, False),
            ("no-sig", f"{DIGEST}  {NAME}\n", "[GNUPG:] NEWSIG", 0, False),
            ("primary-only", f"{DIGEST}  {NAME}\n", validsig(FP, None), 0, True),
        ]
        for label, sums, status, code, ok in cases:
            box = work / label
            box.mkdir()
            env = write_fakes(box / "bin", sums, status, code)
            dest = box / "fresh"
            proc = run(env, dest)
            expect(ok, label, proc)
            if ok and not (dest / NAME).is_file():
                raise SystemExit(f"{label} did not keep the tar")
        sentinel = work / "sentinel"
        sentinel.write_text("keep", encoding="utf-8")
        occupied = work / "occupied"
        occupied.mkdir()
        (occupied / "marker").write_text("keep", encoding="utf-8")
        env = write_fakes(work / "bin-exist", f"{DIGEST}  {NAME}\n", validsig(FP, FP), 0)
        proc = run(env, occupied)
        expect(False, "exists", proc)
        if (occupied / "marker").read_text(encoding="utf-8") != "keep":
            raise SystemExit("existing directory was modified")
        link = work / "link"
        link.symlink_to(sentinel)
        before = sentinel.read_text(encoding="utf-8")
        proc = run(env, link)
        expect(False, "symlink", proc)
        if sentinel.read_text(encoding="utf-8") != before or not link.is_symlink():
            raise SystemExit("symlink sentinel changed")
    finally:
        shutil.rmtree(work)
    print("workspace rootfs stub branches passed")


if __name__ == "__main__":
    main()
