# 工作区 rootfs 准备

**当前（2026-11-08 DNS 门禁）：** 已完成。证据 `dns-20261108-gates-summary.json`，前缀 `dns-20261108-gate`。三次 `cargo test --workspace --offline` 各 313 个唯一通过、0 失败、1 ignored；agent 231，DNS 夹具三组。fmt、agent 严格 clippy（`-D warnings`）、Python 28、rootfs stub、六模型 TLC 都过。本机 gpgv 14 条是 SKIP，不是通过。03:05 的 310×3（`continue-20261108-gates-summary.json`）是历史，不含这次 DNS。更早 308×3 与 `feb96d0` 的 299×3 也是历史。基线 `07bf850` CI `42384023945`（2026-11-07 23:01:25）不含本段。新提交的 CI 看 PR 62，推送前不能写成远端已绿。PR 正文 403 不再重试。不重复预检，不下载，不改全局 DNS 或软件源。

生产关闭。普通网络，没有 `--unshare-net`。仍缺 `gpgv`、`uidmap`、`libsubid5` 和官方四件工件。唯一当前投递目录是 `/workspace/agent-tools/fujiang-local-20261107/offline-inbox`。`sandbox-lab/offline-inbox` 仍是空目录，只是历史路径，不监视、不移动、不删除。

DNS：固定从 `/` 做 root 所有、不跟随的目录 FD 走到 `etc/resolv.conf`。先 `O_NONBLOCK` 再看元数据。普通文件必须 root、0644、nlink 1。只允许精确指向 `/run/systemd/resolve/stub-resolv.conf` 或 `resolv.conf` 的一层链接；链接本身必须 root、nlink 1。快照含 ctime，但时钟可以不动：同一 FD 上第二次有界 `pread` 必须与第一次的字节和长度一致。同长度改内容并恢复 mtime 的夹具会被拒绝。正文只保留不超过 4096 的 ASCII，以及实际支持的指令，不另加解析器。`127.0.0.53`、`127.0.0.1`、`::1` 是继承网络里的本机回环，不需要代理，也不保证底层 DNS 成功。客人 `etc` 必须是属主可写的真实目录。属主的 0444 普通文件拒绝且内容不动。最终符号链接不先 unlink；stage 写完并 fsync 之后才 `renameat` 换目录项、不跟随。临时名冲突时原链接、外面的目标和无关文件都保持不动。`Drop` 只删本次创建的 `.resolv.<pid>.<seq>.tmp`。崩溃留下的旧临时名不再是固定名字，所以不会卡住下一次；没有实现主动清扫。解析夹具三组。没有真实 Ubuntu、apt 或在线 QQ。

元数据 v3 代码仍未实现。v2 仍拒绝 SUID、SGID 和 capability PAX；只有 atime/mtime/ctime 的 PAX 可以通过。依据是 Ubuntu Noble shadow 源码 <https://git.launchpad.net/ubuntu/+source/shadow/tree/debian/rules?h=ubuntu/noble> 里 `override_dh_builddeb-arch` 对 passwd/chfn/chsh/gpasswd/newgrp `chmod u+s`、对 chage/expiry `chmod g+s`。这只证明普通 Noble 包会带这些位，不是已经打开的 rootfs。下一步才是版本化剥离、新的 receipt/树摘要策略，并使旧批准失效。这些都还没做。

三条仍分开：QQ 绑定是真代码，无 NapCat 在线；owner 持久是文件系统夹具，无官方 Ubuntu Base；工具路由是真代码，成功回调是模拟。都还没有真实验收。

历史计数只留在上面。这次提交的 CI 看 PR 62；本地日志不能代替远端。PR 正文 403 不再重试。

消费侧已经接到执行器。每次调用用这一轮真实 `MemoCallScope` 做成 `HostTurn`，`handle_tool_with` 重新 resolve，不复用上一名说话人。群里只有当前 actor 显式选过本人（表键是群号+actor）才进个人 owner，否则是当前群。私聊 source 与 actor 不一致则拒绝。

