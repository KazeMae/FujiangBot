const RUNNER_UNAVAILABLE: &str =
    "runner unavailable: rootfs authority is not an implemented supplier";

pub fn execute(
    owner: crate::workspace::WorkspaceOwner,
    payload: crate::workspace_runner::Payload,
) -> anyhow::Result<crate::workspace_runner::RunResult> {
    execute_with(owner, payload, &UnacceptedRootfs)
}

fn execute_with(
    owner: crate::workspace::WorkspaceOwner,
    payload: crate::workspace_runner::Payload,
    authority: &impl RootfsAuthority,
) -> anyhow::Result<crate::workspace_runner::RunResult> {
    let owner = map_owner(owner)?;
    let root = authority.verified_workspace_root()?;
    let store = crate::workspace_store::WorkspaceStore::open(&root, owner)?;
    crate::workspace_runner::run(store.rootfs(), std::os::fd::AsFd::as_fd(&store), payload)
}

fn map_owner(
    owner: crate::workspace::WorkspaceOwner,
) -> anyhow::Result<crate::workspace_store::Owner> {
    match owner {
        crate::workspace::WorkspaceOwner::Personal(qq) => {
            let id =
                u64::try_from(qq.get()).map_err(|_| anyhow::anyhow!("invalid workspace owner"))?;
            Ok(crate::workspace_store::Owner::personal(id)?)
        }
        crate::workspace::WorkspaceOwner::Group(group) => {
            let id = u64::try_from(group.get())
                .map_err(|_| anyhow::anyhow!("invalid workspace owner"))?;
            Ok(crate::workspace_store::Owner::group(id)?)
        }
    }
}

trait RootfsAuthority {
    fn verified_workspace_root(&self) -> anyhow::Result<std::path::PathBuf>;
}

struct UnacceptedRootfs;

impl RootfsAuthority for UnacceptedRootfs {
    fn verified_workspace_root(&self) -> anyhow::Result<std::path::PathBuf> {
        Err(anyhow::anyhow!(RUNNER_UNAVAILABLE))
    }
}
