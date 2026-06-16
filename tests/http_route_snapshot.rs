//! HTTP route and OpenAPI coverage drift test.
//!
//! Refresh workflow for intentional HTTP surface changes:
//!
//! ```bash
//! cargo test --test http_route_snapshot refresh_http_route_inventory_snapshot -- --ignored
//! cargo test --test http_route_snapshot
//! ```

use serde::Serialize;
use serde_json::Value;

const HTTP_SOURCE_PATH: &str = "src/http.rs";
const SNAPSHOT_PATH: &str = "tests/snapshots/http_route_inventory.json";

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
struct HttpRoute {
    method: String,
    path: String,
    handler: String,
}

#[derive(Debug, Serialize)]
struct RouteInventoryEntry {
    method: String,
    path: String,
    handler: String,
    operation_id: String,
    tag: String,
    parameters: Vec<RouteParameter>,
    request_schema: Option<String>,
    request_strict: Option<bool>,
    response_content_types: Vec<String>,
    security: Vec<String>,
}

#[derive(Debug, Serialize)]
struct RouteParameter {
    name: String,
    location: String,
    required: bool,
}

#[test]
fn http_route_inventory_snapshot_matches_router_and_openapi() {
    let live = render_live_inventory();
    let stored = std::fs::read_to_string(SNAPSHOT_PATH)
        .unwrap_or_else(|err| panic!("failed to read {SNAPSHOT_PATH}: {err}"));

    if live.replace("\r\n", "\n") != stored.replace("\r\n", "\n") {
        eprintln!(
            "HTTP route inventory drift detected. Refresh intentionally with:\n  cargo test --test http_route_snapshot refresh_http_route_inventory_snapshot -- --ignored\n"
        );
        eprintln!("=== GENERATED HTTP ROUTE INVENTORY ===");
        eprintln!("{live}");
        panic!("HTTP route inventory snapshot does not match router/OpenAPI surface");
    }
}

#[test]
#[ignore]
fn refresh_http_route_inventory_snapshot() {
    let live = render_live_inventory();
    std::fs::write(SNAPSHOT_PATH, live).expect("write HTTP route inventory snapshot");
}

fn render_live_inventory() -> String {
    let source = std::fs::read_to_string(HTTP_SOURCE_PATH)
        .unwrap_or_else(|err| panic!("failed to read {HTTP_SOURCE_PATH}: {err}"));
    let routes = parse_axum_routes(&source);
    assert_eq!(routes.len(), 67, "unexpected parsed HTTP route count");

    let openapi = holon::openapi::generate_openapi_json();
    let mut entries = Vec::new();
    for route in routes {
        let operation = &openapi["paths"][&route.path][&route.method];
        assert!(
            operation.is_object(),
            "route {} {} handled by {} is missing from generated OpenAPI",
            route.method,
            route.path,
            route.handler
        );
        entries.push(route_inventory_entry(route, operation, &openapi));
    }

    serde_json::to_string_pretty(&entries).expect("serialize HTTP route inventory")
}

fn parse_axum_routes(source: &str) -> Vec<HttpRoute> {
    let mut routes = Vec::new();
    let mut offset = 0;
    while let Some(relative_start) = source[offset..].find(".route(") {
        let call_start = offset + relative_start + ".route(".len();
        let call_end = balanced_call_end(source, call_start)
            .unwrap_or_else(|| panic!("unterminated .route(...) call near byte {call_start}"));
        routes.push(parse_route_call(&source[call_start..call_end]));
        offset = call_end;
    }
    routes.sort();
    routes
}

fn balanced_call_end(source: &str, start: usize) -> Option<usize> {
    let mut depth = 1usize;
    let mut in_string = false;
    let mut escaped = false;

    for (relative_index, ch) in source[start..].char_indices() {
        if in_string {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_string = false;
            }
            continue;
        }

        match ch {
            '"' => in_string = true,
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    return Some(start + relative_index);
                }
            }
            _ => {}
        }
    }
    None
}