生产关闭。它不是永久的 `UnacceptedRootfs` 占位，也不是生产已接受或 Ubuntu 就绪。`FUJIANG_WORKSPACE_ACCEPTANCE` 未设置（空值同样失败），没有 `acceptance.json`，工具不广告、不启动进程。v1 批准被 v2 拒绝。目标是文件权限隔离、个人/群 owner 持久、每次执行绑定当前 QQ。apt 只应改沙箱，不能成为宿主 root。runner 已去掉 `--unshare-net`，并有负向 argv 测试。不挂宿主敏感目录、套接字或环境密钥。没有另做代理或 host 服务。普通网络不是交付门禁。该变量只给宿主进程，指向管理员的私有信任目录（nofollow）。目录内 `acceptance.json` 须为 bot euid 所有、普通文件、nlink 1、mode 0600。路径、receipt 或 `approved: true` 单独都不构成接受。模型参数仍只有 `script` 与 `lang`，不能传 rootfs 或 owner 路径。`production_available` 只由这一次 `acquire` 派生。历史 receipt 与 owner identity 里的 `unaccepted` 标记不是管理员验收凭据。

已实现的执行链：`RootfsAuthority::acquire` 给出 host 与 `VerifiedBase` 之后，`WorkspaceStore::open(host, typed owner, &VerifiedBase)` 在 owner 锁下准备目录，把该 owner 的 rootfs FD 与 work FD 借给既有 runner。锁随 store 活到调用结束。`VerifiedBase` 的字段是私有 `OwnedFd` 和 32 字节摘要。构造器只有 `cfg(test)` 的合成 `fixture`。生产路径不能用 receipt 造出一个 `VerifiedBase`。

owner 核心在 `workspace_seed.rs`，字节复制在 `workspace_seed_copy.rs`，正式 OS 测试在 `workspace_seed_tests.rs`。三处都已完成。目录是 `personal/<id>` 或 `group/<id>`。私有 generation 里有 rootfs，以及严格 identity 文件：属当前 euid、nlink 1、mode 0600，内容含 owner 种类与 id、base 摘要和 `unaccepted` 策略。发布是锁内一次 `rename`（`RENAME_NOREPLACE`）。失败则在锁下删 stage。`work` 跨打开保留，不随 generation 重做。复制用 nofollow 的目录 FD 做字节复制：绝不硬链接 base，也绝不硬链接其他 owner；同一 owner 树内的硬链接按原链接关系保留。符号链接按原文推迟创建，宿主复制过程不解引用。`proc`、`dev`、`work`、`tmp`、`run`、`sys` 必须是真实目录，不能是链接。

复制界：深度 64、节点 50000、合计 1GiB、单文件 256MiB。来源 setuid/setgid、capability 与特殊文件拒绝。已实现并有测试：普通 rwx 与 sticky 目录；外层包装 0700，其内 root 0755；v2 树摘要。只读清理会先把属主目录改成可进入，再递归，不只是关 FD。运行时不再设置 `RLIMIT_FSIZE`。256MiB 只属于导入单文件界，不能当成 apt 兼容证明，也不是聚合配额。沙箱继承宿主的文件大小限制，不提高 hard limit，父进程不调用 `setrlimit`。本机只读看到的宿主 `RLIMIT_FSIZE` 是 soft `-1`、hard `-1`。`/tmp` 是私有持久 owner root 里的真实目录，root bind 之后固定 `bwrap --chmod 1777 /tmp`。bwrap 的 `--tmpfs` 默认 0755，若再盖住 `/tmp` 会丢掉 1777 并挡住 apt。`/run` 和 `/sys` 仍是 64MiB 空 tmpfs，不是包缓存。apt 默认 `Dir::Cache` 是 `var/cache/apt`（档案在 `archives/`），`Dir::State` 是 `var/lib/apt`（列表在 `lists/`），都在私有持久 root 上。没有实际 apt 安装证明。磁盘占用没有配额上限，耗尽是残留风险，不是出口硬门。壁钟 300 秒、CPU 120 秒、地址空间 512MiB、`NOFILE` 64、输出 64KiB 仍是有限的通用工具轮廓，不表示所有 apt 都能完成。

QQ 选择走真实 SQLite metadata，测试不调用 NapCat。OS 复制、持久、并发、硬链接、符号链接、metadata、FIFO、清理和界的测试通过。ToolLoop 的模拟 LLM，以及成功路径上替换的最终 runner 回调，是模拟。合成的不完整 owner root 上，原生 runner 对 Bash 和 Python 都真实启动过 bwrap：exit 1，`bwrap: execvp /bin/bash: No such file or directory` 与 `bwrap: execvp /usr/bin/python3: No such file or directory`（`owner-agent-second.log`）。这是负向调用，不是 shell、Python 或 Ubuntu 成功。没有官方 rootfs、apt、网络、挂载或 FD 的端到端验收，也没有 NapCat 或 provider 生产测试。

