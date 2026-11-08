# PR 62 正文草稿

不要用 API 更新这条 PR。上次正文写入返回 403，到此为止。

这次把工作区从 Ubuntu 归档验收改成公开系统运行时的私人副本。默认仍然关闭，只有设置 `FUJIANG_WORKSPACE_MVP` 为 owner 宿主目录才会准备 root。bwrap 使用自身的单 UID 用户命名空间，丢掉全部 capability。父进程在 exec 前用 `close_range` 把无关描述符标成 cloexec，并保留 `--bind-fd`。启动要求 `bwrap --help` 含 `--bind-fd`。归档模型 `Bundle.tla` 已删除。

已在本地跑通：bash、Python、`/work` 再次打开仍在，以及一个自建 `.deb` 经 `apt-get install --no-download` 从绝对缓存路径装入后再次打开仍在（子进程退出码 0，且是 newly installed）。宿主哨兵和其他 owner 的字节在故意继承可读可写描述符以及目录描述符之后保持不变。客人内部的硬链接和符号链接对照成功。

私人 root 可以带上本机已有的 apt `http`/`https` 方法和 `/etc/ssl/certs/ca-certificates.crt`。标准库符号链接只在最终仍位于 `/usr/lib`、`/usr/lib64`、`/lib`、`/lib64` 时物化成私人普通文件。唯一省略的是 `/usr/lib/python3.<数字>/sitecustomize.py`（或 `lib64`）词法指向 `/etc/python3.<同一数字>/sitecustomize.py`，不读 `/etc`。其它越界失败。目录符号链接拒绝。

历史：CI `42452313303`（`a538be7`）205 通过、2 失败，是 `unavailable: resolver parent`。已验证的整体绿是 `6e28eb9` 的 CI `42453053742`（2026-11-08 14:06 Asia/Shanghai）；日志里 `resolve` 目录 uid 989、mode 755，stub 是 644。

远程 apt 未验收。CI `42454743976`（`983f41b`）209 通过、3 失败、1 忽略：`default-sequoia.config` 在 CI 上是 root 但 mode 100777，信任检查正确地拒绝了。CI 用所属包的普通重装恢复元数据，还不能写成已绿。Debian/Ubuntu 的官方 HTTPS 源和公钥环种进私人 root，并且是正常启用的；种的时候不拉取。`APT::Sandbox::User "root"` 是 bwrap 用户命名空间里的 uid 0，映射到宿主非特权 uid，不是宿主 root。owner 里对 `deb.debian.org` 的 `apt-get update` 得到 `Temporary failure resolving`，这是 DNS 失败，没有证明策略拒绝。不再由代理重试。本地夹具已通过正确密钥安装、错钥和篡改拒绝，以及 CA 拷贝失败清理；宿主和其他 owner 未改。这不是官方安装。被忽略的 `sensible-utils` 验收没有跑，不能写成通过。单 UID、无 capability，不安装系统服务。在线 QQ 未验收。生产默认关闭。
