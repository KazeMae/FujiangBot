# Agent skills

父问题是 [#53](https://github.com/KazeMae/FujiangBot/issues/53)。这一篇定四件事：技能包怎么懒加载，QQ 工具按谁的身份放行，定时任务到点时再查一次同一条边界，以及把 fun、contest 从常驻目录挪进技能包。

T0 是 [#54](https://github.com/KazeMae/FujiangBot/issues/54)，只落地技能包的骨架。QQ 只读是 [#55](https://github.com/KazeMae/FujiangBot/issues/55)。定时任务是 [#56](https://github.com/KazeMae/FujiangBot/issues/56)。T3 是 [#60](https://github.com/KazeMae/FujiangBot/issues/60)。[#18](https://github.com/KazeMae/FujiangBot/issues/18) 的 MCP 仍延期。NapCat 动作说明用 [4.18.42 的 API 文档](https://napneko.github.io/api/4.18.42)。本仓库能直接调用的名字以 `crates/napcat-sdk/src/api.rs` 为准，不把那一页上的动作全部挂成工具。

## 为什么要懒加载

常驻业务工具是 rank、problem、memo 和分层记忆。fun 和 contest 在技能包里。NapCat 还有一大组群、私聊、文件和账号动作。每一轮都带上全部参数说明，请求会变长，模型也更容易调到不该调的动作。

常驻的只有一份短目录。模型先看有哪些技能包，点名打开一个，同一次发言的后续回合才带上那个包的参数说明。没打开的包不进 `tools`。

## 一次发言里的打开集合

`assemble_tools` 按开关组装仍留在常驻里的业务工具，并接上 `[[plugins.configs.agent.tools]]`。它不放入技能包。`enable_fun_tools` 和 `enable_contest_tools` 不在这里添加工具。这两个开关只决定 `skills.list` 列不列出 `fun` 和 `contest`。

`skills.list` 和 `skills.open` 没有开关。`resident_tools` 在业务工具和配置项后面加上这两个。`.agent tools` 和 `inject_tool_list` 用的是这一份，此时打开集合是空的，所以看不到包里的工具。

每次用户发言，`lib.rs` 新建一个 `ToolPass`，再调用 `run_tool_loop`。打开集合是这个函数里的 `BTreeSet`，不放在 `ToolPass` 的字段上，也不写入 sqlite 或会话消息。函数返回，集合就没了。下一句发言是一次新的调用，集合重新为空。同一个 `ToolPass` 再跑一遍循环，也看不到上一遍打开的包。

`max_tool_rounds` 仍按一批工具调用计一次。`skills.open` 花掉一次。默认 3 次可以先打开，再调用包里的工具，再回答。到顶时用户仍看到「要查的步骤太多了，我先停在这里。」

打开之后，下一次模型请求才把该包的工具放进 `tools`。执行 `skills.open` 的那一次响应里，别的 `function_call` 仍按打开前的集合判断。所以同一次响应里不能先打开再调用刚打开的工具。

| 逻辑名 | Responses 的 `name` | 作用 |
|---|---|---|
| `skills.list` | `skills_list` | 返回每个包的 `name` 和 `description`。没有参数说明，也没有包内工具名 |
| `skills.open` | `skills_open` | 字符串 `name`。已知的包记入本轮集合。未知的名字按工具失败处理 |

未知名字、空白名字、缺 `name`，这一轮停止。用户看到「我没查成，过一会儿再问我一次。」日志是 `agent skill unknown`。再次打开同一个已知包仍然成功，返回同样的 `{"name":"<包名>","opened":true}`。该包的每个工具在后续请求里只出现一次。

包内工具即使被写进 `[[plugins.configs.agent.tools]]`，未打开时也不进入 `tools`。模型若直接调用它在请求里的函数名，执行阶段拒绝，日志是 `agent skill closed`。只从 `tools` 里拿掉不够。拒绝走同一句失败回复，并且不调用 ServiceHub。

配置项若占用 `skills_list` 或 `skills_open`，请求仍用上面这两个常驻工具的说明。

工具列表在代码里变成空时，请求不带 `tools`，插件只调用一次 `LlmClient::complete`。加上这两个常驻工具之后，正常请求不会走那条分支。

## 占位包 ping

T0 带一个无害的占位包，用来把列出、打开、调用串起来。它不访问 NapCat，不写数据库，也不代替 QQ 或定时任务。

| 项 | 值 |
|---|---|
| 包名 | `ping` |
| 工具 | `ping.pong` |
| Responses 的 `name` | `ping_pong` |
| 打开之后的结果 | `{"pong":true}` |

`skills.list` 的描述只说明打开后会多一个无参数工具。工具名要等后续回合的 `tools` 才出现。

## QQ 权限

这一节是 [#55](https://github.com/KazeMae/FujiangBot/issues/55) 和以后发送类工具要遵守的边界。T0 不注册任何 QQ 工具，也不读 NapCat。

当前发言人是入站消息上的 QQ。宿主写入这个号码。模型传来的 `user_id` 不能换成别人。

读按发言人本人：

- 群只读他所属的群。调用前核对 `group_id` 与这个 QQ 的成员关系。bot 自己在群里不够。不在该群就拒绝。
- 私聊只读他自己和 bot 的会话。`get_friend_msg_history` 的对方必须是这个 QQ。不能读别人的私聊。
- `get_msg` 和 `get_group_msg_history` 先落到某一条消息或某一个群，再按上面两条判断。对不上就拒绝。

发消息可以指定目标，目标仍须在允许范围内：

- 发到群时，目标群必须是发言人所属的群。
- 发到人时，对方是 bot 的好友，或与发言人同在至少一个群里。
- 不能借 bot 去看发言人不在的群，也不能借 bot 去看别人的私聊。

危险写操作默认不注册。以后用配置项 `qq_write_allow` 点名放行，默认是空数组。空数组时一个危险动作都不挂。名单里只放动作名，例如：

- 踢人：`set_group_kick`、`set_group_kick_members`
- 禁言：`set_group_ban`、`set_group_whole_ban`
- 设管理员：`set_group_admin`
- 退群：`set_group_leave`
- 撤回：`delete_msg`
- 凭据：`get_cookies`、`get_credentials`、`get_csrf_token`
- 清缓存、退出、重启类动作：`clean_cache`、`bot_exit`，以及文档里的重启类 action

发言人是群管理员，也不自动打开这些动作。T0 不读取 `qq_write_allow`。

[#55](https://github.com/KazeMae/FujiangBot/issues/55) 只做只读技能包 `qq.read`。发送和危险写操作都不在那一刀里。动作和拒绝方式在下一节。

## 只读包 qq.read

打开之前，包里的工具不进 `tools`。这时直接调用，日志是 `agent skill closed`，不访问 NapCat。`skills.list` 只给包名和这句说明，不给工具名：只读 QQ。打开之后可以查看发言人所在的群，以及他自己和 bot 的私聊。不能发消息，也不能踢人、禁言或读取凭据。

发言人的 QQ 由宿主从入站消息写入，和分层记忆一样。模型传来的 `user_id` 不能把发言人换成别人。

群工具先调用 `get_group_member_info`，参数是目标 `group_id` 和发言人的 QQ。返回的 `user_id` 必须就是发言人。查不到、报错，或返回的是别人，都拒绝，并且不再调用目标动作。bot 自己在群里不够。同一次发言里，同一个群的结论会复用。

| 工具 | Responses 的 `name` | NapCat 动作 | 放行 |
|---|---|---|---|
| `qq.get_group_info` | `qq_get_group_info` | `get_group_info` | 发言人在该群 |
| `qq.get_group_member_info` | `qq_get_group_member_info` | `get_group_member_info` | 发言人在该群。`user_id` 是要查的成员 |
| `qq.get_group_member_list` | `qq_get_group_member_list` | `get_group_member_list` | 发言人在该群 |
| `qq.get_essence_msg_list` | `qq_get_essence_msg_list` | `get_essence_msg_list` | 发言人在该群 |
| `qq.get_group_msg_history` | `qq_get_group_msg_history` | `get_group_msg_history` | 先核对 `group_id`。记录里出现别的群或私聊，整次拒绝 |
| `qq.get_msg` | `qq_get_msg` | `get_msg` | 先取出这条消息，再判断群或私聊。群消息再核对成员。私聊对方必须是发言人。对不上就拒绝，内容不返回 |
| `qq.get_friend_msg_history` | `qq_get_friend_msg_history` | `get_friend_msg_history` | `user_id` 必须是发言人。省略时按发言人查。写成别人就拒绝，并且不调用 NapCat |

越权的日志是 `agent qq denied`。用户仍看到「我没查成，过一会儿再问我一次。」

`get_stranger_info` 不在这个包里。它不是所在的群，也不是自己的私聊。发送和危险写操作也不在，包括 `send_private_msg`、`send_group_msg`、`send_msg`、`set_group_kick`、`set_group_ban`、`set_group_whole_ban`、`set_group_admin`、`set_group_leave`、`delete_msg`、`get_cookies`、`get_credentials`、`get_csrf_token`、`clean_cache`、`bot_exit`。

调用走现有的 `Messenger`。`HttpAdapter` 和 `WsAdapter` 用已经连上的 NapCat 客户端，不另开连接。

## 定时任务

这一节是 [#56](https://github.com/KazeMae/FujiangBot/issues/56)。四个工具放在技能包 `schedule` 里，不进入常驻的十一个工具。参数说明比较长，父问题要求常驻目录保持短。人用 `.agent schedule` 时不经过模型，也不需要 `skills.open`。模型先 `skills.open`，`name` 是 `schedule`，同一次发言的后续回合才看得到这四个工具。

| 逻辑名 | Responses 的 `name` | 作用 |
|---|---|---|
| `schedule.list` | `schedule_list` | 列出自己的任务。新的在前，最多 50 条，包含已停用的 |
| `schedule.create` | `schedule_create` | 新建。`kind` 是 `group` 或 `private` |
| `schedule.update` | `schedule_update` | 修改自己的一条 |
| `schedule.cancel` | `schedule_cancel` | 停用自己的一条 |

`skills.list` 只给包名和一句说明。打开之后，请求里才出现 `schedule_list` 等四个名字。`send_group_msg` 和 `send_private_msg` 始终不在 `tools` 里。发送由到点的宿主完成。

### 创建者

`creator_qq` 由宿主写入当前发言人的 QQ。参数里的 `creator_qq` 被忽略。列表只返回这个发言人的行。更新和取消的条件同时要求 `id` 和 `creator_qq`。别人的编号和没有的编号都失败，那一行保持原样。人看到「没有这条定时任务。」模型看到工具失败回复。

### 这一刀发什么

动作只存 `send_text`。别的 `action` 在创建时拒绝。库里已经有别的动作时，到点把 `last_error` 写成 `denied`，不发送，再按下面的规则推进时间。

群任务要数字 `group_id`，同时写了 `user_id` 就拒绝。创建时和到点时都调用 `get_group_member_info`，参数是这个群和创建者 QQ。返回的 `user_id` 必须等于创建者。这和 `qq.read` 用的是同一个函数。

私聊只发给创建者本人。省略 `user_id` 时写成创建者。写成别人则拒绝，不会改成创建者再插入。到点时目标不是创建者也不发送。这一刀只做本人私聊。到点不调用 `get_friend_msg_history` 或 `get_msg`。

正文去掉首尾空白后要有 1 到 800 个字。时间是五段 `cron` 或正整数 `at`，只能填一个。`at` 是 unix 秒。已经过去或等于现在的 `at`，`next_run_at` 写成现在，下一拍发送。

创建时群成员查询返回不是成员，不插入。人看到「这个目标现在不能发。」查询本身失败也不插入。人看到「现在没查成，过一会儿再试。」模型侧分别是拒绝和工具失败。

### 时间

`cron` 用本机本地时间。五段是分、时、日、月、周。支持 `*`、数字、逗号列表、`a-b`、`*/n` 和 `a-b/n`。周的 `0` 和 `7` 都是周日。日和周都不是单独的 `*` 时，碰上其中一个就到点。下一次从 `after` 之后的下一分钟开始找，最多看大约四年。对不上的表达式拒绝，例如 `0 0 31 2 *`。

一次性任务在这次尝试结束（已发出，或被拒绝）后，`enabled` 变成 0，`next_run_at` 清空。周期任务保持启用，`next_run_at` 挪到下一次。算不出下一次就停用，`last_error` 是 `no next`。

### 表

任务只在 agent 插件 sqlite 的 `scheduled_jobs`。`ensure_schema` 先建 `id` 和 `creator_qq`，再用 `ALTER TABLE` 补列。列是 `cron`、`at`、`action`、`kind`、`group_id`、`user_id`、`payload`、`enabled`、`created_at`、`next_run_at`，另有运行用的 `claim_until`、`claim_token`、`last_error`。旧文件缺列时启动补上，原来的行还在。索引是 `(enabled, next_run_at)`。

### 到点

`on_start` 用 `PluginScope::spawn` 起一个 tokio interval，每 15 秒一拍，第一拍马上执行。错过的拍跳过，不补跑。插件卸载时这个任务先取消，然后 `on_stop` 才关连接池。

每一拍最多领 20 条到期任务。领取是一条 `UPDATE`：`enabled = 1`，`next_run_at` 不晚于现在，并且 `claim_until` 为空或已过期。赢家写入 `claim_until = now + 120` 和 `claim_token`。`rows_affected` 为 1 才继续。发送前再读一次 `claim_token`。取消会清掉 token，这次就不再发。确认更新也要求 token 还在。

成员查询或发送失败时清掉租约，写下 `last_error`，`next_run_at` 不动，下一拍再试。不是成员、动作不是 `send_text`、或私聊目标不是创建者时，不发送，记 `denied`，并按一次性或周期推进。创建时的允许结论不留到点复用。

`qq.read` 把成员查询的传输错误当成不是成员。定时任务把同一次查询的 `Err` 留作重试，`Ok(false)` 才跳过这一次。

发送已经成功、确认更新还没写上时进程停了，租约过期后会再发一次。发送失败会重试。权限拒绝不会把这一次再发一遍。取消若发生在发送已经开始之后，这一条仍可能发出。确认更新对不上 token 时不会改写下一轮时间。

人的命令：

```
.agent schedule list
.agent schedule create group <群号> at <unix秒> <正文>
.agent schedule create group <群号> cron <分> <时> <日> <月> <周> <正文>
.agent schedule create private at <unix秒> <正文>
.agent schedule create private cron <分> <时> <日> <月> <周> <正文>
.agent schedule cancel <编号>
```

人的命令没有 `update`。修改走 `schedule.update`。命令不调用模型，也不计入每小时次数。

## 图库包 fun

这一节是 [#60](https://github.com/KazeMae/FujiangBot/issues/60)。四个只读工具放在技能包 `fun` 里，不进入常驻的十一个工具。开关仍是 `enable_fun_tools`，默认 true。

为 false 时，`skills.list` 不出现 `fun`。`skills.open` 的 `name` 写成 `fun` 按未知包失败，日志是 `agent skill unknown`。这时打开集合里即使已经有 `fun`，请求也不带这四个工具，直接调用的日志是 `agent skill closed`，不调用 ServiceHub。

为 true 时，模型先 `skills.open`，`name` 是 `fun`。同一次发言的后续回合才看得到工具。没打开时直接调用，日志同样是 `agent skill closed`，不调用 ServiceHub。同一次响应里先打开再调用，也按打开前的集合拒绝。

| 逻辑名 | Responses 的 `name` | 调用 | 参数 |
|---|---|---|---|
| `fun.tags` | `fun_tags` | fun 的 `tags` | 无 |
| `fun.stars` | `fun_stars` | fun 的 `stars` | 无 |
| `fun.learn` | `fun_learn` | fun 的 `learn` | 可选字符串 `trigger`。宿主写入当前群 |
| `fun.search` | `fun_search` | fun 的 `search` | 字符串 `q`。宿主写入当前群 |

`skills.list` 只给包名和一句说明，不给工具名。打开之后的执行仍走 ServiceHub。`fun.learn` 和 `fun.search` 由宿主盖掉 `group_id`，群里写成当前群，私聊去掉 `group_id`。fun 不注册写工具。`delete`、`retire`、`add` 这类名字不在这个包里。

配置项若写了同名的 `fun.tags` 等函数名，未打开时不进入 `tools`。打开之后用包里的说明，不用配置项里的那一句。

## 比赛包 contest

这一节同样是 [#60](https://github.com/KazeMae/FujiangBot/issues/60)。两个工具放在技能包 `contest` 里，不进入常驻目录。开关仍是 `enable_contest_tools`，默认 true。列出、打开、未打开时拒绝，以及开关关掉后工具不出现，规则和 `fun` 相同。包名是 `contest`。

| 逻辑名 | Responses 的 `name` | 调用 | 参数 |
|---|---|---|---|
| `contest.upcoming` | `contest_upcoming` | contest 的 `upcoming` | 可选字符串 `oj` |
| `contest.today` | `contest_today` | contest 的 `today` | 无 |

`skills.list` 只给包名和一句说明。打开之后走 ServiceHub。`oj` 用来只看一个 OJ，例如 Codeforces 或 Atcoder。不传则返回全部。

## 切片

| Issue | 内容 | 状态 |
|---|---|---|
| #53 | 懒加载、QQ 权限、定时任务 | 边界写在这一篇。T0–T3 已做。T3 是 #60 |
| #54 | `skills.list`、`skills.open`、这篇说明、占位包 | 已实现（#57） |
| #55 | 只读技能包 `qq.read`，按发言人核对 | 已实现（#58） |
| #56 | 技能包 `schedule`：`schedule.list`、`schedule.create`、`schedule.update`、`schedule.cancel`，以及 `.agent schedule`。到点重验 | 已实现 |
| #60 | 技能包 `fun` 与 `contest`。开关仍是 `enable_fun_tools` / `enable_contest_tools` | Done |
| #18 | MCP | 仍延期 |