已实现的宿主链：每次 `acquire` 用编译期嵌入的同一验证器，固定 `/usr/bin/python3 -I -c` 重新跑，不信任来源目录里已有 receipt。顺序是签名、密封归档、安全解包、规范树比对、私有且 inode 独立的复制加租约、`VerifiedBase`。挂载目录和原子发布之前，实际 owner 克隆的摘要必须等于已批准的规范树。私有来源目录 FD 不会把内容冻住。同尺寸改写、链接和硬链接别名都要查。树摘要函数是 `workspace_store` 里的 `tree_digest`。默认没有信任目录就是关闭。

`r*` 是 Rust 独占 `mkdir` 的包装目录，验证器子进程在其中新建 snapshot。名字冲突不能删掉另一次 `acquire`。碰撞回归的哨兵保留。`live-<pid>-<nanos>` 是本次租约。正常失败和 `Drop` 会删掉本次租约，这条和陈旧 GC 是分开测的。GC 只回收有界的私有租约目录：flock、严格 metadata、nofollow。父进程退出后，真正的验证写入进程仍持有租约，收割之后 GC 才删。未知、仍活跃和外来名字保留。这部分 14 项已通过。

管理员命令（只检查，不写批准）：

```
python3 scripts/verify-workspace-bundle.py --unpack /绝对source /私有parent/新snapshot
```

成功的 receipt 恰好八个字段：`archive`、`sha256`、`bytes`、`primary`、`extracted`（true）、`execution_accepted`（恒 false）、`tree_sha256`、`tree_policy`（`workspace-rootfs-runtime-v2`）。这是签名检查后的提案。v1 策略的批准文件拒绝。

九字段批准只在真实工件和下面仍未完成的运行时条件都接受之后才允许手工写入。没有自动晋升。本文不创建 `acceptance.json`。环境变量保持未设置。没有真实成功的 bash、Python、Ubuntu、apt 或在线 QQ。有限结构切片仍见文末。

还不能批准。六个命名空间 capability（含 CAP_FSETID）已经接到 runner。`chmod(2)` 在调用者不是文件所属组且没有 `CAP_FSETID` 时会静默清掉 `S_ISGID`；`capabilities(7)` 说明 `CAP_FOWNER` 不代替它。它只加在已经核对两段映射的 user namespace 里，不是宿主 capability。`user_namespaces(7)` 要求 `CAP_FSETID` 的 UID 和 GID 都在映射内；`CAP_FOWNER` 是组属主的例外，不把这条扩大到其它能力。归档导入仍拒绝 setid 和 capability。本机没有发行版 `newuidmap`/`newgidmap`，所以没有真实跨 GID 的 SGID 或 apt。完整 UID/GID 引导的代码在，但还不能工作。`box:100000:65536` 是显式部署范围。最低后续是：在合法环境装上这两个 04755 辅助程序，再拿到合法签名的官方 rootfs，然后做真实端到端。这里不重试 DNS、不改软件源、不借 CI 下载，也不自造 setuid 辅助程序。

启动用 block-fd 握手：先阻塞，再 pin init 的 pidfd，然后才释放 payload。确认 init 死亡并回收 wrapper 之后，才 join 输出。join 可以取消。返回成功时，进程清理和 reader 回收都已经完成。清理结果未知时不能标成功。没有宿主 exec 降级。

解释器允许 bash（默认，不读 profile 和 rc）、兼容 sh，以及 `python3 -I`。CPU、地址空间、`NOFILE` 和 core 仍在子进程里设置。文件大小限制继承宿主，父进程不改。缺聚合配额只表示磁盘耗尽仍可能发生，不单独挡住文件隔离。

单元 fixture 只把公开的 `true` 和 `sleep` 复制到任务 tempdir。那不是 rootfs。

`spec/tla/Runner.tla` 只证明这条抽象顺序：拒绝时零 spawn，pin 之前不释放 payload，正常finish解除看护前确认init和wrapper清理；Drop仅尽力清理，不证明未知错误下最终无残留，成功返回前进程清理和 reader 回收都完成，未知清理不能标成功。它不证明操作系统隔离，也不是真实 apt。

`spec/tla/Bundle.tla` 只证明解包抽象顺序，不证明密码、操作系统、CPU 或权限。



