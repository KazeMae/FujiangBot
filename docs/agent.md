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
| `.agent reset` | 删掉当前会话的对话记录和摘要。人设留下。每小时计数也留下 |
| `.agent memory` | 显示当前会话的摘要。还没有摘要时回复「暂无摘要」 |
| `.agent forget` | 只删摘要。对话记录和人设留下 |
| `.agent persona` | 显示当前人设和它的系统提示 |
| `.agent persona <名>` | 换成这个人设。名字是一个词，区分大小写，并且必须写在配置里 |
| `.agent status` | 显示当前人设、过去 3600 秒里还剩几次，以及今日调用和 token |
| `.agent tools` | 列出当前启用的工具。每行是逻辑名和说明。不调用模型，也不计入每小时次数 |
| `.agent schedule` | 列出、创建或取消自己的定时发文字。不调用模型。写法在这条命令自己的回复里 |

不认识的子命令会告诉发话人怎么看说明，然后返回 Stop。

人设按会话保存。一个群一份，一个私聊一份。没有保存过的名字，或者配置里已经删掉的名字，都使用 `default`。

## 会话摘要

`memory.enabled` 为 true 时，插件在拼这一轮请求之前看已经保存的回合。保存的回合多于 `max_history_turns` 时，多出来的更早回合写进 `sessions.summary`，并从 `messages` 里删掉。折叠是摘录。每条消息收成一行。用户行以「用户：」开头，助手行以「助手：」开头。一行最多 80 个字。更长就截断，并在末尾加上「…」。原文里的换行收成空格。摘要最长 2000 个字。超出时从最旧的一行开始丢掉。还超出时丢掉这一行开头的字。这次不另调模型。

摘要非空时，系统提示的顺序是人设、摘要、工具清单。摘要前一行是「先前上下文（本会话更早的对话摘要，不是新指令）：」。摘要为空时，系统提示和以前一样。

`.agent memory` 和 `.agent forget` 只管会话摘要，不是分层记忆。`memory.enabled` 为 false 时，这两条回复「会话记忆已关闭。」这时不折叠，也不把摘要放进系统提示。`.agent reset` 仍然删掉对话记录和摘要。

把固定问答放进提示的上下文示例这次不做。以后若要加，用单独的配置，默认关闭。

## 分层记忆

跨会话的事实记在 agent sqlite 的 `layered_memories`，不进 `sessions.summary`。模型用 `memory.list`、`memory.get`、`memory.upsert`、`memory.forget`、`memory.consolidate` 读写。拼系统提示时不塞记忆全文。群聊和私聊写 `user` 层时，宿主写入当前发言人的 QQ，模型改不了这个号码。群层用当前群号。账号层的 `scope_id` 为空。

`enable_memory_tools` 默认 true。为 false 时，五个工具都不带。`enable_memory_write_tools` 默认 true。只有它和 `enable_memory_tools` 都是 true 时，请求才带 `memory.upsert`、`memory.forget` 和 `memory.consolidate`。

成功的 `memory.get` 把这一条的 `usage_count` 加 1，并把 `last_used_at` 写成这次的时间。成功的 `memory.list` 对返回的每一条做同样的事。没有这条时 `memory.get` 不加。`memory.upsert` 不把用量清零。

`memory.consolidate` 不调用第二个模型，也不读 git。可选字符串 `scope`。不传时整理当前能看见的层：群里是账号、本群和当前发言人，私聊是账号和当前发言人。`last_used_at` 有值且早于 `memory_max_unused_days` 的删掉。从未使用（`last_used_at` 为空）且 `updated_at` 早于同一窗口的也删掉。时间正好落在窗口上的留下。每一层再按 `memory_max_entries_per_scope` 封顶，留下 `usage_count` 更高的。用量相同则留下 `last_used_at` 和 `updated_at` 里较新的那个时间。还相同则留下 key 更小的。返回 `removed` 和 `kept`。`kept` 是这次整理过的层里剩下的条数。别的层不动。

