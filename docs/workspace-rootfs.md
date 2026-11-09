# 工作区：宿主只读系统目录

默认关闭。只有 `FUJIANG_WORKSPACE_MVP` 指向绝对的宿主目录时才打开。绑定仍是每次调用按 QQ 解析的个人或群，生产不因此打开。

客人用 bwrap 只读挂载宿主 `/usr`、`/etc`、`/var`。唯一可写的持久目录是 owner 的 `/work`。`/tmp`、`/run` 是 tmpfs。宿主的 `/var/log`、`/var/tmp`、`/var/cache` 若存在则用 tmpfs 盖住。`/etc/apt/auth.conf.d` 若存在则用 tmpfs 盖住；`/etc/apt/auth.conf` 若存在则绑到 `/dev/null`。Landlock ABI 至少 6，在 exec bwrap 之前限制抽象 unix 套接字和信号；更低就拒绝，错误是 `unavailable: landlock scope`。按用户范围不隔离网络：不用 `--unshare-net`。bwrap 进程与命名空间 init 的回收上限都是 1 秒。

已取消（2026-11-09 12:57 用户取消沙箱内 apt 安装）：沙箱内 apt/dpkg 安装。因此不再做 overlay、目录骨架、host-stamp，也不再把 init 回收放宽到 30 秒。apt/dpkg 在沙箱内自然失败，且不改宿主。

已删除、不再实现：私人 root 拷贝、解释器与库的链接物化、DNS/resolver 拷贝与回收、私人 apt 源和钥匙环供给，以及只为它们服务的 CI 步骤。

## 验收

真实通过：`workspace_backend_tests` 里的原生 bwrap。bash 与 python3 有输出。`/work` 文件跨两次执行仍在。客人不能在 `/usr/bin`、`/etc`、`/var/lib/dpkg` 建文件，也不能追加 `/var/lib/dpkg/status`；`apt-get install` 与 `dpkg` 非零退出。宿主 status 的 sha256 和 mtime、`/usr/bin` 的目录项和 mtime 不变。测试清掉 `FD_CLOEXEC` 后，继承的宿主哨兵读写 FD 在 `/proc/self/fd` 和 `/proc/1/fd` 上做真实读和写都不可用。任一 `/proc/*/environ` 都没有宿主环境标记，失败时只报告命中次数。指向宿主哨兵或其他 owner 的符号链接失败。连接宿主抽象 unix 套接字得到 `EPERM`（Landlock scope）。客人 `/proc/self/fd` 与 `/proc/1/fd` 里 fd≥3 的目录 fd 为 0：owner 根目录 fd 不再传给 bwrap（此前 bwrap 不会关闭未消费的 fd，会把它留给客人）。

模拟/单元：Landlock ABI 低于 6 时失败关闭。

未验收：在线 QQ。多 UID。CI 内核的 Landlock ABI 要等 CI 确认。
