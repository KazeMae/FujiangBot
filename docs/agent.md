# agent 插件

agent 是动态插件。有人在群里或私聊里叫到它时，它把这个会话交给配置的模型，再把回复发回同一个会话。它处理过的消息返回 Stop，后面的插件不会再看到这条消息。

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

不认识的子命令会告诉发话人怎么看说明，然后返回 Stop。

人设按会话保存。一个群一份，一个私聊一份。没有保存过的名字，或者配置里已经删掉的名字，都使用 `default`。

## 配置

配置写在 `[plugins.configs.agent]`。缺着的字段用下表的默认值。某个字段类型不对时，只这个字段退回默认值。宿主字段 `groups`、`allow_private`、`priority`、`inject`、`provides` 的规则见 [插件说明](plugins.md)。

| 字段 | 默认 | 含义 |
|---|---|---|
| `base_url` | `https://api.deepseek.com` | 对话接口的根地址。插件去掉末尾的 `/` |
| `model` | `deepseek-flash` | 模型名 |
| `api` | `chat` | 协议：`chat` 走 `POST {base_url}/chat/completions`；`responses`（别名 `response`）走 `POST {base_url}/responses`（OpenAI Responses：`input` / `instructions` / `output`）。默认 `chat`，DeepSeek 现网默认行为不变。未知值按 `chat` |
| `api_key_env` | `FUJIANG_LLM_KEY` | 环境变量的名字。这里填名字，不填密钥 |
| `temperature` | `0.7` | 采样温度 |
| `max_history_turns` | `20` | 请求里带上的历史回合数。一回合是一条用户消息加上一条回复 |
| `timeout_secs` | `60` | 单次请求的超时秒数。写成 `0` 时仍是 60 秒 |
| `system_prompt` | `你是福酱，西南科技大学 ACM 群助手。` | `default` 人设的系统提示 |
| `wake_words` | `["福酱"]` | 唤醒词。空数组表示没有唤醒词 |
| `require_at` | `false` | 为 true 时，唤醒词不算叫到 |
| `rate_limit.per_user_per_hour` | `30` | 每个发言人在过去 3600 秒内的次数，所有会话加在一起。`0` 表示这一项不限 |
| `rate_limit.per_group_per_hour` | `120` | 当前会话在过去 3600 秒内的次数。一个私聊也算一个会话。`0` 表示这一项不限 |
| `personas.<名>.system_prompt` | 无 | 额外的人设。`personas.default` 盖过顶层的 `system_prompt` |

插件忽略空白的系统提示、空白的名字，以及不是表的人设项。

```toml
[plugins.configs.agent]
priority = 10
base_url = "https://api.deepseek.com"
model = "deepseek-flash"
api = "chat"   # 或 "responses"
api_key_env = "FUJIANG_LLM_KEY"
system_prompt = "你是福酱，西南科技大学 ACM 群助手。"

[plugins.configs.agent.rate_limit]
per_user_per_hour = 30
per_group_per_hour = 120

[plugins.configs.agent.personas.strict]
system_prompt = "只回答竞赛相关的问题。不确定就说不知道。"
```

`config.example.toml` 里的同一段是注释。抄进 `config.toml` 时不要写上密钥。

调用方仍走同一套消息拼装；`api = "responses"` 时客户端把 system 拼进 `instructions`，其余回合放进 `input`，并从 `output_text` 或 `output` 里的 `output_text` 段取出回复。

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

换这个版本之前，先备份这个 sqlite 文件，以及旁边的 `-wal` 文件和 `-shm` 文件。备份范围和换二进制的步骤见 [分支管理手册](BRANCHING.md) 第 7 节。这次只改插件库，不改主库。

发布构建的命令如下。

```bash
cargo build -p fujiang-plugin-agent --release
```

Linux 上的产物是 `target/release/libfujiang_plugin_agent.so`。macOS 上是同名的 `.dylib`。把它放到 `[plugins].dir`。默认目录是 `plugins/`。`plugins.watch` 为 true 时，换掉这个文件会热加载，并在启动时补上新列和新表。

只换这个 `.so` 时，工作区 version 保持不动，NapCat 保持运行。`PLUGIN_ABI` 仍然是 5。
