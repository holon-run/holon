// Borrowed callbacks and thread-local mutation/read witnesses stay on this
// thread. Tokio hands its worker to another thread during synchronous SQL.
pub(super) fn at_blocking_boundary<T>(f: impl FnOnce() -> T) -> T {
    if tokio::runtime::Handle::try_current()
        .is_ok_and(|handle| handle.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread)
    {
        tokio::task::block_in_place(f)
    } else {
        f()
    }
}