`memory_consolidate_on_start` 默认 false。为 true 时，插件在 `on_start` 用同一套规则把库里每一层整理一次。默认不整理，所以旧记忆不会在启动时被删。

设计见 [分层记忆](layered-memory.md)。不做向量检索，也不做第二轮模型合并。

## 配置

配置写在 `[plugins.configs.agent]`。缺着的字段用下表的默认值。某个字段类型不对时，只这个字段退回默认值。宿主字段 `groups`、`allow_private`、`priority`、`inject`、`provides` 的规则见 [插件说明](plugins.md)。

停用时把 `agent` 写进 `plugins.disabled`。不要加 `[plugins.agent]`。

| 字段 | 默认 | 含义 |
|---|---|---|
| `base_url` | `https://api.deepseek.com` | 对话接口的根地址。插件去掉末尾的 `/` |
| `model` | `deepseek-flash` | 模型名 |
| `api_key_env` | `FUJIANG_LLM_KEY` | 环境变量的名字。这里填名字，不填密钥 |
| `temperature` | `0.7` | 采样温度 |
| `max_history_turns` | `20` | 请求里带上的历史回合数。一回合是一条用户消息加上一条回复。更早的回合在 `memory.enabled` 为 true 时折进摘要 |
| `memory.enabled` | `true` | 为 false 时不折叠历史，也不把摘要放进系统提示。缺省或类型不对时仍是 true |
| `timeout_secs` | `60` | 单次请求的超时秒数。写成 `0` 时仍是 60 秒。工具循环里的每一次模型请求都用这个值 |
| `max_tool_rounds` | `3` | 一轮发言里最多执行几次工具。到顶后模型还要调用工具时，回复一句中文并停下。`0` 表示一次都不执行 |
| `enable_rank_tools` | `true` | 为 false 时，请求不带 `rank_list` 和 `rank_get`。缺省或类型不对时仍是 true |
| `enable_contest_tools` | `true` | 为 false 时，`skills.list` 不列技能包 `contest`，`skills.open` 把它当未知包。为 true 时，打开 `contest` 之后的请求才带 `contest_upcoming` 和 `contest_today`。缺省或类型不对时仍是 true |
| `enable_problem_tools` | `true` | 为 false 时，请求不带 `problem_today`。缺省或类型不对时仍是 true |
| `enable_memo_tools` | `true` | 为 false 时，请求不带 `memo_list`，也不带 `memo_add`。缺省或类型不对时仍是 true |
| `enable_memo_write_tools` | `false` | 为 true 且 `enable_memo_tools` 为 true 时，请求才带 `memo_add`。缺省或类型不对时是 false |
| `enable_fun_tools` | `true` | 为 false 时，`skills.list` 不列技能包 `fun`，`skills.open` 把它当未知包。为 true 时，打开 `fun` 之后的请求才带 `fun_tags`、`fun_stars`、`fun_learn`、`fun_search`。缺省或类型不对时仍是 true |
| `enable_memory_tools` | `true` | 为 false 时，请求不带五个 `memory_*`。缺省或类型不对时仍是 true |
| `enable_memory_write_tools` | `true` | 为 true 且 `enable_memory_tools` 为 true 时，请求才带 `memory_upsert`、`memory_forget` 和 `memory_consolidate`。缺省或类型不对时是 true |
| `memory_max_unused_days` | `90` | 整理时，上次使用早于这么多天的删掉。从未使用的改看 `updated_at`。缺省或类型不对时是 90。写成 `0` 时窗口就是 0 天 |
| `memory_max_entries_per_scope` | `200` | 每一层整理后最多留下这么多条。缺省或类型不对时是 200。写成 `0` 时这一层清空 |
| `memory_consolidate_on_start` | `false` | 为 true 时，启动时把库里每一层整理一次。缺省或类型不对时是 false |
| `inject_tool_list` | `false` | 为 true 时，把当前启用工具的短清单追加到系统提示。缺省或类型不对时是 false |
| `tools` | `[]` | 额外工具。每项有 `name`、`description`、`parameters`。和已启用内置工具的函数名相同时，这一项不发出去 |
| `system_prompt` | `你是福酱，西南科技大学 ACM 群助手。` | `default` 人设的系统提示 |
| `wake_words` | `["福酱"]` | 唤醒词。空数组表示没有唤醒词 |
| `require_at` | `false` | 为 true 时，唤醒词不算叫到 |
| `vision_enabled` | `false` | 为 true 时，把当前消息里的图片发给模型。缺省或类型不对时是 false |
| `max_image_bytes` | `5242880` | 单张图片的字节上限。超过这个数就回复一句中文，不调用模型。缺省、`0` 或类型不对时是 5242880 |
| `rate_limit.per_user_per_hour` | `30` | 每个发言人在过去 3600 秒内的次数，所有会话加在一起。`0` 表示这一项不限 |
| `rate_limit.per_group_per_hour` | `120` | 当前会话在过去 3600 秒内的次数。同一个群里的每个人共用这个会话。一个私聊也算一个会话。`0` 表示这一项不限 |
| `blacklist` | `[]` | 子串列表。命中后回复固定的一句，不调用模型，也不计数。匹配的是去掉唤醒词之后的正文。空数组什么都不拦 |
| `admins` | `[]` | QQ 号。这些人不计数，也不被两项每小时上限拦住。黑名单仍然对他们生效。空数组表示没有人免去次数。`plugins.fun.admins` 是另一份名单，那边的空数组表示所有人 |
| `personas.<名>.system_prompt` | 无 | 额外的人设。`personas.default` 盖过顶层的 `system_prompt` |