`scripts/prepare-workspace-rootfs.sh` 只做公开工件的下载和核对。它要求 uid 不是 0，`umask 077`，并且只接受一个绝对路径。路径先 canonical 父目录，再用 `mkdir` 原子创建全新目录。目标已经是文件、目录或符号链接时拒绝，不覆盖。`GNUPGHOME` 建在这个新目录里。

脚本用 `curl` 的 `--proto '=https'`、`--proto-redir '=https'`、连接 10 秒、总时长 60 秒。它不改 DNS 或代理。公钥来自 `https://archive.ubuntu.com/ubuntu/project/ubuntu-archive-keyring.gpg`。`gpg --no-options --batch` 导入后，verify 必须退出 0。`--status-fd` 的 `VALIDSIG` 接受主指纹字段 `$12` 为 `843938DF228D22F7B3742BC0D94AA3F0EFE21092` 的子 key，或主 key 自己签名且主指纹缺失或相同。不要求子 key 指纹等于主指纹。`scripts/test-workspace-rootfs.py` 只用假的 curl 和 gpg 检查这些分支，不是官方工件的加密验签，也不是端到端证明。清单里只能有一条 amd64 的 `ubuntu-base-24.04` 或 `ubuntu-base-24.04.<数字>` 的 `base` tar，摘要是 64 位十六进制，文件名不能带路径。然后核对这个 tar 的 SHA256。

没有 `--unpack`。不在脚本里解包。已知 DNS 失败的 rootfs 下载不再重试，也不借 CI 传工件。成功跑完脚本也不等于 bwrap 或文件隔离验收。缺 rootfs 时工具保持不可用。cgroup 与聚合配额不是文件隔离的前置条件。

`scripts/verify-workspace-bundle.py` 是独立离线验证入口。调用是 `python3 scripts/verify-workspace-bundle.py /绝对source /私有parent/新snapshot`。source 含固定 3 个元数据文件：`SHA256SUMS` 上限 1MiB、`SHA256SUMS.gpg` 上限 64KiB、`ubuntu-archive-keyring.gpg` 上限 8MiB，以及唯一匹配的 24.04 amd64 archive（上限 512MiB）。生产公开主指纹沿用 `scripts/prepare-workspace-rootfs.sh` 已接受的 `843938DF228D22F7B3742BC0D94AA3F0EFE21092`，不新增测试 key 信任。

调用者不是 root。路径逐级 NOFOLLOW，打开用 NONBLOCK，避免对预打开对象阻塞。拒 FIFO、目录、符号链接和硬链接。父目录是 0700 且属于本人。目标独占创建。私有副本文件是 0600。摘要精确匹配 SHA256。不解包，不执行。receipt 的 gate 为 false。`gpgv` 壁钟 15 秒、CPU 10 秒、地址空间 256MiB，单文件输出上限 1MiB。`gpgv` 必须退出 0，且 status 里恰好一个 `VALIDSIG`，其 primary 指纹等于上述固定官方指纹。绝不能把 `GOODSIG` 或给人看的 stderr 当成信任。`gpgv` 的 stdout 仅按 status 协议解析。本脚本 CLI 成功时 stdout 一行 receipt JSON；失败时 stderr 固定 error，退出并非 0。

无 `--unpack` 的 receipt 不是 runtime 授权。多 ID 引导代码已接上，真实辅助程序、完整内核映射和官方 rootfs 端到端仍未完成。在线 QQ 与 Ubuntu/apt 未验证。

本机 `gpg` 2.4.7 存在，`gpgv` 缺失。2026-11-07 17:26 Asia/Shanghai 用正常包管理安装时失败：`mirrors.aliyun.com` Temporary failure resolving。已停止。不改软件源、DNS 或代理，也不绕路。本机真实 `gpgv` 测试必须明确 skip。CI 安装 `gpgv`、`gpg`、`gpg-agent`、`gpgconf` 之后运行 `python3 scripts/test-workspace-bundle.py` 和 `REQUIRE_GPGV=1 python3 scripts/test-workspace-bundle-gpgv.py`，无 `continue-on-error`，强制真实签名、错签名和篡改。

这里不声称验证已经实际成功，也不声称官方 rootfs 已经到手。未实测的 gate 继续关闭。

## `--unpack SOURCE NEWTARGET`

`python3 scripts/verify-workspace-bundle.py --unpack SOURCE NEWTARGET` 的契约如下。没有 `--unpack` 时，CLI 仍走原来的验证路径。`scripts/prepare-workspace-rootfs.sh` 仍然不解包。

