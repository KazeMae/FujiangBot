# PR 62 草稿

不要用 API 发布或更新这篇草稿。生产默认关闭，不合并不部署。

## 摘要

2026-11-09 11:33 用户要求更轻的实现。2026-11-09 12:57 用户取消沙箱内 apt 安装。工作区只读挂载宿主 `/usr`、`/etc`、`/var`，只有 owner 的 `/work` 可写。网络按范围与宿主共享。Landlock ABI≥6 在 exec bwrap 前限制抽象 unix 套接字和信号。apt/dpkg 在沙箱内自然失败，不改宿主。

## 已实现

owner 绑定、owner FD 锁、打开/重开、进程回收、`close_range`、清环境和 seccomp 仍在。生产门默认关。500k/90% 压缩和身份代码未改。bwrap 与命名空间 init 的回收上限都是 1 秒。

## 真实通过

原生 bwrap：bash/python 输出；`/work` 跨执行仍在。系统目录只读：不能写 `/usr/bin`、`/etc`、`/var/lib/dpkg`，不能追加 dpkg status；`apt-get install` 与 `dpkg` 非零退出；宿主 status 与 `/usr/bin` 不变。非 CLOEXEC 的宿主哨兵读写 FD 经 `/proc/self/fd` 与 `/proc/1/fd` 的真实读写不可用。`/proc/*/environ` 无宿主环境标记，失败时只报告命中次数。指向宿主哨兵或其他 owner 的符号链接失败。宿主抽象 unix 套接字连接为 `EPERM`。客人 `/proc/self/fd` 与 `/proc/1/fd` 里 fd≥3 的目录 fd 为 0：owner 根目录 fd 不再传给 bwrap（此前 bwrap 不会关闭未消费的 fd，会把它留给客人）。

CI 历史：43e2b9b 的运行 42559792547 里 overlay 时代的 Landlock、FD、apt 测试曾通过，但 `init reap exceeded bound` 失败。当时实测是 overlay 卸载 sync 宿主文件系统。d6c1d6c 曾把 init 上限拆到 30 秒，运行 42561746655 通过。overlay 后来已删除，init 回收回到 1 秒。https://github.com/KazeMae/FujiangBot/actions/runs/42561746655

## 模拟

Landlock ABI&lt;6 失败关闭。

## 未验收

在线 QQ。多 UID。CI 内核的 Landlock ABI。

## 已取消

沙箱内 apt/dpkg 安装。overlay、目录骨架、host-stamp、30 秒 init 回收上限。

## 已删模块

`workspace_runtime`、`workspace_runtime_tests`、`workspace_apt`、`workspace_resolv`、`workspace_resolv_gc_tests`、`workspace_resolv_reclaim`、`workspace_seed`、`workspace_seed_copy`、`workspace_seed_tests`、`workspace_overlay`。私人 root、链接物化、DNS/resolver 拷贝与回收、私人 apt 源和钥匙环，以及对应 CI 步骤一并去掉。
