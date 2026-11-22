use super::*;

#[test]
fn recursive_component_definitions_lift_to_openapi_components() {
    let api = generate_openapi_json();
    let schemas = api["components"]["schemas"]
        .as_object()
        .expect("components schemas object");
    let tree = &schemas["AgentTreeProjection"];
    assert!(
        tree.get("definitions").is_none(),
        "recursive subschemas must not stay nested under their parent component"
    );
    assert!(
        schemas.contains_key("AgentTreeNode"),
        "AgentTreeNode should be lifted to a top-level component"
    );
    let raw = serde_json::to_string(&schemas).expect("serialize schemas");
    assert!(
        !raw.contains("#/definitions/"),
        "draft-style definitions refs must be rewritten to component refs"
    );
}

#[test]
fn generated_openapi_includes_current_http_surface() {
    let api = generate_openapi_json();
    let paths = api["paths"].as_object().expect("paths object");
    let operation_count = paths
        .values()
        .flat_map(|path| path.as_object().into_iter().flat_map(|ops| ops.keys()))
        .count();
    assert!(operation_count >= 40, "expected baseline coverage");
    let error_response = &api["components"]["schemas"]["ErrorResponse"];
    let required_error_fields = error_response["required"]
        .as_array()
        .expect("ErrorResponse required fields");
    for field in ["ok", "error", "code"] {
        assert!(
            required_error_fields
                .iter()
                .any(|required| required == field),
            "ErrorResponse must require {field}"
        );
    }
    assert_eq!(error_response["properties"]["code"]["type"], "string");
    assert!(
        error_response.get("additionalProperties").is_none(),
        "implicit additional properties preserve extension compatibility without generating an invalid Kotlin map subclass"
    );
    assert_eq!(
        error_response["properties"]["correlation"]["$ref"],
        "#/components/schemas/RuntimeErrorContext"
    );
    assert_eq!(
        error_response["properties"]["domain"]["anyOf"][0]["$ref"],
        "#/components/schemas/RuntimeErrorDomain"
    );
    assert!(paths["/api/events/stream"]["get"].is_object());
    assert!(paths["/api/agents/{agent_id}/events/stream"]["get"].is_object());
    let locate = &paths["/api/agents/{agent_id}/conversation/locate"]["post"];
    assert_eq!(locate["operationId"], "agentConversationLocate");
    assert_eq!(
        locate["requestBody"]["content"]["application/json"]["schema"]["$ref"],
        "#/components/schemas/ConversationLocateRequest"
    );
    assert_eq!(
        locate["responses"]["200"]["content"]["application/json"]["schema"]["$ref"],
        "#/components/schemas/ConversationLocateResponse"
    );
    for schema in ["ConversationLocateRequest", "ConversationLocateResponse"] {
        assert!(api["components"]["schemas"][schema].is_object(), "{schema}");
    }
    let history_parameters = paths["/api/agents/{agent_id}/conversation"]["get"]["parameters"]
        .as_array()
        .expect("conversation history parameters");
    for name in ["before", "after"] {
        assert!(
            history_parameters
                .iter()
                .any(|parameter| parameter["name"] == name && parameter["in"] == "query"),
            "missing history parameter {name}"
        );
    }
    let conversation_stream = &paths["/api/agents/{agent_id}/conversation/stream"]["get"];
    assert!(conversation_stream["requestBody"].is_null());
    let stream_parameters = conversation_stream["parameters"]
        .as_array()
        .expect("conversation stream parameters");
    for (name, location) in [
        ("agent_id", "path"),
        ("after", "query"),
        ("limit", "query"),
        ("activity_limit", "query"),
        ("Last-Event-ID", "header"),
    ] {
        assert!(
            stream_parameters
                .iter()
                .any(|parameter| parameter["name"] == name && parameter["in"] == location),
            "missing {location} parameter {name}"
        );
    }
    assert_eq!(
        paths["/api/agents/{agent_id}/events"]["get"]["responses"]["200"]["content"]
            ["application/json"]["schema"]["$ref"],
        "#/components/schemas/EventsPageResponse"
    );
    let envelope = &api["components"]["schemas"]["StreamEventEnvelope"];
    for field in [
        "event_log_epoch",
        "payload_schema",
        "payload_schema_version",
    ] {
        assert!(envelope["properties"][field].is_object(), "{field}");
    }
    for field in ["contract_version", "provenance"] {
        assert!(
            envelope["properties"].get(field).is_none(),
            "StreamEventEnvelope must not redeclare per-event {field}"
        );
    }
    assert!(
        api["components"]["schemas"]["EventsPageResponse"]["properties"]["contract_version"]
            .is_object()
    );
    assert!(paths["/api/callbacks/wake/{callback_token}"]["post"].is_object());
    assert_eq!(
        paths["/api/agents/{agent_id}/status"]["get"]["x-holon-openapi-source"],
        "aide::axum::ApiRouter::api_route_docs"
    );
    assert!(
        paths["/api/control/agents/{agent_id}/tasks"]["post"]["x-holon-openapi-source"].is_null()
    );
    assert_eq!(
        paths["/api/agents/{agent_id}/state"]["get"]["responses"]["200"]["content"]
            ["application/json"]["schema"]["$ref"],
        "#/components/schemas/AgentStateSnapshotDto"
    );
    for (path, method, schema) in [
        ("/api/handshake", "get", "HandshakeResponse"),
        ("/api/agents/list", "get", "AgentListResponse"),
        (
            "/api/control/agents/{agent_id}/prompt",
            "post",
            "EnqueueResponse",
        ),
        ("/api/auth/session/me", "get", "CurrentUserResponse"),
    ] {
        assert_eq!(
            paths[path][method]["responses"]["200"]["content"]["application/json"]["schema"]
                ["$ref"],
            format!("#/components/schemas/{schema}")
        );
    }
    assert_eq!(
        paths["/api/control/agents/{agent_id}/prompt"]["post"]["requestBody"]["content"]
            ["application/json"]["schema"]["$ref"],
        "#/components/schemas/ControlPromptRequest"
    );
    let error = &api["components"]["schemas"]["ErrorResponse"];
    for field in [
        "ok",
        "error",
        "code",
        "hint",
        "domain",
        "retryable",
        "context",
        "correlation",
    ] {
        assert!(error["properties"][field].is_object(), "{field}");
    }
}

