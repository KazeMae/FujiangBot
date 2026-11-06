# Agent skills

父问题是 [#53](https://github.com/KazeMae/FujiangBot/issues/53)。这一篇定三件事：技能包怎么懒加载，QQ 工具按谁的身份放行，定时任务到点时再查一次同一条边界。

T0 是 [#54](https://github.com/KazeMae/FujiangBot/issues/54)，只落地技能包的骨架。QQ 只读是 [#55](https://github.com/KazeMae/FujiangBot/issues/55)。定时任务是 [#56](https://github.com/KazeMae/FujiangBot/issues/56)。[#18](https://github.com/KazeMae/FujiangBot/issues/18) 的 MCP 仍延期。NapCat 动作说明用 [4.18.42 的 API 文档](https://napneko.github.io/api/4.18.42)。本仓库能直接调用的名字以 `crates/napcat-sdk/src/api.rs` 为准，不把那一页上的动作全部挂成工具。

## 为什么要懒加载

业务工具已经有 rank、contest、problem、memo、fun 和分层记忆。NapCat 还有一大组群、私聊、文件和账号动作。每一轮都带上全部参数说明，请求会变长，模型也更容易调到不该调的动作。

常驻的只有一份短目录。模型先看有哪些技能包，点名打开一个，同一次发言的后续回合才带上那个包的参数说明。没打开的包不进 `tools`。

## 一次发言里的打开集合

`assemble_tools` 仍只按开关组装业务工具，并接上 `[[plugins.configs.agent.tools]]`。它不知道技能包。

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

这一节是 [#56](https://github.com/KazeMae/FujiangBot/issues/56) 的边界。T0 不建表，不起时钟，也不注册这些工具。

每条任务记下创建者 QQ、时间、动作、目标。时间是 cron 或一次性的 unix 秒。目标是群号或 QQ。

工具用这四个名字：`schedule.list`、`schedule.create`、`schedule.update`、`schedule.cancel`。用户命令是 `.agent schedule`。

创建时先按创建者 QQ 做上一节的检查。他当时不能做的动作，不能写进任务。

到点由宿主触发。宿主取出任务上的创建者 QQ，再做同一次检查。不沿用创建时的允许结论，也不改用 bot 自己的身份，也不改用那时正好在说话的人。创建者已经退群时，群上的读和发按同一条成员规则拒绝。

定时任务如果要调用 QQ 动作，走和当场发言相同的允许判断。危险写操作仍然只看 `qq_write_allow`。

## 切片

| Issue | 内容 | T0 的状态 |
|---|---|---|
| #53 | 懒加载、QQ 权限、定时任务 | 边界写在这一篇 |
| #54 | `skills.list`、`skills.open`、这篇说明、占位包 | 本分支实现 |
| #55 | 只读技能包 `qq.read`，按发言人核对 | 本分支实现 |
| #56 | `schedule.list`、`schedule.create`、`schedule.update`、`schedule.cancel`，以及 `.agent schedule`。到点重验 | 未做 |
| #18 | MCP | 仍延期 |

可选的后一步是把 fun、contest 这类非核心工具挪进技能包。#53 把它记为 T3，还没有单独的 issue。T0 不动这些业务工具的开关。
