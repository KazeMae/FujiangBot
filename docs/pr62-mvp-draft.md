# PR 62 草稿

不要用 API 发布或更新这篇草稿。生产默认关闭，不合并不部署。

## 摘要

2026-11-09 11:33 用户要求更轻的实现。工作区不再拷贝私人 root，改为宿主 `/usr`、`/etc`、`/var` 的 bwrap overlay。owner 只保留空目录骨架、`/work`、apt 锁空文件、首次拷入的 dpkg `status`/`available`，以及私有的 `/var/log`、`/var/cache/apt`、`lists/partial`。网络按范围与宿主共享，不隔离。Landlock ABI≥6 在 exec bwrap 前限制抽象 unix 套接字和信号。

## 已实现

owner 绑定、owner FD 锁、打开/重开、进程回收、`close_range`、清环境和 seccomp 仍在。生产门默认关。500k/90% 压缩和身份代码未改。

## 真实通过

原生 bwrap：bash/python 输出；`/work` 跨执行仍在；离线本地 `.deb` newly installed，重开仍在；探针中的 apt remove/reinstall。宿主 `/usr` 与宿主 dpkg 不变，其他 owner 看不见。非 CLOEXEC 的宿主哨兵读写 FD 经 `/proc/self/fd` 与 `/proc/1/fd` 的真实读写不可用。`/proc/*/environ` 无宿主环境标记。指向宿主哨兵或其他 owner 的符号链接失败。宿主抽象 unix 套接字连接为 `EPERM`。upper 里客人种下的外指符号链接不会让宿主侧 `prepare` 写出 owner。

## 模拟

骨架谓词。Landlock ABI&lt;6 失败关闭。非普通 dpkg status 拒绝。

## 未验收

远程 apt update/install：宿主 DNS 当前失败，不重试。在线 QQ。dpkg 硬链接备份的宿主包升级。多 UID/服务包。CI 上的 Landlock ABI。

## 已删模块

`workspace_runtime`、`workspace_runtime_tests`、`workspace_apt`、`workspace_resolv`、`workspace_resolv_gc_tests`、`workspace_resolv_reclaim`、`workspace_seed`、`workspace_seed_copy`、`workspace_seed_tests`。私人 root、链接物化、DNS/resolver 拷贝与回收、私人 apt 源和钥匙环，以及对应 CI 步骤一并去掉。
