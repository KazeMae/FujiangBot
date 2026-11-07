#!/bin/sh
set -eu
root=$(CDPATH= cd -- "$(dirname "$0")/.." && pwd)
spec="$root/spec/tla"
if ! command -v java >/dev/null 2>&1; then
  echo "需要 Java 11 或更高版本。Debian 或 Ubuntu 可执行: sudo apt-get install openjdk-21-jre-headless" >&2
  exit 2
fi
jar="${TLA_TOOLS_JAR:-${XDG_CACHE_HOME:-$HOME/.cache}/fujiang/tla2tools.jar}"
if [ ! -f "$jar" ]; then
  if ! command -v curl >/dev/null 2>&1; then
    echo "需要 curl 来下载 tla2tools.jar，或设置 TLA_TOOLS_JAR" >&2
    exit 2
  fi
  mkdir -p "$(dirname "$jar")"
  url="https://github.com/tlaplus/tlaplus/releases/download/v1.7.4/tla2tools.jar"
  echo "下载 $url"
  curl -fsSL -o "$jar.partial" "$url"
  mv "$jar.partial" "$jar"
fi
sum=$(sha1sum "$jar" | awk '{print $1}')
expect=bee4a54f3ee3d4afc347c3240ec2d9e93b075104
if [ "$sum" != "$expect" ]; then
  echo "tla2tools.jar 的 sha1 是 $sum，不是 1.7.4 的 $expect" >&2
  exit 2
fi
run_model() {
  name=$1
  echo "检查 $name"
  java -XX:+UseParallelGC -cp "$jar" tlc2.TLC \
    -workers 1 \
    -deadlock \
    -config "$spec/$name.cfg" \
    "$spec/$name.tla"
}

run_model ToolLoop
run_model Session
run_model Workspace
run_model Runner
run_model Bundle