每次调用都重新验签，并复制一份新的私有 snapshot，不信任旧 receipt。SHA 核对之后，归档放进 memfd，并设置 `F_SEAL_WRITE`、`F_SEAL_GROW`、`F_SEAL_SHRINK`、`F_SEAL_SEAL`，使随后解包读到的字节保持不变。这只约束顺序，不把密封当成密码学证明。

私有 stage 由受控 fork 写入。限额是 CPU 10 秒、硬限制 11 秒、地址空间 512MiB、壁钟 15 秒。失败先 kill，再 wait，然后删除 stage，不发布。成功之后才发布 rootfs 和 receipt。

目录打开使用 `FD_NOFOLLOW`，新建文件使用 `O_EXCL`。重复的规范路径拒绝，并计入隐式目录 nodes。条目上限 50000，展开上限 1GiB，单文件 256MiB，膨胀比 200，计算包含头 padding 和尾部。v2 写出普通 rwx，目录可带 sticky；外层包装 0700，其内 root 0755。不 chown，不写 xattr 或 capability。setuid/setgid 仍拒绝。只读清理先 `chmod` 属主目录再递归。失败时的原子清理和 `execution_accepted: false` 不变。Rust 与 Python 的 v2 黄金摘要独立一致：`d4c7888733e446897db069da80dbd6dc00fc402689b3050f98ac36bc295d8dca`。Python 归档 28 已经通过，不是待重跑。

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

历史本地记录（不是当前门禁）：fmt 通过；agent clippy 全部 target、`--no-deps --offline -D warnings` 通过；`cargo test --workspace --offline` 278 通过（agent 196），0 失败、0 ignored。当时覆盖率重跑后 TOTAL 行覆盖 89.80%（10100 行，1030 未覆盖）。同一输出里的 region 87.39% 不是行覆盖。`workspace_acceptance` 行覆盖 77.80%，树模块 86.24%。Python bundle 当时 27。真实 gpgv 14 条 SKIP。rootfs stub 11。六模型 TLC 54/688/512/346/17/16。验证器脚本 1000 行。文件长度与 `git diff --check` 通过。行覆盖 89.80% 只属于这段历史。当前状态见文首 03:42 DNS，不把这段历史当成现门禁。

`acceptance.json` 必须恰好九键：`version`（JSON 数字 1）、`kind`（`runtime_safety_approval`）、`source`、`host`、`archive_sha256`、`primary`（`843938DF228D22F7B3742BC0D94AA3F0EFE21092`）、`policy`（`workspace-rootfs-runtime-v2`）、`tree_sha256`、`approved`（JSON `true`）。两枚摘要是 64 位小写十六进制。文件上限 8192 字节，mode 0600，属 bot euid。信任目录 mode 0700。v1 的 `policy` 拒绝。当前没有这份文件。

代码提交 `8d7eda1f302d05d9b29cbbc7225e4c348447c399` 已推送。它的 CI run `42374926956` 于 2026-11-07 21:04:03 Asia/Shanghai 全部成功：https://github.com/KazeMae/FujiangBot/actions/runs/42374926956 。本文件后来的文档修改不是该 run 所验证的提交。没有新的覆盖率百分比；89.80% 仍只属于更早基线。先前基线 CI 是 `aaa7291` / run `42370884522`（20:08:31 Asia/Shanghai）。PR 正文 403 已是最终状态，不再重试。不部署、不合并、不重启。不重试已知 DNS、rootfs 下载或 CI 工件传输。

## 本切片变更

本切片在既有 runner 上接了标准 uidmap 辅助程序、`fd:N`、`setgroups=allow`、核对过的 namespace FD、bwrap `--userns`，以及 `--cap-drop ALL` 之后的六个命名空间 capability（含 CAP_FSETID）。holder 在 `unshare` 之前用 `close_range` 关掉除两根管道以外的 FD。缺已安装的辅助程序、真实完整映射和官方 rootfs，所以不是 apt。生产关闭。

## 本切片验证记录

历史（不是当前门禁）：曾有 `cargo test --workspace --offline` 284 通过（agent 202）。其后一轮 agent 201 通过、1 失败，失败项 `existing_runner_failclosed_on_incomplete_root`，owner lock EAGAIN。该失败已由 `OwnerLock` 的 `LOCK_UN` 修掉，并包含在下面的 286 里。

