# 工作区：宿主 overlay

默认关闭。只有 `FUJIANG_WORKSPACE_MVP` 指向绝对的宿主目录时才打开。绑定仍是每次调用按 QQ 解析的个人或群，生产不因此打开。

客人直接使用宿主的 `/usr`、`/etc`、`/var`，经 bwrap `--overlay` 叠到 owner 的空目录骨架上。骨架只补可搜索的真实目录，不跟符号链接，不跨文件系统，不拷贝普通文件。只有宿主 dpkg status 或 `/usr`、`/etc`、`/var` 顶层目录变化时才重走骨架；宿主装包之后新目录才会可写。`/work` 是 owner 的工作目录。`/var/log`、`/var/cache/apt` 和 `/var/lib/apt/lists/partial` 绑定到 owner 自己的目录。apt 锁（`lock`、`lock-frontend`、`triggers/Lock`、lists 的 `lock`、`debconf/passwords.dat`）只在 upper 里放空文件。`/var/lib/dpkg/status` 和 `available` 只在该 owner 第一次创建时拷进 upper；之后 owner 的 dpkg 库与宿主后来的升级分开。`/etc/apt/auth.conf.d` 用 tmpfs 盖住。Landlock ABI 至少 6，在 exec bwrap 之前限制抽象 unix 套接字和信号；更低就拒绝，错误是 `unavailable: landlock scope`。按用户范围不隔离网络：不用 `--unshare-net`，客人与宿主共享网络。命名空间 init 退出包含 overlay 卸载时对宿主上层文件系统的 sync，耗时随宿主脏页量增长，故 init 回收上限为 30 秒（bwrap 进程回收仍为 1 秒）。

已删除、不再实现：私人 root 拷贝、解释器与库的链接物化、DNS/resolver 拷贝与回收、私人 apt 源和钥匙环供给，以及只为它们服务的 CI 步骤。

## 验收

真实通过：`workspace_backend_tests` 里的原生 bwrap。bash 与 python3 有输出。`/work` 文件跨两次执行仍在。离线本地 `.deb` 经 apt 显示 newly installed，再次打开仍已安装；2026-11-09 探针另有 apt remove 后再安装。宿主 `/usr` 和宿主 `dpkg` 不变，另一个 owner 看不见该包和文件。测试清掉 `FD_CLOEXEC` 后，继承的宿主哨兵读写 FD 在 `/proc/self/fd` 和 `/proc/1/fd` 上做真实读和写都不可用。任一 `/proc/*/environ` 都没有宿主环境标记。指向宿主哨兵或其他 owner 的符号链接失败。连接宿主抽象 unix 套接字得到 `EPERM`（Landlock scope）。客人在 upper 里把目录换成指向宿主目录的符号链接后，下一次宿主侧 `prepare` 不会在 owner 外写文件。

模拟/单元：骨架谓词跳过符号链接、不可读目录和其他文件系统。Landlock ABI 低于 6 时失败关闭。非普通文件的 dpkg status 被拒绝。

未验收：远程 `apt-get update` / 安装。宿主 DNS 当前失败，没有重试。在线 QQ。用 dpkg 硬链接备份升级宿主已安装包。多 UID 和服务账户包。CI 内核的 Landlock ABI 要等 CI 确认。
