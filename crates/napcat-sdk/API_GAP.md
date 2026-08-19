# napcat-sdk vs NapCat 4.18.19

Typed wrappers start from [node-napcat-ts](https://github.com/HkTeamX/node-napcat-ts) (`NCWebsocketApi.ts`, based on napcat-v4.12.2) and add the 4.18-era actions already listed there (`get_rkey`, `get_rkey_server`, `set_group_robot_add_option`, `set_group_add_option`, `set_group_search`, `get_doubt_friends_add_request`, `send_poke`, AI record, file move/rename, etc.).

Anything not given a named method can still be called:

```rust
client.call("some_action", serde_json::json!({ "foo": 1 })).await?;
```

When a 4.18.19 action is missing a wrapper, add a thin method in `src/api.rs` and note it here.

Checked against node-napcat-ts surface (2026-03). Official page https://napneko.github.io/api/4.18.19 is a SPA; new actions should be appended as they are discovered in live NapCat `get_version_info` / docs.
