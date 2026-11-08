# 工作区 rootfs

**当前（公开系统运行时，2026-11-08）：** 不再下载或核验 Ubuntu Base，也不再使用批准文件、`gpgv` 或 `newuidmap`。`FUJIANG_WORKSPACE_MVP` 指向 owner 的宿主目录才启用；未设置则执行入口直接失败。生产默认关闭。

种子是一次私人拷贝：`/usr/bin/python3 -I` 在清空环境、5 秒超时下报告解释器和标准库；`/usr/bin/ldd` 必须是 root 拥有、他人不可写、全世界可读的公开文件。只拷贝 `/usr`、`/lib`、`/lib64` 下符合同一条件的解释器、bash、dpkg/apt 程序、动态库和标准库。绝对符号链接不进入客人。库放在客人 `/lib/runtime`，`LD_LIBRARY_PATH` 指向那里。动态链接器按它自己的绝对路径拷贝。若 `/etc/ssl/certs/ca-certificates.crt` 是普通的 root 只读文件，会单独拷这一份，不拷宿主 apt 源、状态或凭据。

runner 使用 bwrap 自带的 `--unshare-user --uid 0 --gid 0`，`--cap-drop ALL`，继承普通网络。启动前要求 `/usr/bin/bwrap --help` 含 `--bind-fd`（看功能，不看版本号）。父进程在 exec 前用 `close_range` 把 3 号及以上标成 cloexec，再只对 root、work、info、block、seccomp 清掉 cloexec。不在 `pre_exec` 里扫描 `/proc/self/fd`。客人 DNS 仍由现有的固定走法写入私人 `etc/resolv.conf`。

单 UID、无 capability 时，root 拥有的本地 `.deb` 可以装进私人 root，并在下次打开后还在。apt 必须看到**绝对**缓存路径；相对文件名会被拒。客人源列表保持为空。若本机已有 apt 的 `file`/`http`/`https` 方法和 `/etc/ssl/certs/ca-certificates.crt`，会拷进私人 root，客人 `SSL_CERT_FILE` 只指向这份沙箱路径。远程 apt 仍未验收：它还需要客人自己的软件源，以及签名验证器（例如 `gpgv`）和密钥材料。这些都没有拷入，也没有测过。多 UID 包仍未验收。`chown` 到未映射 UID 得到 `EINVAL`。

旧的 `scripts/verify-workspace-bundle.py`、`prepare-workspace-rootfs.sh`、`test-workspace-bundle.py`、`test-workspace-rootfs.py` 已删除。CI 不再安装 gpgv，也不再跑这些脚本。QQ 每次调用仍真实 resolve；500000 与 90% 摘要、身份展示不改。

之前 `v3-gc-20261108-gate` 的三遍 319 是旧树的历史门禁，不是这次提交的结果。`869f581` 的本地三遍 Rust 与五份 TLC 通过，不等于远端全绿。CI run `42450488129` 在 bwrap 之前失败：`refused public file /usr/share/dpkg/cputable`（`workspace_backend_tests` 182 与 431）。策略仍是 root 拥有、他人不可写、全世界可读的普通文件。下一次 CI 会 `--reinstall dpkg` 并打印这些表的 uid/gid/mode，拒绝信息也会带上同一组元数据。
