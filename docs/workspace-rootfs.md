# 工作区 rootfs 准备

消费侧已经接到执行器。每次调用用这一轮真实 `MemoCallScope` 做成 `HostTurn`，`handle_tool_with` 重新 resolve，不复用上一名说话人。群里只有当前 actor 显式选过本人（表键是群号+actor）才进个人 owner，否则是当前群。私聊 source 与 actor 不一致则拒绝。

生产供给接线已经实现，离线单元门禁已通过。它不是永久的 `UnacceptedRootfs` 占位，也不是生产已接受或 Ubuntu 就绪。`FUJIANG_WORKSPACE_ACCEPTANCE` 现已确认未设置（空值同样失败），工具不广告、不启动进程。该变量只给宿主进程，指向管理员的私有信任目录（nofollow）。目录内 `acceptance.json` 须为 bot euid 所有、普通文件、nlink 1、mode 0600。路径、receipt 或 `approved: true` 单独都不构成接受。模型参数仍只有 `script` 与 `lang`，不能传 rootfs 或 owner 路径。`production_available` 只由这一次 `acquire` 派生。历史 receipt 与 owner identity 里的 `unaccepted` 标记不是管理员验收凭据。

已实现的执行链：`RootfsAuthority::acquire` 给出 host 与 `VerifiedBase` 之后，`WorkspaceStore::open(host, typed owner, &VerifiedBase)` 在 owner 锁下准备目录，把该 owner 的 rootfs FD 与 work FD 借给既有 runner。锁随 store 活到调用结束。`VerifiedBase` 的字段是私有 `OwnedFd` 和 32 字节摘要。构造器只有 `cfg(test)` 的合成 `fixture`。生产路径不能用 receipt 造出一个 `VerifiedBase`。

owner 核心在 `workspace_seed.rs`，字节复制在 `workspace_seed_copy.rs`，正式 OS 测试在 `workspace_seed_tests.rs`。三处都已完成。目录是 `personal/<id>` 或 `group/<id>`。私有 generation 里有 rootfs，以及严格 identity 文件：属当前 euid、nlink 1、mode 0600，内容含 owner 种类与 id、base 摘要和 `unaccepted` 策略。发布是锁内一次 `rename`（`RENAME_NOREPLACE`）。失败则在锁下删 stage。`work` 跨打开保留，不随 generation 重做。复制用 nofollow 的目录 FD 做字节复制：绝不硬链接 base，也绝不硬链接其他 owner；同一 owner 树内的硬链接按原链接关系保留。符号链接按原文推迟创建，宿主复制过程不解引用。`proc`、`dev`、`work`、`tmp`、`run`、`sys` 必须是真实目录，不能是链接。

复制界：深度 64、节点 50000、合计 1GiB、单文件 256MiB。全局节点预算在预取分配之前计入，清理也计入；有一条很小的真实嵌套宽预算测试。来源 setuid/setgid 与特殊文件拒绝。不复制 xattr 或 capability。普通文件 0600，可执行 0700。私有 owner 包装目录 0700。guest 的 root 与上述挂载点目录可以是 0755。这些界和 runner 里已有的每进程 rlimit 只约束这一次复制和该进程。它们不是聚合文件系统配额，也不是完整的 cgroup 安全。

QQ 选择走真实 SQLite metadata，测试不调用 NapCat。OS 复制、持久、并发、硬链接、符号链接、metadata、FIFO、清理和界的测试通过。ToolLoop 的模拟 LLM，以及成功路径上替换的最终 runner 回调，是模拟。合成的不完整 owner root 上，原生 runner 对 Bash 和 Python 都真实启动过 bwrap：exit 1，`bwrap: execvp /bin/bash: No such file or directory` 与 `bwrap: execvp /usr/bin/python3: No such file or directory`（`owner-agent-second.log`）。这是负向调用，不是 shell、Python 或 Ubuntu 成功。没有官方 rootfs、apt、网络、挂载或 FD 的端到端验收，也没有 NapCat 或 provider 生产测试。