历史本地计数：三次 workspace 各 298 通过（agent 216）。那不是当前门禁。

历史计数：`final-fsetid-*.log` 的 299×3（agent 217，0 ignored）属于 `feb96d0`，不是当前门禁。`95c435b` / `42380383930` 是更早的红 CI。

当前非生产探针的完整名字是 `workspace_store::acceptance::rootfs_probe::admin_real_rootfs_acceptance`。命令是：

```sh
FUJIANG_ROOTFS_SOURCE=/绝对/私有0700源 FUJIANG_ROOTFS_WORK=/绝对/0700父目录下尚不存在的叶子 \
  cargo test -p fujiang-plugin-agent --lib --offline \
  workspace_store::acceptance::rootfs_probe::admin_real_rootfs_acceptance \
  -- --ignored --exact --test-threads=1
```

短名字加 `--exact` 会匹配 0 个测试，不算这次调用。`--list` 里这个完整名字必须恰好一条。夹具在旁边的 `probe_tests` 模块，不是这条管理入口。

预检在创建叶子之前完成，且不写 `acceptance.json`、不设置生产变量：

- 源和工作父目录必须是调用者拥有的 0700。拒绝符号链接、`..`，以及与生产信任目录重叠。叶子已存在则拒绝，不删除。
- 预检工具是 `/usr/bin/gpgv`、`/usr/bin/newuidmap`、`/usr/bin/newgidmap`、`/usr/bin/bwrap`、`/usr/bin/python3`，加上官方 metadata。归档只要求有普通 `.tar.gz`；签名文件名和 24.04 版本仍由现有验证器选择。
- 缺件时九个用例都是 `unrun`，进程非 0。2026-11-08 只读预检：仍缺 gpgv、newuidmap、newgidmap。`bwrap` 是 0755。`/usr/bin/python3` 是 0777 符号链接，`stat -L` 的目标是 root:root 0755，不是 0777 的可执行文件。包缓存是空的。配置源 `https://mirrors.aliyun.com/debian` trixie/main 的缓存候选（不是当场联网复核）是 `uidmap` `1:4.17.4-2`、同时缺失的 `libsubid5` `1:4.17.4-2`、`gpgv` `2.4.7-21+deb13u1+b5`。其余 uidmap/gpgv 依赖已安装。inbox `/workspace/agent-tools/fujiang-local-20261107/offline-inbox` 仍是 0700 空目录，没有覆盖工件。2026-11-07 17:26 的记录只证明 `mirrors.aliyun.com` `Temporary failure resolving`，不能证明策略 403。不重试该域名；普通网络并没有全部关掉。安装只走以后批准过的正常包管理，不在这里下载。

用例顺序是 shell、persist、isolate、apt、root-iso、python、proc-fd、maps-caps、sgid。客人里已有 `python3` 时 apt 打出单独一行 `PYTHON_PRESENT`，只安装尚不存在的 `hello`；没有 `python3` 时打出 `MISSING_PYTHON` 并同时安装 `python3-minimal`。`hello` 已经装好仍是未满足的前提。第一次 verifier/seal 的完整 receipt（archive、sha256、bytes、tree、primary、policy、`extracted=true`、`execution_accepted=false`）钉住整次运行；后来的 acquire 若身份不同，先丢掉这次 base，再拒绝，不跑 payload。成功行必须整行等于 `用例: ok` 加这一份身份。裸 `ok`、缺字段或混用身份都不算真通过。`root-iso` 在 apt 之后由 owner A 写 `/etc/fujiang-root-mark`；同号群 owner 和另一名个人 owner 必须先有 `dpkg-query`，marker 不存在，且 `hello` 不是 `install ok installed`，然后 exit 0 打印 `ABSENT`。夹具只做 `bash -n` 和报告绑定，不在宿主执行会写 `/etc` 的客人脚本。这些运行时步骤本机没有执行，不是 Ubuntu apt 通过。先前 `runtime-final` 的 308×3 和 03:05 的 310×3 都是历史门禁。元数据 v3 仍未写。继承的网络命名空间已接上，runner 按文首的 DNS 规则写客人 `etc/resolv.conf`。官方 rootfs 的软件源和一次真实 apt 仍要真正跑过。条数看 `dns-20261108-gate`。不把代理、cgroup 或配额算成这道门。现有 apt/capability 范围不是所有包的兼容证明。若工件带当前导入拒绝的 metadata，或客人没有可用的解析配置，代码路径仍会失败。生产门禁关闭。解包根 0755 的怀疑已撤回，不记成修复。