fn parse_route_call(call: &str) -> HttpRoute {
    let (path, after_path) = parse_first_string(call);
    let call_tail = after_path.trim_start_matches(|ch: char| ch == ',' || ch.is_whitespace());
    for method in ["get", "post", "patch"] {
        let prefix = format!("{method}(");
        if let Some(handler_start) = call_tail.find(&prefix) {
            let handler_start = handler_start + prefix.len();
            let handler_end = call_tail[handler_start..]
                .find(')')
                .map(|relative| handler_start + relative)
                .unwrap_or_else(|| panic!("missing handler terminator for route {path}"));
            let handler = call_tail[handler_start..handler_end].trim().to_string();
            return HttpRoute {
                method: method.to_string(),
                path,
                handler,
            };
        }
    }
    panic!("unsupported route call: {call}");
}

fn parse_first_string(call: &str) -> (String, &str) {
    let start = call
        .find('"')
        .unwrap_or_else(|| panic!("route call is missing path string: {call}"));
    let mut escaped = false;
    for (relative_index, ch) in call[start + 1..].char_indices() {
        if escaped {
            escaped = false;
        } else if ch == '\\' {
            escaped = true;
        } else if ch == '"' {
            let end = start + 1 + relative_index;
            return (call[start + 1..end].to_string(), &call[end + 1..]);
        }
    }
    panic!("unterminated route path string: {call}");
}

fn route_inventory_entry(
    route: HttpRoute,
    operation: &Value,
    openapi: &Value,
) -> RouteInventoryEntry {
    let request_schema = operation
        .pointer("/requestBody/content/application~1json/schema/$ref")
        .and_then(Value::as_str)
        .map(schema_name_from_ref);
    let request_strict = request_schema
        .as_deref()
        .and_then(|name| openapi["components"]["schemas"][name]["additionalProperties"].as_bool())
        .map(|additional_properties| !additional_properties);

    RouteInventoryEntry {
        method: route.method,
        path: route.path,
        handler: route.handler,
        operation_id: string_field(operation, "operationId"),
        tag: operation["tags"]
            .as_array()
            .and_then(|tags| tags.first())
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        parameters: parameters(operation),
        request_schema,
        request_strict,
        response_content_types: response_content_types(operation),
        security: security(operation),
    }
}

fn schema_name_from_ref(reference: &str) -> String {
    reference
        .strip_prefix("#/components/schemas/")
        .unwrap_or(reference)
        .to_string()
}

fn string_field(value: &Value, key: &str) -> String {
    value[key]
        .as_str()
        .unwrap_or_else(|| panic!("OpenAPI operation is missing string field {key}: {value}"))
        .to_string()
}

fn response_content_types(operation: &Value) -> Vec<String> {
    let mut content_types = operation["responses"]
        .as_object()
        .into_iter()
        .flat_map(|responses| responses.values())
        .filter_map(|response| response["content"].as_object())
        .flat_map(|content| content.keys().cloned())
        .collect::<Vec<_>>();
    content_types.sort();
    content_types.dedup();
    content_types
}

fn parameters(operation: &Value) -> Vec<RouteParameter> {
    let mut parameters = operation["parameters"]
        .as_array()
        .into_iter()
        .flat_map(|parameters| parameters.iter())
        .map(|parameter| RouteParameter {
            name: string_field(parameter, "name"),
            location: string_field(parameter, "in"),
            required: parameter["required"].as_bool().unwrap_or(false),
        })
        .collect::<Vec<_>>();
    parameters.sort_by(|left, right| {
        left.location
            .cmp(&right.location)
            .then_with(|| left.name.cmp(&right.name))
    });
    parameters
}

fn security(operation: &Value) -> Vec<String> {
    let mut schemes = operation["security"]
        .as_array()
        .into_iter()
        .flat_map(|entries| entries.iter())
        .filter_map(Value::as_object)
        .flat_map(|entry| entry.keys().cloned())
        .collect::<Vec<_>>();
    schemes.sort();
    schemes.dedup();
    schemes
}
