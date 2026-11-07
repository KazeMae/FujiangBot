#!/bin/sh
set -eu
umask 077

usage() {
  echo "用法: $0 <新建的绝对任务目录>" >&2
  echo "只下载并核对 Ubuntu Base 24.04 的公开 SHA256SUMS。不下载成功过，不解包，也没有执行后端。" >&2
  exit 2
}

[ $# -eq 1 ] || usage
dest=$1
case $dest in
  /*) ;;
  *) echo "只接受一个绝对路径" >&2; exit 2 ;;
esac
if [ "$(id -u)" -eq 0 ]; then
  echo "不要用 uid 0 运行" >&2
  exit 2
fi
parent=$(dirname "$dest")
[ -d "$parent" ] || { echo "父目录不存在" >&2; exit 2; }
parent=$(cd "$parent" && pwd -P)
base=$(basename "$dest")
case $base in
  .|..|"") echo "目录名无效" >&2; exit 2 ;;
esac
dest="$parent/$base"
if [ -e "$dest" ] || [ -L "$dest" ]; then
  echo "目标已存在，拒绝覆盖" >&2
  exit 2
fi
mkdir "$dest"

base_url=https://cdimage.ubuntu.com/ubuntu-base/releases/24.04/release
fp=843938DF228D22F7B3742BC0D94AA3F0EFE21092
key_url=https://archive.ubuntu.com/ubuntu/project/ubuntu-archive-keyring.gpg
curl_get() {
  curl --proto '=https' --proto-redir '=https' --connect-timeout 10 --max-time 60 -fsSL -o "$2" "$1"
}
curl_get "$base_url/SHA256SUMS" "$dest/SHA256SUMS"
curl_get "$base_url/SHA256SUMS.gpg" "$dest/SHA256SUMS.gpg"
curl_get "$key_url" "$dest/ubuntu-archive-keyring.gpg"
export GNUPGHOME="$dest/gnupg"
mkdir "$GNUPGHOME"
chmod 700 "$GNUPGHOME"
gpg --no-options --batch --homedir "$GNUPGHOME" --import "$dest/ubuntu-archive-keyring.gpg"
if status=$(gpg --no-options --batch --homedir "$GNUPGHOME" --status-fd 1 --verify "$dest/SHA256SUMS.gpg" "$dest/SHA256SUMS" 2>/dev/null); then
  printf '%s\n' "$status" | awk -v fp="$fp" '
    $1 == "[GNUPG:]" && $2 == "VALIDSIG" {
      found = 1
      sign = $3
      primary = $12
      if (primary == fp || (sign == fp && (primary == "" || primary == fp))) ok = 1
    }
    END { exit !(found && ok) }
  ' || {
    echo "VALIDSIG 的主指纹不是 $fp" >&2
    exit 2
  }
else
  echo "gpg verify 失败" >&2
  exit 2
fi

set --
while IFS= read -r line; do
  case $line in
    ''|'#'*) continue ;;
  esac
  sum=${line%%[[:space:]]*}
  name=${line#"$sum"}
  while :; do
    case $name in
      [[:space:]]*) name=${name#?} ;;
      *) break ;;
    esac
  done
  name=${name#\*}
  case $name in
    ubuntu-base-24.04-base-amd64.tar.gz) ;;
    ubuntu-base-24.04.[0-9]-base-amd64.tar.gz) ;;
    ubuntu-base-24.04.[0-9][0-9]-base-amd64.tar.gz) ;;
    ubuntu-base-24.04.[0-9][0-9][0-9]-base-amd64.tar.gz) ;;
    *) continue ;;
  esac
  case $name in
    *[!A-Za-z0-9._-]*) echo "清单文件名不纯" >&2; exit 2 ;;
  esac
  if [ "${#sum}" -ne 64 ]; then
    echo "摘要不是 64 位十六进制" >&2
    exit 2
  fi
  case $sum in
    *[!0-9a-fA-F]*|"") echo "摘要不是 64 位十六进制" >&2; exit 2 ;;
  esac
  set -- "$@" "$sum" "$name"
done < "$dest/SHA256SUMS"
[ $# -eq 2 ] || { echo "amd64 24.04 条目不是正好一条" >&2; exit 2; }
sum=$1
name=$2
curl_get "$base_url/$name" "$dest/$name"
got=$(sha256sum "$dest/$name" | awk '{print $1}')
[ "$got" = "$sum" ] || { echo "SHA256 是 $got，清单是 $sum" >&2; exit 2; }
echo "已核对 $name。下载在这台机器上尚未作为成功验收；没有解包，也没有执行后端。"