已实现的宿主链：每次 `acquire` 用编译期嵌入的同一验证器，固定 `/usr/bin/python3 -I -c` 重新跑，不信任来源目录里已有 receipt。顺序是签名、密封归档、安全解包、规范树比对、私有且 inode 独立的复制加租约、`VerifiedBase`。挂载目录和原子发布之前，实际 owner 克隆的摘要必须等于已批准的规范树。私有来源目录 FD 不会把内容冻住。同尺寸改写、链接和硬链接别名都要查。树摘要函数是 `workspace_store` 里的 `tree_digest`。默认没有信任目录就是关闭。

`r*` 是 Rust 独占 `mkdir` 的包装目录，验证器子进程在其中新建 snapshot。名字冲突不能删掉另一次 `acquire`。碰撞回归的哨兵保留。`live-<pid>-<nanos>` 是本次租约。正常失败和 `Drop` 会删掉本次租约。启动崩溃留下的孤儿租约仍然没有回收器。

管理员命令（只检查，不写批准）：

```
python3 scripts/verify-workspace-bundle.py --unpack /绝对source /私有parent/新snapshot
```

成功的 receipt 恰好八个字段：`archive`、`sha256`、`bytes`、`primary`、`extracted`（true）、`execution_accepted`（恒 false）、`tree_sha256`、`tree_policy`（`workspace-rootfs-runtime-v1`）。这是签名检查后的提案。

九字段批准只在真实工件和运行时缺口都接受之后才允许手工写入。当前已知代码未完成，不能批准。没有自动晋升。本文不创建 `acceptance.json`。环境变量保持未设置。尚无官方工件，也没有真实成功的 bash、Python、Ubuntu、apt 或在线 QQ。有限结构切片仍见文末。

仍挡住真实运行批准的代码（不只是缺官方工件）：按 owner 的受控隔离出口和代理没有实现，`--unshare-net` 现在没有出站；这不是永久禁止 apt 联网。与 apt 兼容的 mode、UID、GID、capability 和软件包维护者执行策略未决；来源复制用 0600/0700、去掉 xattr、拒绝 setid，不是完整 Ubuntu POSIX 属主。3 秒壁钟和 64MiB `RLIMIT_FSIZE` 需要 apt 策略才能用。聚合 cgroup、pids、内存、磁盘和 inode 配额没有。既有 runner 的退出、整组杀死和直接 reap 夹具已通过，含有界观察；这不是所有 OS 失败都已清理的强证明，也不是完整沙箱。

启动用 block-fd 握手：先阻塞，再 pin init 的 pidfd，然后才释放 payload。确认 init 死亡并回收 wrapper 之后，才 join 输出。join 可以取消。返回成功时，进程清理和 reader 回收都已经完成。清理结果未知时不能标成功。没有宿主 exec 降级。

解释器允许 bash（默认，不读 profile 和 rc）、兼容 sh，以及 `python3 -I`。资源 rlimit 按每个进程计算，不等于聚合限额。

单元 fixture 只把公开的 `true` 和 `sleep` 复制到任务 tempdir。那不是 rootfs。

`spec/tla/Runner.tla` 只证明这条抽象顺序：拒绝时零 spawn，pin 之前不释放 payload，正常finish解除看护前确认init和wrapper清理；Drop仅尽力清理，不证明未知错误下最终无残留，成功返回前进程清理和 reader 回收都完成，未知清理不能标成功。它不证明操作系统隔离，也不是真实 apt。

`spec/tla/Bundle.tla` 只证明解包抽象顺序，不证明密码、操作系统、CPU 或权限。



`scripts/prepare-workspace-rootfs.sh` 只做公开工件的下载和核对。它要求 uid 不是 0，`umask 077`，并且只接受一个绝对路径。路径先 canonical 父目录，再用 `mkdir` 原子创建全新目录。目标已经是文件、目录或符号链接时拒绝，不覆盖。`GNUPGHOME` 建在这个新目录里。