插件忽略空白的系统提示、空白的名字，以及不是表的人设项。

```toml
[plugins.configs.agent]
priority = 10
base_url = "https://api.deepseek.com"
model = "deepseek-flash"
api_key_env = "FUJIANG_LLM_KEY"
system_prompt = "你是福酱，西南科技大学 ACM 群助手。"
# blacklist = ["代写"]
# admins = [10001]

[plugins.configs.agent.rate_limit]
per_user_per_hour = 30
per_group_per_hour = 120

[plugins.configs.agent.personas.strict]
system_prompt = "只回答竞赛相关的问题。不确定就说不知道。"
```

`config.example.toml` 里的同一段是注释。抄进 `config.toml` 时不要写上密钥。

客户端只走 OpenAI Responses：`POST {base_url}/responses`。system 拼进 `instructions`，其余回合放进 `input`，从 `output_text` 或 `output` 里的 `output_text` 段取出回复。没有工具时公共方法是 `LlmClient::complete`。有工具时用 `LlmClient::complete_with_tools`，仍是同一个地址。当前消息带图片并且 `vision_enabled` 为 true 时，这一条用户消息的 `content` 是数组，见下文「看图」。更早的回合仍是字符串。若配置里仍写着旧的 `api` 字段，会被忽略。工具规则见下文「工具」。

## 工具

适配层把 Responses 的 `function_call` 交给 ServiceHub。模型返回 `function_call` 时，插件按工具名调用 `ctx.call`。`memory` 例外，直接读写本插件的 sqlite。`skills` 和占位包 `ping` 由本插件直接回答。`qq.read` 和 `schedule` 也由本插件处理，不走 ServiceHub。`fun` 和 `contest` 是技能包，打开之后仍走 ServiceHub。结果放进下一次请求的 `function_call_output`。模型返回文本后，这一轮结束。工具来回只留在这一轮的内存里。会话表仍只保存用户原文和最终回复。分层记忆写在另一张表，见上文「分层记忆」。定时任务写在 `scheduled_jobs`，见下文「数据与部署」。