2026-11-08 02:58 Asia/Shanghai 只读：宿主 uid/gid 1000；当前用户的 `/etc/subuid` 和 `/etc/subgid` 仍都是 `100000:65536`，没有改。`/proc/sys/user/max_user_namespaces` 是 63991。`/usr/bin/unshare --user --map-root-user -- /usr/bin/id -u` 退出 0，stdout 是 `0`。这次没有挂载、没有改文件系统、没有辅助程序、没有网络。它只说明当前用户能做一个简单 user namespace，不是标准 newuidmap 全映射、bwrap 或 apt。`apt-get -s --no-download install gpgv uidmap` 还会带上 `libsubid5`，然后因为缓存是空的无法取得归档；没有下载或安装。只在本任务证据目录里按文件名递归找过，不跟随链接，没有官方工件或 `.deb`。不能据此说整块磁盘都没有。

放到该 inbox 的文件名只应是 `SHA256SUMS`、`SHA256SUMS.gpg`、`ubuntu-archive-keyring.gpg`，以及签名清单选中的 `ubuntu-base-24.04` 或 `ubuntu-base-24.04.<数字>` 的 `base-amd64.tar.gz`。公开地址仍是 `https://cdimage.ubuntu.com/ubuntu-base/releases/24.04/release` 和 `https://archive.ubuntu.com/ubuntu/project/ubuntu-archive-keyring.gpg`。网络恢复并经正常批准后，管理员自己安装 `uidmap` 和 `gpgv`，不在这里下载或安装。

三条验收，分开算：

1. QQ 绑定：`lib.rs` 用当前 `msg.source` 与 `msg.user_id()` 做 `HostTurn`；`resolve` 每次重算。真代码。没有 NapCat 在线。
2. owner 根：`WorkspaceStore::open` 按 personal/group 与正数 id 打开独立目录，真实文件系统夹具。没有官方 Ubuntu Base 工件。
3. 工具路由：`tools/mod.rs` 把当前 `MemoCallScope` 交给 `handle_tool_with`。成功终态回调在测试里是替换的，不是真 bash。

没有真实 bash、Python、Ubuntu 或 apt 的生产端到端。`acceptance.json` 不会由准备脚本自动写成 `approved: true`。

## 本机管理员准备

本机是 Debian 13 amd64。已有 `gpg`、`gpg-agent`、`gpgconf`、`bwrap`、`python3`、`unshare`。`/usr/bin/python3` 是 0777 符号链接，解析后的目标是 root:root 0755。缺三个普通包：`uidmap`（缓存候选 `1:4.17.4-2`）、`libsubid5`（缓存候选 `1:4.17.4-2`）、`gpgv`（缓存候选 `2.4.7-21+deb13u1+b5`）。这是已配置镜像的缓存版本，不是当场联网复核。`.deb` 缓存是空的。`/etc/subuid` 与 `/etc/subgid` 的 `box:100000:65536` 不要改。网络正式恢复并且正常批准之后，由管理员自己运行，不给 payload 宿主 root，这一轮不执行、不重试 DNS：

```sh
sudo apt-get install --no-install-recommends uidmap gpgv
stat -c '%U:%G %a %h %n' /usr/bin/newuidmap /usr/bin/newgidmap
```

官方工件只来自 `scripts/prepare-workspace-rootfs.sh` 里的常量：`https://cdimage.ubuntu.com/ubuntu-base/releases/24.04/release` 上的 `SHA256SUMS` 与 `SHA256SUMS.gpg`，公钥 `https://archive.ubuntu.com/ubuntu/project/ubuntu-archive-keyring.gpg`。归档文件名由签过名的 manifest 决定，不写死 point 版本，不用 cloud-minimal xz。

已有命令用新的绝对目录，父目录私有且为 0700：

```sh
scripts/prepare-workspace-rootfs.sh /绝对/新目录
python3 scripts/verify-workspace-bundle.py --unpack /绝对/source /绝对/私有父/新snapshot
```

没有单独的 shell 验收脚本。`cfg(test)` 管理探针已经接上，复用现有 verifier、seal、`RootfsAuthority`、`execute_with` 和 runner。真实 runtime 仍未验收：本机缺前置和官方工件，官方 metadata 兼容性未验证。生产门禁保持关闭。真实 bash、Python、apt 和 NapCat 都未验收。