脚本用 `curl` 的 `--proto '=https'`、`--proto-redir '=https'`、连接 10 秒、总时长 60 秒。它不改 DNS 或代理。公钥来自 `https://archive.ubuntu.com/ubuntu/project/ubuntu-archive-keyring.gpg`。`gpg --no-options --batch` 导入后，verify 必须退出 0。`--status-fd` 的 `VALIDSIG` 接受主指纹字段 `$12` 为 `843938DF228D22F7B3742BC0D94AA3F0EFE21092` 的子 key，或主 key 自己签名且主指纹缺失或相同。不要求子 key 指纹等于主指纹。`scripts/test-workspace-rootfs.py` 只用假的 curl 和 gpg 检查这些分支，不是官方工件的加密验签，也不是端到端证明。清单里只能有一条 amd64 的 `ubuntu-base-24.04` 或 `ubuntu-base-24.04.<数字>` 的 `base` tar，摘要是 64 位十六进制，文件名不能带路径。然后核对这个 tar 的 SHA256。

没有 `--unpack`。不在脚本里解包。这台环境访问 `cdimage.ubuntu.com`、`archive.ubuntu.com` 和 `partner-images.canonical.com` 时 DNS 失败，所以下载还没有实际成功。成功跑完脚本也不等于 bwrap 或文件隔离验收。缺 rootfs 时工具保持不可用。不把 cgroup 当成文件隔离的前置条件。

`scripts/verify-workspace-bundle.py` 是独立离线验证入口。调用是 `python3 scripts/verify-workspace-bundle.py /绝对source /私有parent/新snapshot`。source 含固定 3 个元数据文件：`SHA256SUMS` 上限 1MiB、`SHA256SUMS.gpg` 上限 64KiB、`ubuntu-archive-keyring.gpg` 上限 8MiB，以及唯一匹配的 24.04 amd64 archive（上限 512MiB）。生产公开主指纹沿用 `scripts/prepare-workspace-rootfs.sh` 已接受的 `843938DF228D22F7B3742BC0D94AA3F0EFE21092`，不新增测试 key 信任。

调用者不是 root。路径逐级 NOFOLLOW，打开用 NONBLOCK，避免对预打开对象阻塞。拒 FIFO、目录、符号链接和硬链接。父目录是 0700 且属于本人。目标独占创建。私有副本文件是 0600。摘要精确匹配 SHA256。不解包，不执行。receipt 的 gate 为 false。`gpgv` 壁钟 15 秒、CPU 10 秒、地址空间 256MiB，单文件输出上限 1MiB。`gpgv` 必须退出 0，且 status 里恰好一个 `VALIDSIG`，其 primary 指纹等于上述固定官方指纹。绝不能把 `GOODSIG` 或给人看的 stderr 当成信任。`gpgv` 的 stdout 仅按 status 协议解析。本脚本 CLI 成功时 stdout 一行 receipt JSON；失败时 stderr 固定 error，退出并非 0。

无 `--unpack` 的 receipt 不是 runtime 授权。出口、apt 策略、配额和租约回收的缺口见文首。三块要分开：QQ 宿主授权是真代码；owner 持久是真实文件系统测试；bash/Python 路由是真代码，终端成功夹具是模拟。在线 QQ 与完整 Ubuntu/apt 未验证。

本机 `gpg` 2.4.7 存在，`gpgv` 缺失。2026-11-07 17:26 Asia/Shanghai 用正常包管理安装时失败：`mirrors.aliyun.com` Temporary failure resolving。已停止。不改软件源、DNS 或代理，也不绕路。本机真实 `gpgv` 测试必须明确 skip。CI 安装 `gpgv`、`gpg`、`gpg-agent`、`gpgconf` 之后运行 `python3 scripts/test-workspace-bundle.py` 和 `REQUIRE_GPGV=1 python3 scripts/test-workspace-bundle-gpgv.py`，无 `continue-on-error`，强制真实签名、错签名和篡改。

