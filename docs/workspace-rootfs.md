# 工作区 rootfs 准备

消费侧已经接到执行器。每次真实 resolve 都经 `handle_tool` 进入。唯一生产供应策略UnacceptedRootfs始终拒绝。配置和模型都不能把 rootfs 标成已接受，也不能因此启用 bash 或 code。

owner 目录、锁和 bwrap 原语的代码已经有。还没有真实 rootfs，没有独立写层，也没有 bash、python、apt 的验收。下一切片是供应者实现，以及离线验签后的安全解包。

启动用 block-fd 握手：先阻塞，再 pin init 的 pidfd，然后才释放 payload。确认 init 死亡并回收 wrapper 之后，才 join 输出。join 可以取消。返回成功时，进程清理和 reader 回收都已经完成。清理结果未知时不能标成功。没有宿主 exec 降级。

解释器允许 bash（默认，不读 profile 和 rc）、兼容 sh，以及 `python3 -I`。资源 rlimit 按每个进程计算，不等于聚合限额。

单元 fixture 只把公开的 `true` 和 `sleep` 复制到任务 tempdir。那不是 rootfs。

`spec/tla/Runner.tla` 只证明这条抽象顺序：拒绝时零 spawn，pin 之前不释放 payload，正常finish解除看护前确认init和wrapper清理；Drop仅尽力清理，不证明未知错误下最终无残留，成功返回前进程清理和 reader 回收都完成，未知清理不能标成功。它不证明操作系统隔离，也不是真实 apt。



`scripts/prepare-workspace-rootfs.sh` 只做公开工件的下载和核对。它要求 uid 不是 0，`umask 077`，并且只接受一个绝对路径。路径先 canonical 父目录，再用 `mkdir` 原子创建全新目录。目标已经是文件、目录或符号链接时拒绝，不覆盖。`GNUPGHOME` 建在这个新目录里。

脚本用 `curl` 的 `--proto '=https'`、`--proto-redir '=https'`、连接 10 秒、总时长 60 秒。它不改 DNS 或代理。公钥来自 `https://archive.ubuntu.com/ubuntu/project/ubuntu-archive-keyring.gpg`。`gpg --no-options --batch` 导入后，verify 必须退出 0。`--status-fd` 的 `VALIDSIG` 接受主指纹字段 `$12` 为 `843938DF228D22F7B3742BC0D94AA3F0EFE21092` 的子 key，或主 key 自己签名且主指纹缺失或相同。不要求子 key 指纹等于主指纹。`scripts/test-workspace-rootfs.py` 只用假的 curl 和 gpg 检查这些分支，不是官方工件的加密验签，也不是端到端证明。清单里只能有一条 amd64 的 `ubuntu-base-24.04` 或 `ubuntu-base-24.04.<数字>` 的 `base` tar，摘要是 64 位十六进制，文件名不能带路径。然后核对这个 tar 的 SHA256。

没有 `--unpack`。不在脚本里解包。这台环境访问 `cdimage.ubuntu.com`、`archive.ubuntu.com` 和 `partner-images.canonical.com` 时 DNS 失败，所以下载还没有实际成功。成功跑完脚本也不等于 bwrap 或文件隔离验收。缺 rootfs 时工具保持不可用。不把 cgroup 当成文件隔离的前置条件。

`scripts/verify-workspace-bundle.py` 是独立离线验证入口。调用是 `python3 scripts/verify-workspace-bundle.py /绝对source /私有parent/新snapshot`。source 含固定 3 个元数据文件：`SHA256SUMS` 上限 1MiB、`SHA256SUMS.gpg` 上限 64KiB、`ubuntu-archive-keyring.gpg` 上限 8MiB，以及唯一匹配的 24.04 amd64 archive（上限 512MiB）。生产公开主指纹沿用 `scripts/prepare-workspace-rootfs.sh` 已接受的 `843938DF228D22F7B3742BC0D94AA3F0EFE21092`，不新增测试 key 信任。

调用者不是 root。路径逐级 NOFOLLOW，打开用 NONBLOCK，避免对预打开对象阻塞。拒 FIFO、目录、符号链接和硬链接。父目录是 0700 且属于本人。目标独占创建。私有副本文件是 0600。摘要精确匹配 SHA256。不解包，不执行。receipt 的 gate 为 false。`gpgv` 壁钟 15 秒、CPU 10 秒、地址空间 256MiB，单文件输出上限 1MiB。`gpgv` 必须退出 0，且 status 里恰好一个 `VALIDSIG`，其 primary 指纹等于上述固定官方指纹。绝不能把 `GOODSIG` 或给人看的 stderr 当成信任。`gpgv` 的 stdout 仅按 status 协议解析。本脚本 CLI 成功时 stdout 一行 receipt JSON；失败时 stderr 固定 error，退出并非 0。

receipt 不是 runtime 授权。唯一生产供应策略 UnacceptedRootfs 依旧拒绝。

本机 `gpg` 2.4.7 存在，`gpgv` 缺失。2026-11-07 17:26 Asia/Shanghai 用正常包管理安装时失败：`mirrors.aliyun.com` Temporary failure resolving。已停止。不改软件源、DNS 或代理，也不绕路。本机真实 `gpgv` 测试必须明确 skip。CI 安装 `gpgv`、`gpg`、`gpg-agent`、`gpgconf` 之后运行 `python3 scripts/test-workspace-bundle.py` 和 `REQUIRE_GPGV=1 python3 scripts/test-workspace-bundle-gpgv.py`，无 `continue-on-error`，强制真实签名、错签名和篡改。

这里不声称验证已经实际成功，也不声称官方 rootfs 已经到手。下一切片只做安全解包：拒绝 absolute、`..`、symlink、hardlink 逃逸和 device，带资源限制和私有写层。未实测的 gate 继续关闭。本次不实现解包。