工具名的格式是 `服务.方法`。第一个 `.` 之前是服务名，之后是方法名。`rank.list` 调用 rank 的 `list`。`rank.get` 调用 rank 的 `get`。`contest.upcoming` 调用 contest 的 `upcoming`。`problem.today` 调用 problem 的 `today`。Responses 要求函数名匹配 `^[a-zA-Z0-9_-]+$`，所以请求里的 `name` 把 `.` 换成 `_`。`rank.list` 发出去是 `rank_list`。`rank.get` 发出去是 `rank_get`。

默认带十一个常驻工具。九个是业务工具：`rank.list`、`rank.get`、`problem.today`、`memo.list`、`memory.list`、`memory.get`、`memory.upsert`、`memory.forget`、`memory.consolidate`。另外两个是 `skills.list` 和 `skills.open`，没有开关。`memo.add` 默认不带。分层记忆的读写默认都带。`fun` 和 `contest` 是技能包，不在这十一个里。fun 不注册写工具，也没有删除图库的方法。其它技能包里的工具也不在这十一个里。`schedule` 是其中一个包，打开名是 `schedule`。四个工具是 `schedule.list`、`schedule.create`、`schedule.update`、`schedule.cancel`，请求里是 `schedule_list`、`schedule_create`、`schedule_update`、`schedule_cancel`。创建者 QQ 由宿主写入当前发言人。到点只发文字：群要求创建者仍是成员，私聊只发给创建者本人。打开规则和投递见 [agent skills](agent-skills.md)。

下表里的 `contest.*` 和 `fun.*` 要等对应的包打开才进入请求。`memo.add` 仍默认不注册。表里其余行在各自开关打开时属于常驻工具。

| 配置名 | 请求里的函数名 | 调用 | 参数 |
|---|---|---|---|
| `rank.list` | `rank_list` | rank 的 `list` | 无 |
| `rank.get` | `rank_get` | rank 的 `get` | 可选字符串 `handle` |
| `contest.upcoming` | `contest_upcoming` | contest 的 `upcoming` | 可选字符串 `oj` |
| `contest.today` | `contest_today` | contest 的 `today` | 无 |
| `problem.today` | `problem_today` | problem 的 `today` | 无 |
| `memo.list` | `memo_list` | memo 的 `list` | 无。宿主写入当前会话 |
| `memo.add` | `memo_add` | memo 的 `add` | 字符串 `body`。默认不注册 |
| `fun.tags` | `fun_tags` | fun 的 `tags` | 无 |
| `fun.stars` | `fun_stars` | fun 的 `stars` | 无 |
| `fun.learn` | `fun_learn` | fun 的 `learn` | 可选字符串 `trigger`。宿主写入当前群 |
| `fun.search` | `fun_search` | fun 的 `search` | 字符串 `q`。宿主写入当前群 |
| `memory.list` | `memory_list` | 本插件 sqlite | 可选字符串 `scope` |
| `memory.get` | `memory_get` | 本插件 sqlite | 字符串 `scope`、`key` |
| `memory.upsert` | `memory_upsert` | 本插件 sqlite | 字符串 `scope`、`key`、`value`。默认注册 |
| `memory.forget` | `memory_forget` | 本插件 sqlite | 字符串 `scope`、`key`。可选布尔 `prefix`。默认注册 |
| `memory.consolidate` | `memory_consolidate` | 本插件 sqlite | 可选字符串 `scope`。默认注册 |
| `skills.list` | `skills_list` | 本插件，不走 ServiceHub | 无。返回技能包的 `name` 和 `description` |
| `skills.open` | `skills_open` | 本插件，不走 ServiceHub | 字符串 `name`。未知名字失败。再次打开仍成功 |

`oj` 用来只看一个 OJ。例如 Codeforces、Atcoder、LuoGu、NowCoder 或 SCPC。不传则返回全部。

`handle` 是已登记的 Codeforces 用户名。不传时 rank 的 `get` 报错。用户看到「我没查成，过一会儿再问我一次。」