这里不声称验证已经实际成功，也不声称官方 rootfs 已经到手。未实测的 gate 继续关闭。

## `--unpack SOURCE NEWTARGET`

`python3 scripts/verify-workspace-bundle.py --unpack SOURCE NEWTARGET` 的契约如下。没有 `--unpack` 时，CLI 仍走原来的验证路径。`scripts/prepare-workspace-rootfs.sh` 仍然不解包。

每次调用都重新验签，并复制一份新的私有 snapshot，不信任旧 receipt。SHA 核对之后，归档放进 memfd，并设置 `F_SEAL_WRITE`、`F_SEAL_GROW`、`F_SEAL_SHRINK`、`F_SEAL_SEAL`，使随后解包读到的字节保持不变。这只约束顺序，不把密封当成密码学证明。

私有 stage 由受控 fork 写入。限额是 CPU 10 秒、硬限制 11 秒、地址空间 512MiB、壁钟 15 秒。失败先 kill，再 wait，然后删除 stage，不发布。成功之后才发布 rootfs 和 receipt。

目录打开使用 `FD_NOFOLLOW`，新建文件使用 `O_EXCL`。重复的规范路径拒绝，并计入隐式目录 nodes。条目上限 50000，展开上限 1GiB，单文件 256MiB，膨胀比 200，计算包含头 padding 和尾部。目录和可执行文件模式 0700，普通文件 0600。不 chown，不写 xattr。setuid/setgid 仍拒绝，消毒后的模式不变。资源限额、失败时的原子清理和 gate 都不变。

`--unpack` 的 receipt 含 `extracted: true`、`execution_accepted: false`、`tree_sha256` 和 `tree_policy`。这是提案，不是接受。

`spec/tla/Bundle.tla` 的顺序是：刚验证通过，密封归档，worker 写私有 stage，成功回收 worker，然后发布 rootfs 和 receipt。失败必须先 kill 并 reap，再 cleanup，且不发布。`executionAccepted` 始终为 FALSE。发布前必须已经验证（含 hash）、密封，并且 worker 已成功回收。失败路径没有可用 rootfs。

### 历史：`a37ac01` 拒绝全部链接与 PAX

提交 `a37ac01` 的整体 CI 已成功：2026-11-07 18:11:48 Asia/Shanghai，https://github.com/KazeMae/FujiangBot/actions/runs/42362577354 。该切片路径允许一次根成员 `.` 或 `./dir`，以及标准 `./` 前缀；只接受 plain ASCII 的 regular 和 directory，格式只限 UStar 与 V7；拒绝全部链接、PAX 与 GNU 扩展、sparse、device、FIFO、socket。这是当时范围，不是当前已实现的有限结构切片。本次不重试任何 DNS 失败的下载或安装，也不借 CI 下载工件。本机仍缺 `gpgv`，真实测试继续由既有 CI 环境承担，不改依赖。

### 已实现：延迟链接与有限 PAX（有限结构切片）

有限结构切片已实现。下面这一段数字是当时的记录，不是当前门禁：24 个 Python、248 个 Rust、5 个 TLC、本机 gpgv 14 项 skip。当时新 head 的 CI 未知。

全部真实文件数据落盘之后才处理链接，并先校验整张虚拟图。硬链接其次：使用真实目录 FD，源必须是普通文件且 nofollow。符号链接最后，按归档原文存储，绝对目标也照存。

绝对符号链接只在隔离 root 内解析。宿主导入和复制工具绝不能解引用它们。相对 `..` 只允许留在 root 内。root 下溢、环与跳数或工作预算、重复名字，以及把链接当作归档父目录，一律拒绝。硬链接正向链必须终止于普通文件数据，目标路径上不穿越符号链接。允许安全悬空符号链接，例如 `etc/mtab` → `/proc/mounts`。本单位尚未验收运行时挂载。

