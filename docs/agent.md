# agent 插件

agent 编进 fujiang 本体，随进程启动。有人在群里或私聊里叫到它时，它把这个会话交给配置的模型，再把回复发回同一个会话。它处理过的消息返回 Stop，后面的插件不会再看到这条消息。

## 触发

叫到它的条件如下。

- 消息 @ 了这个账号。
- 消息回复了这个账号发出的上一条。
- `require_at` 为 false，并且正文里出现 `wake_words` 中的某个词。匹配按子串。插件先去掉正文开头连续的 `@名字`。

`require_at` 为 true 时，只认 @ 和回复。这时单独出现唤醒词不算叫到。

`.agent` 不走上面的条件。这条命令不调用模型，也不计入每小时次数。`require_at` 管不到它。命令必须是消息文本的第一个词。@ 如果是单独的消息段，文本仍然可以从 `.agent` 开始。

唤醒词要靠优先级排到 fun 前面。在 `[plugins.configs.agent]` 里把 `priority` 设为 10。fun 的默认优先级是 0。命令由命令主人先分发，不靠这个数字。

## 命令

实际前缀是 `bot.command_prefix`。下表按默认的 `.` 来写。

| 命令 | 作用 |
|---|---|
| `.agent` | 打印说明 |
| `.agent reset` | 删掉当前会话的对话记录。人设留下。每小时计数也留下 |
| `.agent persona` | 显示当前人设和它的系统提示 |
| `.agent persona <名>` | 换成这个人设。名字是一个词，区分大小写，并且必须写在配置里 |
| `.agent status` | 显示当前人设，以及过去 3600 秒里还剩几次 |
| `.agent tools` | 列出当前启用的工具。每行是逻辑名和说明。不调用模型，也不计入每小时次数 |

不认识的子命令会告诉发话人怎么看说明，然后返回 Stop。

人设按会话保存。一个群一份，一个私聊一份。没有保存过的名字，或者配置里已经删掉的名字，都使用 `default`。

## 配置

配置写在 `[plugins.configs.agent]`。缺着的字段用下表的默认值。某个字段类型不对时，只这个字段退回默认值。宿主字段 `groups`、`allow_private`、`priority`、`inject`、`provides` 的规则见 [插件说明](plugins.md)。

停用时把 `agent` 写进 `plugins.disabled`。不要加 `[plugins.agent]`。

| 字段 | 默认 | 含义 |
|---|---|---|
| `base_url` | `https://api.deepseek.com` | 对话接口的根地址。插件去掉末尾的 `/` |
| `model` | `deepseek-flash` | 模型名 |
| `api_key_env` | `FUJIANG_LLM_KEY` | 环境变量的名字。这里填名字，不填密钥 |
| `temperature` | `0.7` | 采样温度 |
| `max_history_turns` | `20` | 请求里带上的历史回合数。一回合是一条用户消息加上一条回复 |
| `timeout_secs` | `60` | 单次请求的超时秒数。写成 `0` 时仍是 60 秒。工具循环里的每一次模型请求都用这个值 |
| `max_tool_rounds` | `3` | 一轮发言里最多执行几次工具。到顶后模型还要调用工具时，回复一句中文并停下。`0` 表示一次都不执行 |
| `enable_rank_tools` | `true` | 为 false 时，请求不带 `rank_list` 和 `rank_get`。缺省或类型不对时仍是 true |
| `enable_contest_tools` | `true` | 为 false 时，请求不带 `contest_upcoming` 和 `contest_today`。缺省或类型不对时仍是 true |
| `enable_problem_tools` | `true` | 为 false 时，请求不带 `problem_today`。缺省或类型不对时仍是 true |
| `inject_tool_list` | `false` | 为 true 时，把当前启用工具的短清单追加到系统提示。缺省或类型不对时是 false |
| `tools` | `[]` | 额外工具。每项有 `name`、`description`、`parameters`。和已启用内置工具的函数名相同时，这一项不发出去 |
| `system_prompt` | `你是福酱，西南科技大学 ACM 群助手。` | `default` 人设的系统提示 |
| `wake_words` | `["福酱"]` | 唤醒词。空数组表示没有唤醒词 |
| `require_at` | `false` | 为 true 时，唤醒词不算叫到 |
| `vision_enabled` | `false` | 为 true 时，把当前消息里的图片发给模型。缺省或类型不对时是 false |
| `max_image_bytes` | `5242880` | 单张图片的字节上限。超过这个数就回复一句中文，不调用模型。缺省、`0` 或类型不对时是 5242880 |
| `rate_limit.per_user_per_hour` | `30` | 每个发言人在过去 3600 秒内的次数，所有会话加在一起。`0` 表示这一项不限 |
| `rate_limit.per_group_per_hour` | `120` | 当前会话在过去 3600 秒内的次数。一个私聊也算一个会话。`0` 表示这一项不限 |
| `personas.<名>.system_prompt` | 无 | 额外的人设。`personas.default` 盖过顶层的 `system_prompt` |

