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

## 历史图片

`view_image`（请求里的函数名；逻辑名 `view.image`）让模型按聊天记录里的占位符回看图片。当前消息的每张图写成 `[图片#消息号:序号]`，引用消息在本会话里时写成 `[引用图片#消息号:序号]`。占位符是纯文字，进会话表，不另存图。工具只在 `vision_enabled` 且该模型有已核实的图像 token 上界时出现在工具定义和清单里。编号只按这一轮真实会话查：群查 `msg_g{群号}`，私聊查 `msg_pm` 里 `user_id` 等于对方的行。没有行时才 `get_message`，来源必须与当前会话一致。每次最多 3 张，每轮最多 6 张。超预算、超传输限制或没有图像上界时，这张图不放进请求，工具结果是「超出上下文预算」。

### 真实通过

同群占位符能解析，下一次模型请求带 `input_image` 的 data URL。别的群、别的私聊、私聊里要群消息、`get_message` 来源不一致，都返回「不在当前会话或已过期」，并且不下载。编号格式、越界、一次超过 3 张、一轮超过 6 张、取图失败、过大和格式不对，都是工具结果里的中文，不中断这一轮。`request_bytes` 按已核实的图像上界为先前看图逐张加价。超预算时发出去的请求没有这张图。

### 模拟

存储行缺失时用假的 `get_message` 和假的字节。预算用本地 mock 的上下文上限。

### 未验收

线上 QQ / NapCat。图片 URL 过期后，真实 `get_message` 换到的新链接能否再下载，测试没有打到 NapCat。真实模型服务商是否接受 `function_call_output` 之后紧跟带 `input_image` 的 user 消息，只在本地 mock 上验证。

## 已删模块

`workspace_runtime`、`workspace_runtime_tests`、`workspace_apt`、`workspace_resolv`、`workspace_resolv_gc_tests`、`workspace_resolv_reclaim`、`workspace_seed`、`workspace_seed_copy`、`workspace_seed_tests`、`workspace_overlay`。私人 root、链接物化、DNS/resolver 拷贝与回收、私人 apt 源和钥匙环，以及对应 CI 步骤一并去掉。