`problem.today` 返回数组。每项有 `date`、`band`、`contest_id`、`idx`、`url`。这一天还没有保存的题目时，数组是空的。日期和 `.每日一题` 用同一个规则。过了 `daily_reset_hour` 才换下一天。这个方法只读。调用 `problem.today` 不会抽新题。

`enable_rank_tools` 为 false 时，请求不带 `rank_list` 和 `rank_get`。`enable_problem_tools` 为 false 时，请求不带 `problem_today`。这两个字段缺省时按 true 处理。类型不是布尔值时也按 true 处理。`enable_contest_tools` 同样默认 true。为 false 时，`skills.list` 不列 `contest`，`skills.open` 把它当未知包，请求里也不会出现 `contest_upcoming` 和 `contest_today`。为 true 时，要先 `skills.open`，`name` 是 `contest`，后续回合才带上这两个工具。

`enable_memo_tools` 同样默认 true。为 false 时，请求不带 `memo_list` 和 `memo_add`。`enable_memo_write_tools` 默认 false。只有它和 `enable_memo_tools` 都是 true 时，请求才带 `memo_add`。

`enable_fun_tools` 默认 true。为 false 时，`skills.list` 不列 `fun`，`skills.open` 把它当未知包，请求里也不会出现 `fun_tags`、`fun_stars`、`fun_learn`、`fun_search`。缺省或类型不对时仍是 true。为 true 时，要先 `skills.open`，`name` 是 `fun`，后续回合才带上这四个工具。没有 `enable_fun_write_tools`。fun 服务只有这四个读方法。`delete`、`retire`、`add` 这类名字会报错，图库行还在。

`memory.list` 按层返回键。不传 `scope` 时，群里是账号、本群和当前发言人，私聊是账号和当前发言人。返回的每一条都记一次使用。`memory.get` 没有这条时结果是 null，并且不加用量。读到时记一次使用。`memory.upsert` 的 `value` 最多 500 个字。`user` 的编号是当前发言人，`group` 的编号是当前群，`account` 的编号是空字符串。模型传来的 `user_id` 会被盖掉。`memory.forget` 在 `prefix` 为 true 时按前缀删除。`memory.consolidate` 的规则见上文「分层记忆」。这五个工具不走 ServiceHub。

`memo.list` 返回数组。每项有 `id`、`user_id`、`body`。顺序是新的在前，最多 20 条。这个对话还没有备忘时，数组是空的。模型不传来源。宿主按当前消息写入 `source_kind`、`source_id`、`session`、`user_id`。群是 `group` 和 `group:<id>`。私聊是 `friend` 和 `private:<id>`。这和 `.memo list` 用的是同一张表、同一次会话。直接 `ctx.call("memo", "list", …)` 时可以自己传 `source_kind` 加 `source_id`，或传 `session`。

`memo.add` 的 `body` 是要记下的正文。宿主同样写上来源。正文是空白，或这个对话已经达到 `max_per_source` 时，服务返回错误，用户看到「我没查成，过一会儿再问我一次。」

memo 是动态插件，编成 `libfujiang_plugin_memo.so`（macOS 上是 `.dylib`）后由宿主加载。服务写在 memo crate 里。加载成功时 `on_start` 执行 `ctx.provide("memo", …)`。agent 编进 fujiang，不链接 memo。memo 没加载，或服务调用失败时，`memo_list` 走上面的失败回复。数据仍在插件自己的 SQLite，不进 `fujiang.db`。

fun 编进 fujiang。`on_start` 执行 `ctx.provide("fun", …)`。四个读方法和现有命令读的是同一份数据。`fun.tags` 就是 `.tag list`、`.album`、`.idea`。旧专辑已经并进 tag，所以没有单独的专辑列表。每项有 `name`、`count`、`aliases`。还没有 tag 时是空数组。`fun.stars` 就是不带子命令的 `.star`。每项有 `name`、`url`。还没有收藏时是空数组。