插件忽略空白的系统提示、空白的名字，以及不是表的人设项。

```toml
[plugins.configs.agent]
priority = 10
base_url = "https://api.deepseek.com"
model = "deepseek-flash"
api_key_env = "FUJIANG_LLM_KEY"
system_prompt = "你是福酱，西南科技大学 ACM 群助手。"

[plugins.configs.agent.rate_limit]
per_user_per_hour = 30
per_group_per_hour = 120

[plugins.configs.agent.personas.strict]
system_prompt = "只回答竞赛相关的问题。不确定就说不知道。"
```

`config.example.toml` 里的同一段是注释。抄进 `config.toml` 时不要写上密钥。

客户端只走 OpenAI Responses：`POST {base_url}/responses`。system 拼进 `instructions`，其余回合放进 `input`，从 `output_text` 或 `output` 里的 `output_text` 段取出回复。没有工具时公共方法是 `LlmClient::complete`。有工具时用 `LlmClient::complete_with_tools`，仍是同一个地址。当前消息带图片并且 `vision_enabled` 为 true 时，这一条用户消息的 `content` 是数组，见下文「看图」。更早的回合仍是字符串。若配置里仍写着旧的 `api` 字段，会被忽略。工具规则见下文「工具」。

## 工具

适配层把 Responses 的 `function_call` 交给 ServiceHub。模型返回 `function_call` 时，插件按工具名调用 `ctx.call`。结果放进下一次请求的 `function_call_output`。模型返回文本后，这一轮结束。工具来回只留在这一轮的内存里。会话表仍只保存用户原文和最终回复，不新增表。

工具名的格式是 `服务.方法`。第一个 `.` 之前是服务名，之后是方法名。`rank.list` 调用 rank 的 `list`。`rank.get` 调用 rank 的 `get`。`contest.upcoming` 调用 contest 的 `upcoming`。`problem.today` 调用 problem 的 `today`。Responses 要求函数名匹配 `^[a-zA-Z0-9_-]+$`，所以请求里的 `name` 把 `.` 换成 `_`。`rank.list` 发出去是 `rank_list`。`rank.get` 发出去是 `rank_get`。

默认带五个内置工具。

| 配置名 | 请求里的函数名 | 调用 | 参数 |
|---|---|---|---|
| `rank.list` | `rank_list` | rank 的 `list` | 无 |
| `rank.get` | `rank_get` | rank 的 `get` | 可选字符串 `handle` |
| `contest.upcoming` | `contest_upcoming` | contest 的 `upcoming` | 可选字符串 `oj` |
| `contest.today` | `contest_today` | contest 的 `today` | 无 |
| `problem.today` | `problem_today` | problem 的 `today` | 无 |

`oj` 用来只看一个 OJ。例如 Codeforces、Atcoder、LuoGu、NowCoder 或 SCPC。不传则返回全部。

`handle` 是已登记的 Codeforces 用户名。不传时 rank 的 `get` 报错。用户看到「我没查成，过一会儿再问我一次。」

`problem.today` 返回数组。每项有 `date`、`band`、`contest_id`、`idx`、`url`。这一天还没有保存的题目时，数组是空的。日期和 `.每日一题` 用同一个规则。过了 `daily_reset_hour` 才换下一天。这个方法只读。调用 `problem.today` 不会抽新题。

`enable_rank_tools` 为 false 时，请求不带 `rank_list` 和 `rank_get`。`enable_contest_tools` 为 false 时，请求不带 `contest_upcoming` 和 `contest_today`。`enable_problem_tools` 为 false 时，请求不带 `problem_today`。字段缺省时按 true 处理。类型不是布尔值时也按 true 处理。

`[[plugins.configs.agent.tools]]` 可以再追加别的 `服务.方法`。配置项的函数名和某个已启用内置工具相同，就留下内置的那一项。这个内置工具的开关为 false 时，同名配置项会发出去。

组装之后如果一个工具都没有，请求不带 `tools` 字段，插件只调用一次 `LlmClient::complete`。有工具就走工具循环。一轮发言里的执行次数不超过 `max_tool_rounds`。

`.agent tools` 打印这一次会带上的工具。顺序和请求一致。内置工具在前，后面是配置追加的工具。每一行先写逻辑名，再写说明。逻辑名是 `服务.方法`，例如 `rank.list`。说明跟在全角冒号后面。请求里的函数名仍把点换成下划线，所以 `rank.list` 发出去是 `rank_list`。这条命令不调用模型。一个工具都没有时，回复「当前没有可用工具。」说明为空时，这一行只有逻辑名。

