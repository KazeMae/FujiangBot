# PR 62 正文草稿

不要用 API 更新这条 PR。上次正文写入返回 403，到此为止。

这次把工作区从 Ubuntu 归档验收改成公开系统运行时的私人副本。默认仍然关闭，只有设置 `FUJIANG_WORKSPACE_MVP` 为 owner 宿主目录才会准备 root。bwrap 使用自身的单 UID 用户命名空间，丢掉全部 capability。父进程在 exec 前用 `close_range` 把无关描述符标成 cloexec，并保留 `--bind-fd`。启动要求 `bwrap --help` 含 `--bind-fd`。归档模型 `Bundle.tla` 已删除。

已在本地跑通：bash、Python、`/work` 再次打开仍在，以及一个自建 `.deb` 经 `apt-get install --no-download` 从绝对缓存路径装入后再次打开仍在（子进程退出码 0，且是 newly installed）。宿主哨兵和其他 owner 的字节在故意继承可读可写描述符以及目录描述符之后保持不变。客人内部的硬链接和符号链接对照成功。

私人 root 可以带上本机已有的 apt `http`/`https` 方法和 `/etc/ssl/certs/ca-certificates.crt`。远程 apt 没有通过：还缺客人软件源、签名验证器和密钥。没有多 UID 包，没有官方 rootfs。`chown` 到未映射 UID 会失败。
