use super::*;

pub(super) fn assert_input_ready_scheduler_domains_unprepared(
    runtime: &RuntimeHandle,
) -> Result<()> {
    let root = runtime
        .inner
        .mutation_owner
        .capture()?
        .context("original owner publication missing")?;
    assert!(root.input_evidence.is_some());
    assert!(root.work.is_none());
    assert!(root.tasks.is_none());
    assert!(root.scheduling.is_none());
    Ok(())
}

pub async fn publish_unprepared_scheduler_frontier(runtime: &RuntimeHandle) -> Result<()> {
    // Bootstrap prepares every domain. Keep its real Input while explicitly
    // publishing the unprepared scheduler frontier this interleaving exercises.
    let mut reservation = runtime.inner.mutation_owner.reserve().await?;
    let bootstrap = runtime
        .inner
        .mutation_owner
        .capture()?
        .context("bootstrap owner publication missing")?;
    assert!(bootstrap.input_evidence.is_some());
    reservation.begin(crate::ids::audit_event_id())?;
    reservation.committed_prepared_with_scheduling_domains(
        bootstrap.state.clone(),
        bootstrap.episodes.clone(),
        bootstrap.input_evidence.clone(),
        None,
        None,
        None,
    )?;
    reservation.publish(bootstrap.state.clone())?;
    drop(reservation);
    assert_input_ready_scheduler_domains_unprepared(runtime)
}