#[test]
fn openapi_registers_conversation_routes_and_schemas() {
    let api = generate_openapi_json();
    let schemas = api["components"]["schemas"].as_object().unwrap();
    for name in [
        "ConversationReadQuery",
        "ConversationSummaryResponse",
        "ConversationActivityResponse",
        "ConversationShadowDiagnostics",
    ] {
        assert!(schemas.contains_key(name), "missing schema {name}");
    }
    assert_eq!(
        api["paths"]["/api/agents/{agent_id}/conversation"]["get"]["responses"]["200"]["content"]
            ["application/json"]["schema"]["$ref"],
        "#/components/schemas/ConversationSummaryResponse"
    );
    assert_eq!(
        api["paths"]["/api/agents/{agent_id}/turns/{turn_id}/activities"]["get"]["responses"]
            ["200"]["content"]["application/json"]["schema"]["$ref"],
        "#/components/schemas/ConversationActivityResponse"
    );
    assert_eq!(
        api["paths"]["/api/control/agents/{agent_id}/conversation/shadow-diagnostics"]["get"]
            ["responses"]["200"]["content"]["application/json"]["schema"]["$ref"],
        "#/components/schemas/ConversationShadowDiagnostics"
    );
    let summary_parameters = api["paths"]["/api/agents/{agent_id}/conversation"]["get"]
        ["parameters"]
        .as_array()
        .unwrap();
    assert_eq!(
        api["paths"]["/api/agents/{agent_id}/conversation/locate"]["post"]["responses"]["200"]
            ["content"]["application/json"]["schema"]["$ref"],
        "#/components/schemas/ConversationLocateResponse"
    );
    assert!(
        api["components"]["schemas"]["ConversationLocateRequest"]["properties"]
            .get("source_ref")
            .is_some()
    );
    assert_eq!(
        summary_parameters
            .iter()
            .map(|parameter| parameter["name"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["agent_id", "limit", "before", "after"]
    );
    let activity_parameters = api["paths"]["/api/agents/{agent_id}/turns/{turn_id}/activities"]
        ["get"]["parameters"]
        .as_array()
        .unwrap();
    assert_eq!(
        activity_parameters
            .iter()
            .map(|parameter| parameter["name"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["agent_id", "turn_id", "limit", "before"]
    );
    let shadow_parameters = api["paths"]
        ["/api/control/agents/{agent_id}/conversation/shadow-diagnostics"]["get"]["parameters"]
        .as_array()
        .unwrap();
    assert_eq!(
        shadow_parameters
            .iter()
            .map(|parameter| parameter["name"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["agent_id", "turn_limit"]
    );
    assert_eq!(shadow_parameters[1]["in"], "query");
    assert_eq!(shadow_parameters[1]["required"], false);
    assert_eq!(shadow_parameters[1]["schema"]["minimum"], 1);
    assert_eq!(shadow_parameters[1]["schema"]["maximum"], 100);
    assert_eq!(shadow_parameters[1]["schema"]["default"], 30);
}

#[test]
fn openapi_registers_observer_sync_schemas_without_routes() {
    let api = generate_openapi_json();
    let schemas = api["components"]["schemas"].as_object().unwrap();
    for name in [
        "AgentRosterSnapshot",
        "AgentRosterEntry",
        "AgentEventWindow",
        "AgentLatestBrief",
        "AgentProjectionSnapshot",
        "AgentCanonicalProjection",
        "AgentWorkItemAnchor",
        "ConversationRevisionAnchors",
        "AgentHydrationKey",
        "ProjectionEffect",
        "CursorNotFoundError",
    ] {
        assert!(schemas.contains_key(name), "missing schema {name}");
    }
    let paths = api["paths"].as_object().unwrap();
    // S4 registers the roster snapshot route and S5 the per-Agent
    // projection snapshot route. Route existence still must never be
    // mistaken for capability support: the handlers gate on durable
    // verification.
    assert!(paths.contains_key("/api/agents/snapshot"));
    assert_eq!(
        paths["/api/agents/snapshot"]["get"]["responses"]["200"]["content"]["application/json"]
            ["schema"]["$ref"],
        "#/components/schemas/AgentRosterSnapshot"
    );
    assert!(paths.contains_key("/api/agents/{agent_id}/projection-snapshot"));
    assert_eq!(
        paths["/api/agents/{agent_id}/projection-snapshot"]["get"]["responses"]["200"]["content"]
            ["application/json"]["schema"]["$ref"],
        "#/components/schemas/AgentProjectionSnapshot"
    );
}
