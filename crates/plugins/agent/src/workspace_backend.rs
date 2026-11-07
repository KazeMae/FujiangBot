use std::os::fd::{AsFd, BorrowedFd};
use std::path::PathBuf;

const RUNNER_UNAVAILABLE: &str = "accepted official rootfs unavailable";

pub(crate) use crate::workspace_store::VerifiedBase;

pub(crate) fn production_available() -> bool {
    UnacceptedRootfs.acquire().is_ok()
}

pub fn execute(
    owner: crate::workspace::WorkspaceOwner,
    payload: crate::workspace_runner::Payload,
) -> anyhow::Result<crate::workspace_runner::RunResult> {
    execute_with(
        owner,
        payload,
        &UnacceptedRootfs,
        crate::workspace_runner::run,
    )
}

pub(crate) fn execute_with(
    owner: crate::workspace::WorkspaceOwner,
    payload: crate::workspace_runner::Payload,
    authority: &impl RootfsAuthority,
    run_callback: impl FnOnce(
        BorrowedFd<'_>,
        BorrowedFd<'_>,
        crate::workspace_runner::Payload,
    ) -> anyhow::Result<crate::workspace_runner::RunResult>,
) -> anyhow::Result<crate::workspace_runner::RunResult> {
    let owner = map_owner(owner)?;
    let (host, base) = authority.acquire()?;
    let store = crate::workspace_store::WorkspaceStore::open(&host, owner, &base)?;
    let result = run_callback(store.rootfs(), store.as_fd(), payload);
    drop(store);
    result
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

pub(crate) trait RootfsAuthority {
    fn acquire(&self) -> anyhow::Result<(PathBuf, VerifiedBase)>;
}

struct UnacceptedRootfs;

impl RootfsAuthority for UnacceptedRootfs {
    fn acquire(&self) -> anyhow::Result<(PathBuf, VerifiedBase)> {
        Err(anyhow::anyhow!(RUNNER_UNAVAILABLE))
    }
}

#[cfg(test)]
#[path = "workspace_backend_tests.rs"]
mod tests;
