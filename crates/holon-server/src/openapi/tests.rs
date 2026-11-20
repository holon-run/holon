use super::*;

#[test]
fn aide_paths_merge_with_existing_path_items() {
    let mut paths = serde_json::Map::new();
    paths.insert(
        "/shared".into(),
        json!({
            "get": { "operationId": "manualGet" },
            "parameters": [{ "name": "shared" }]
        }),
    );

    let mut incoming = serde_json::Map::new();
    incoming.insert(
        "/shared".into(),
        json!({
            "post": { "operationId": "aidePost" }
        }),
    );
    incoming.insert(
        "/aide-only".into(),
        json!({
            "get": { "operationId": "aideOnly" }
        }),
    );

    merge_path_items(&mut paths, incoming);

    assert_eq!(paths["/shared"]["get"]["operationId"], "manualGet");
    assert_eq!(paths["/shared"]["post"]["operationId"], "aidePost");
    assert_eq!(paths["/shared"]["parameters"][0]["name"], "shared");
    assert_eq!(paths["/aide-only"]["get"]["operationId"], "aideOnly");
}