路径分量允许可打印 ASCII，包括 coreutils 的 `[` 和 dpkg 的 `:`。拒绝控制字符、反斜杠、绝对路径条目和 `..` 条目。实际成员路径至多 64 个分量。staging 前父进程 soft `RLIMIT_NOFILE` 必须至少 192；从不提高或改写父进程限额。子进程 `NOFILE` 仍是 64。这挡住可确定触发的对抗性深树在清理时的递归与 FD 耗尽。

每文件 PAX `x` 头只允许 `atime`、`mtime`、`ctime`：严格有界的十进制记录，校验后忽略，不恢复时间戳。未知记录、path、linkpath、size 覆盖、全局 `g`、xattr 与 capability、ACL、sparse 和 GNU 扩展都拒绝。

这不是完整 Ubuntu 供应，也不是 apt 可用。来源只说明工具会写出这类头：Ubuntu noble 的 livecd-rootfs `auto/build`（https://git.launchpad.net/livecd-rootfs/plain/live-build/auto/build?h=ubuntu/noble）使用 `tar -c --sort=name --xattrs`；本机 GNU tar 1.35 对同样的合成 `--xattrs` 探测会写出 mtime、atime、ctime，本地证据是 `/workspace/agent-tools/fujiang-local-20261107/pax-probe-strings.txt`；POSIX pax（https://pubs.opengroup.org/onlinepubs/9699919799/utilities/pax.html）规定记录字节长度和小数 mtime。这些都不能证明官方工件的实际元数据。官方 rootfs 仍不存在。

### 历史本地门禁（已推基线及更早）

`OwnerLayer.tla` 只证明抽象不变量。日志里的 346 个 distinct states 不代替 OS 测试。

已推基线 `92a7b15` 当时的本地数字是：全工作区 261（agent 179）、行覆盖 90.76%、bundle 24、gpgv 14 SKIP、stub 11。更早的 248 项与 Workspace 640 只属于当时那一头。该基线全部 CI 步骤含真实 gpgv 于 2026-11-07 19:25:36 Asia/Shanghai 通过，run `42367869558`。`32427e4` 的 run `42364745250` 更早。这些都不是下面这一节。

### 提交前本地验收记录

独立复核：fmt 通过；agent clippy 全部 target、`--no-deps --offline -D warnings` 通过；`cargo test --workspace --offline` 278 通过（agent 196），0 失败、0 ignored。覆盖率重跑 agent 测试通过。实际 TOTAL 行覆盖 89.80%（10100 行，1030 未覆盖）。同一输出里的 region 87.39% 不是行覆盖。`workspace_acceptance` 行覆盖 77.80%，树模块 86.24%，都不是端到端证明。Python bundle 27 通过，含重复 FD、硬链接别名、同尺寸篡改和负向边界的黄金用例。真实 gpgv 14 条在本机明确 SKIP。既有 rootfs stub 11 通过。六模型 TLC distinct states 54/688/512/346/17/16，通过。`scripts/verify-workspace-bundle.py` 恰好 1000 行。文件长度与 `git diff --check` 通过。这是提交前本地验收记录。CI 以 PR 62 当前 head 的检查页为准。下文只记录已核验的 `92a7b15` 基线 CI，不另写提交号。

`acceptance.json` 必须恰好九键：`version`（JSON 数字 1）、`kind`（`runtime_safety_approval`）、`source`、`host`、`archive_sha256`、`primary`（`843938DF228D22F7B3742BC0D94AA3F0EFE21092`）、`policy`（`workspace-rootfs-runtime-v1`）、`tree_sha256`、`approved`（JSON `true`）。两枚摘要是 64 位小写十六进制。文件上限 8192 字节，mode 0600，属 bot euid。信任目录 mode 0700。当前没有这份文件。

PR 62 中文正文更新 403，已停止，不再用 gh 或浏览器重试。拟稿在仓库外。不部署、不合并、不重启。运行时没有作为 Ubuntu 交付。
