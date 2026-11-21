pub use holon_domain::work_item::*;

#[cfg(holon_lib_test_shard = "c")]
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_types_path_reexports_canonical_work_item_types() {
        let state: crate::types::WorkItemState = WorkItemState::Open;
        let todo: crate::types::TodoItem = TodoItem {
            text: "ship".into(),
            state: TodoItemState::Pending,
        };

        assert_eq!(state, WorkItemState::Open);
        assert_eq!(todo.state, TodoItemState::Pending);
    }

    #[test]
    fn legacy_types_path_preserves_work_item_json_schema() {
        let canonical = schemars::schema_for!(WorkItemRecord);
        let legacy = schemars::schema_for!(crate::types::WorkItemRecord);

        assert_eq!(
            serde_json::to_value(canonical).unwrap(),
            serde_json::to_value(legacy).unwrap()
        );
    }
}