`inject_tool_list` 为 true 时，插件把同一段文字追加到当次人设的系统提示后面。人设和清单之间空一行。清单以「可用工具：」起头。没有启用工具时不追加。为 false 时，系统提示保持人设原文。模型请求里的 `tools` 字段两种情况下都照旧发送。

插件在 `on_start` 打一条 `info` 日志。字段 `tools` 是当时的逻辑名列表。有人发送 `.agent tools` 时再打一条。管理页不单独展示这份清单。配置改完后，下一次 `.agent tools` 用的是新配置。启动时的那条日志要等重启才更新。

次数到顶而模型还要调用工具时，用户看到「要查的步骤太多了，我先停在这里。」工具名不在这一轮的列表里、参数不是 JSON，或 ServiceHub 返回错误时，用户看到「我没查成，过一会儿再问我一次。」插件不退出。到顶和调用失败都会打 `tracing::warn!`。

生产环境的 agent 编进 fujiang，不从 `plugins/` 里的 `.so` 启动。部署细节见下文「数据与部署」。

## 看图

`vision_enabled` 为 false 时，插件不取图片。模型只看到文字。回复开头多一句「我现在还看不了图，先按文字回你。」

为 true 时，插件对当前消息里的每张 `Image` 段调用 `fetch_media_bytes`。视频段不送。历史里仍只存用户原文和最终回复，不存图片字节。

取图失败时，回复「这张图我没取到，过一会儿再发一次。」字节数大于 `max_image_bytes` 时，回复「这张图太大了，我看不了。」内容不是 JPEG、PNG、GIF 或 WebP 时，回复「这张图的格式我看不了。」这三种情况都不调用模型。次数已经记下的这一次仍然算进每小时次数。

请求仍是 `POST {base_url}/responses`。当前这条用户消息的 `content` 变成数组。文字是 `input_text`。图片是 `input_image`，`image_url` 是 `data:` URL。不传 `detail`，也不传 `file_id`。

## 环境变量

`api_key_env` 命名的变量放在 fujiang 进程的环境里。默认名字是 `FUJIANG_LLM_KEY`。变量的值是密钥。插件在每次准备调用模型时读取它。变量不存在，或者值是空白，就回复 `未配置` 加上这个变量名，并且不计入每小时次数。

不要把密钥写进 git。`config.toml` 已经在 gitignore 里，这个文件里也不要放密钥。

改环境变量之后要重启 fujiang。进程读到的是启动时的环境。NapCat 保持运行。

## 每小时次数

插件在密钥已经读到、请求还没发出的时候记一行。请求失败也算进这一行。次数已经用完时，不记新的一行，也不发 HTTP，只回复一句中文。这句话会说明是发言人的次数到了，还是当前会话的次数到了。命令不计数。

两项上限都检查。先看发言人，再看会话。

写成 `0` 的那一项不设上限。两项都是 `0` 时不记行。

窗口从现在往前数 3600 秒。

`.agent reset` 不清这个计数。

## 数据与部署

这些表在插件自己的 SQLite 里，不在 `fujiang.db`。`store.db` 为 `data/fujiang.db` 时，默认实例的文件是 `data/plugin-data/agent/plugin.sqlite`。额外实例在 `share_data` 不是 true 时，目录名是 `agent__` 加上实例 id。

`sessions.persona` 保存当前人设的名字。旧文件里没有这一列时，插件启动会补上，原来的行还在。`usage_events` 保存次数，列是 `session_key`、`user_id`、`ts`。

换 fujiang 之前，先备份这个 sqlite 文件，以及旁边的 `-wal` 文件和 `-shm` 文件。备份范围和换二进制的步骤见 [分支管理手册](BRANCHING.md) 第 7 节。这次不改 `fujiang.db`。

Linux 生产把 agent 链进 fujiang，不从 `.so` 启动。`.so` 自己带一份 tokio。`on_start` 走到 `open_instance_db` 时，插件里的 `tokio::fs` 找不到宿主 runtime，进程会 abort。宿主发现内置名字 `agent` 后会拒绝用 `.so` 覆盖，扫描只打警告。不要把 `libfujiang_plugin_agent.so` 放进 `plugins/`。cdylib 热更新以后再做。

发布时编宿主。

```bash
cargo build -p fujiang --release
```

`cargo build -p fujiang-plugin-agent --release` 仍会生成 `target/release/libfujiang_plugin_agent.so`。macOS 上是同名的 `.dylib`。这个文件留给以后的热更新，不要拷进 `plugins/`。`PLUGIN_ABI` 仍然是 5。换二进制时 NapCat 保持运行。