`fun.learn` 就是 `.learn list`。可选字符串 `trigger`，按触发词精确匹配。空白或不传则列出全部。群里只看本群。私聊只看全局词库，也就是 `group_id` 为空的那些行。宿主盖掉模型传来的 `group_id`。直接 `ctx.call("fun", "learn", …)` 时可以自己传整数 `group_id`。还没有学话时是空数组。

`fun.search` 在 tag 的名字和别名、收藏的名称和链接、以及当前会话的学话里做包含匹配。匹配按原文，`cat` 对不上 `Cat`。学话的范围和 `fun.learn` 相同。返回对象有 `tags`、`stars`、`learn` 三个数组，形状和上面三个方法一样。`q` 缺失或空白时，服务返回错误，用户看到「我没查成，过一会儿再问我一次。」查不到时三个数组都是空的，这不是错误。

`[[plugins.configs.agent.tools]]` 可以再追加别的 `服务.方法`。配置项的函数名和某个已启用内置工具相同，就留下内置的那一项。这个内置工具的开关为 false 时，同名配置项会发出去。`fun.*` 和 `contest.*` 走技能包的规则：没打开时不发出去，打开之后用包里的说明。开关为 false 时，这两类名字也不从配置项里发出去。

`skills.list` 和 `skills.open` 每次都在请求里。业务工具仍按各自的开关组装。正常请求因此带 `tools`，并走工具循环。一轮发言里的执行次数不超过 `max_tool_rounds`。打开一个技能包花掉其中一次。工具列表在代码里变成空时，请求不带 `tools`，插件只调用一次 `LlmClient::complete`。当前这两个常驻工具不会把列表变空。

`.agent tools` 打印这一次请求开始时会带上的工具。还没打开的技能包不在里面。顺序是业务内置工具、配置追加的工具、`skills.list`、`skills.open`。每一行先写逻辑名，再写说明。逻辑名是 `服务.方法`，例如 `rank.list`。说明跟在全角冒号后面。请求里的函数名仍把点换成下划线，所以 `rank.list` 发出去是 `rank_list`，`skills.list` 发出去是 `skills_list`。这条命令不调用模型。列表为空时，回复「当前没有可用工具。」当前常驻工具使这份列表不为空。说明为空时，这一行只有逻辑名。

`inject_tool_list` 为 true 时，插件把同一段文字追加到当次人设的系统提示后面。人设和清单之间空一行。清单以「可用工具：」起头。这段文字同样不含尚未打开的技能包。没有启用工具时不追加。为 false 时，系统提示保持人设原文。模型请求里的 `tools` 字段两种情况下都照旧发送。

插件在 `on_start` 打一条 `info` 日志。字段 `tools` 是当时的逻辑名列表。有人发送 `.agent tools` 时再打一条。管理页不单独展示这份清单。配置改完后，下一次 `.agent tools` 用的是新配置。启动时的那条日志要等重启才更新。

次数到顶而模型还要调用工具时，用户看到「要查的步骤太多了，我先停在这里。」工具名不在这一轮的列表里、参数不是 JSON，或 ServiceHub 返回错误时，用户看到「我没查成，过一会儿再问我一次。」插件不退出。到顶和调用失败都会打 `tracing::warn!`。

生产环境的 agent 编进 fujiang，不从 `plugins/` 里的 `.so` 启动。部署细节见下文「数据与部署」。

## 看图

`vision_enabled` 为 false 时，插件不取图片。模型只看到文字。回复开头多一句「我现在还看不了图，先按文字回你。」

为 true 时，插件对当前消息里的每张 `Image` 段调用 `fetch_media_bytes`。视频段不送。历史里仍只存用户原文和最终回复，不存图片字节。

取图失败时，回复「这张图我没取到，过一会儿再发一次。」字节数大于 `max_image_bytes` 时，回复「这张图太大了，我看不了。」内容不是 JPEG、PNG、GIF 或 WebP 时，回复「这张图的格式我看不了。」这三种情况都不调用模型。次数已经记下的这一次仍然算进每小时次数。

