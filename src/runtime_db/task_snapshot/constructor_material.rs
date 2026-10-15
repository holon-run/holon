//! Only independently allocated constructor keys, never the moved coverage DTO.
use super::*;
use serde::ser::SerializeSeq;

pub(super) struct BorrowedKeys<'a>(pub(super) &'a TaskCoverage);
impl serde::Serialize for BorrowedKeys<'_> {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        let c = self.0;
        let mut seq = serializer.serialize_seq(None)?;
        macro_rules! key {
            ($family:expr, $id:expr) => {
                seq.serialize_element(&($family, $id))?
            };
        }
        // resident_roots/refresh_views: root set plus pending queue can coexist.
        for k in &c.required_keys { key!(k.family, &k.id); }
        for id in &c.active_task_ids { key!(TaskFamily::Task, id); }
        for id in &c.unsettled_result_ids { key!(TaskFamily::ResultSettlement, id); }
        for k in &c.terminal.keys { key!(k.family, &k.id); }
        for id in c.query.all_selected_task_ids() { key!(TaskFamily::Task, id); }
        // Every related edge may enter both sets; the per-record relation set
        // also exists at the same time. Count all edges, including duplicates.
        for ids in c.task_result_ids.values() {
            for id in ids { key!(TaskFamily::ResultSettlement, id); }
        }
        for relation in c.result_dependencies.values() {
            if let TaskResultDependencyRelation::QualifiedCurrent { task } = &relation.relation {
                key!(task.key.family, &task.key.id);
            }
        }
        // Validators create temporary exact TaskKeys even outside root closure.
        for id in c.stop.heads.keys() { key!(TaskFamily::Task, id); }
        for id in c.output.heads.keys() { key!(TaskFamily::Task, id); }
        for id in c.status.heads.keys() { key!(TaskFamily::Task, id); }
        for headers in c.query.parent_turn_tasks.values() {
            for id in headers.keys() { key!(TaskFamily::Task, id); }
        }
        seq.end()
    }
}