请求仍是 `POST {base_url}/responses`。当前这条用户消息的 `content` 变成数组。文字是 `input_text`。图片是 `input_image`，`image_url` 是 `data:` URL。不传 `detail`，也不传 `file_id`。

## 环境变量

`api_key_env` 命名的变量放在 fujiang 进程的环境里。默认名字是 `FUJIANG_LLM_KEY`。变量的值是密钥。插件在每次准备调用模型时读取它。变量不存在，或者值是空白，就回复 `未配置` 加上这个变量名，并且不计入每小时次数。黑名单命中时不走这句回复。

不要把密钥写进 git。`config.toml` 已经在 gitignore 里，这个文件里也不要放密钥。

改这个变量的值之后要重启 fujiang。进程读到的是启动时的环境。NapCat 保持运行。配置里的变量名在下一条消息生效，见下文「配置何时生效」。

## 黑名单

命中时用户看到「这句话我不能接，换个说法再问我。」

比较的是原本要发给模型的那句正文。匹配按子串，并且区分大小写。插件先去掉唤醒词，再把连续空白收成一个空格。正文里还有别的字时，名单里只有唤醒词不会命中。消息只有唤醒词时，正文仍是这个词。去掉唤醒词之后如果没有别的字，正文是 `你好`。名单里的 `你好` 会命中这一条。

`admins` 里的 QQ 也过不了黑名单。

命中后不调用模型，不写每小时次数，也不写 `usage_daily`。`handle` 返回 Stop，后面的插件看不到这条消息。密钥没有配好时，用户看到的仍是上面这句，不是 `未配置`。

`.agent` 不走这份名单。

空白项会被丢掉。不是字符串的项也会被丢掉。

## 每小时次数

黑名单命中时不写行，也不发 HTTP。回复是「这句话我不能接，换个说法再问我。」

没有命中黑名单，并且密钥没有配好时，不写行。回复是 `未配置` 加上 `api_key_env` 里的变量名。

密钥已经读到，并且请求还没发出，才可能记一行。请求失败也算进这一行。

`admins` 里的人不写行。会话上限满了以后，其他人仍然被拦住。这些 QQ 还可以说话。

两项上限都检查。先看发言人，再看会话。`per_group_per_hour` 就是群里的刷屏上限。过了这个数的消息只收到一句中文，不调用模型。这句话会说明是发言人的次数到了，还是当前会话的次数到了。

写成 `0` 的那一项不设上限。两项都是 `0` 时不记行。

窗口从现在往前数 3600 秒。

`.agent status` 按已有的行报还剩几次。`admins` 里的 QQ 不增加行。

`.agent reset` 不清这个计数。命令不计数。

## 配置何时生效

`[plugins.configs.agent]` 里的字段，下一条消息就读到。这包括 `blacklist`、`admins`、`rate_limit`、`api_key_env` 里的名字、`priority`、`groups` 和 `allow_private`。

管理页保存会更新内存里的配置，并重启 agent 插件。重启关掉 sqlite 连接，再打开。库文件、`usage_events`、`usage_daily` 和人设都还在。这次保存的 `restart_required` 是空的。fujiang 进程保持运行。

重启时正在处理的那条消息，可能收到「福酱刚才没想出来，稍后再试。」下一条消息用新配置。

改 `api_key_env` 所指环境变量的值，要重启 fujiang 进程。NapCat 保持运行。

这些宿主字段要重启 fujiang 进程才生效。它们是 `store.db`、`store.image_root`、`store.archive_dir`、`admin.listen`、`plugins.dir` 和 `plugins.watch`。

agent 编进 fujiang。不要热替换它的 `.so`。宿主拒绝用 `.so` 覆盖内置的 `agent`。

## 每日用量

`usage_daily` 按 UTC 日、会话和发言人各存一行。列是 `calls`、`in_tokens`、`out_tokens`。同一天的下一次调用加进这一行。下一天另起一行。`day` 等于 Unix 时间除以 86400。日界是 UTC 0 点。

`calls` 计 Responses 请求。每小时次数计的是一次发言。工具来回里的每一次模型请求都算 1 次。请求失败、超时，或响应里没有 `usage` 时，这一次仍然计入 `calls`，token 记 0。

响应里有 `usage` 时，`in_tokens` 加上 `input_tokens`，`out_tokens` 加上 `output_tokens`。缺字段、负数，或不是整数时，这一项按 0。插件不记 `total_tokens`。

正文解析失败时，`calls` 加 1，token 记 0。解析失败的响应里如果带了 `usage`，那些 token 不计入。

`.agent status` 显示今天的发言人合计和会话合计。今日你是这个发言人在所有会话里的合计。今日会话是这个会话里所有发言人的合计。没有记录时两个合计都是 0。

命令不调用模型，所以不增加这张表。`.agent reset` 不删这张表。两项每小时上限都是 0 时，模型请求仍然记入每日用量。

黑名单命中、密钥没读到、次数已经用完，或图片被拒绝时，还没有 Responses 请求，不记每日用量。

## 数据与部署

这些表在插件自己的 SQLite 里，不在 `fujiang.db`。`store.db` 为 `data/fujiang.db` 时，默认实例的文件是 `data/plugin-data/agent/plugin.sqlite`。额外实例在 `share_data` 不是 true 时，目录名是 `agent__` 加上实例 id。

`sessions.persona` 保存当前人设的名字。`sessions.summary` 保存这个会话的摘要。旧文件里没有这两列时，插件启动会补上，原来的行还在。`.agent reset` 清摘要。`.agent forget` 只清摘要。两次都留下人设。`layered_memories` 保存跨会话的分层记忆。列是 `scope_kind`、`scope_id`、`key`、`value`、`updated_at`、`source`、`usage_count`、`last_used_at`。同一层同一个键只有一行。旧文件没有这张表时，插件启动会补上。旧表没有 `usage_count` 或 `last_used_at` 时，启动会 `ALTER` 补上，原来的行还在。`usage_count` 从 0 起，`last_used_at` 为空。`scheduled_jobs` 保存定时发文字。列是 `id`、`creator_qq`、`cron`、`at`、`action`、`kind`、`group_id`、`user_id`、`payload`、`enabled`、`created_at`、`next_run_at`，以及领取用的 `claim_until`、`claim_token`、`last_error`。建表时先有 `id` 和 `creator_qq`，其余列由启动时的 `ALTER` 补上。旧文件缺列时补上，原来的行还在。`usage_events` 保存次数，列是 `session_key`、`user_id`、`ts`。`usage_daily` 保存每日用量，列是 `day`、`session_key`、`user_id`、`calls`、`in_tokens`、`out_tokens`。这张表不在 `fujiang.db`。

换 fujiang 之前，先备份这个 sqlite 文件，以及旁边的 `-wal` 文件和 `-shm` 文件。备份范围和换二进制的步骤见 [分支管理手册](BRANCHING.md) 第 7 节。这次不改 `fujiang.db`。

Linux 生产把 agent 链进 fujiang，不从 `.so` 启动。`.so` 自己带一份 tokio。`on_start` 走到 `open_instance_db` 时，插件里的 `tokio::fs` 找不到宿主 runtime，进程会 abort。宿主发现内置名字 `agent` 后会拒绝用 `.so` 覆盖，扫描只打警告。不要把 `libfujiang_plugin_agent.so` 放进 `plugins/`。cdylib 热更新以后再做。

发布时编宿主。

```bash
cargo build -p fujiang --release
```

`cargo build -p fujiang-plugin-agent --release` 仍会生成 `target/release/libfujiang_plugin_agent.so`。macOS 上是同名的 `.dylib`。这个文件留给以后的热更新，不要拷进 `plugins/`。`PLUGIN_ABI` 仍然是 5。换二进制时 NapCat 保持运行。
