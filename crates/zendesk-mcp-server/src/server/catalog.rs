use zendesk::catalog::{self, Operation};

use super::*;

const DEFAULT_LIMIT: u64 = 20;
const MAX_LIMIT: u64 = 100;

/// Path fragments of the operations that return or set credentials: API and OAuth tokens,
/// OAuth client and webhook signing secrets, Help Center JWTs, passwords, and ZIS
/// connections and inbound webhooks. A read of one could be auto-approved and put the
/// secret in the model's context, so the tools leave them out; the CLI runs them.
const CREDENTIAL_PATHS: [&str; 8] = [
    "/api_tokens",
    "/oauth/",
    "/signing_secret",
    "/help_center/integration/token",
    "/password",
    "/session/renew",
    "/connections",
    "/inbound_webhooks",
];

fn handles_credentials(op: &Operation) -> bool {
    CREDENTIAL_PATHS.iter().any(|p| op.path.contains(p))
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct SearchOperationsParams {
    /// Keywords, e.g. "list ticket comments"; an operation must match every word
    query: String,
    /// Operations to return (defaults to 20, max 100)
    limit: Option<u64>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct GetOperationParams {
    /// The operation ID from search_api_operations, e.g. ShowTicket
    operation_id: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct CallReadParams {
    /// The operation ID from search_api_operations, e.g. ShowTicket
    operation_id: String,
    /// Path and query parameters by name, e.g. {"ticket_id": 1, "include": "users"}. An array is sent comma-separated, or as repeated pairs when the name ends in [] (e.g. "role[]") or the API wants the parameter repeated. An object is sent as name[key]=value, so {"page": {"size": 10}} becomes page[size]=10
    params: Option<Map<String, Value>>,
    /// Keep only these keys of each object in the response, to save tokens. Applies to the elements of top-level arrays and to top-level objects other than meta and links; other values (next_page, count, ...) stay
    fields: Option<Vec<String>>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct CallWriteParams {
    /// The operation ID from search_api_operations, e.g. UpdateTicket
    operation_id: String,
    /// Path and query parameters by name, e.g. {"ticket_id": 1}. An array is sent comma-separated, or as repeated pairs when the name ends in [] (e.g. "ids[]") or the API wants the parameter repeated. An object is sent as name[key]=value, so {"page": {"size": 10}} becomes page[size]=10
    params: Option<Map<String, Value>>,
    /// The JSON request body; get_api_operation shows an example
    body: Option<Value>,
}

/// The operation with this id, or an error pointing at the search.
fn operation(id: &str) -> Result<&'static Operation> {
    let op = catalog::find(id).ok_or_else(|| {
        anyhow!("Unknown operation '{id}'. Use search_api_operations to find operation ids.")
    })?;
    if handles_credentials(op) {
        bail!(
            "{} returns or changes credentials, so it is not available here; run it with the zendesk CLI.",
            op.id
        );
    }
    Ok(op)
}

/// The tool that runs `op`.
fn tool_for(op: &Operation) -> &'static str {
    if op.is_read() {
        "call_api_read"
    } else {
        "call_api_write"
    }
}

/// Reduce an object to the keys in `fields`; anything else is left as it is.
fn keep_fields(value: &mut Value, fields: &[String]) {
    if let Value::Object(object) = value {
        object.retain(|key, _| fields.contains(key));
    }
}

/// Trim a response to `fields`: the objects in a top-level array, and in a top-level
/// object the array elements and object values, except the paging `meta` and `links`.
fn trim_response(response: &mut Value, fields: &[String]) {
    match response {
        Value::Array(items) => items.iter_mut().for_each(|i| keep_fields(i, fields)),
        Value::Object(object) => {
            for (key, value) in object.iter_mut() {
                match value {
                    Value::Array(items) => items.iter_mut().for_each(|i| keep_fields(i, fields)),
                    Value::Object(_) if key != "meta" && key != "links" => {
                        keep_fields(value, fields)
                    }
                    _ => {}
                }
            }
        }
        _ => {}
    }
}

impl ZendeskServer {
    /// Run `op` and return its response as compact JSON: responses can be large.
    async fn call_operation(
        &self,
        op: &'static Operation,
        params: Option<Map<String, Value>>,
        body: Option<Value>,
        fields: Option<Vec<String>>,
    ) -> CallToolResult {
        self.call(|c| async move {
            let params = params.unwrap_or_default();
            let mut response = c.call_operation(op, &params, body.as_ref()).await?;
            if response.is_null() {
                let (path, _) = op.request(&params)?;
                response = json!({ "message": format!("{} {path} succeeded", op.method) });
            }
            if let Some(fields) = fields.filter(|f| !f.is_empty()) {
                trim_response(&mut response, &fields);
            }
            Ok(ContentBlock::text(serde_json::to_string(&response)?))
        })
        .await
    }
}

fn refusal(message: String) -> CallToolResult {
    CallToolResult::error(vec![ContentBlock::text(format!("Error: {message}"))])
}

#[tool_router(router = catalog_router, vis = "pub(super)")]
impl ZendeskServer {
    #[tool(
        description = "Search the full Zendesk API (Support, Help Center, Talk, webhooks, chat, ...) by keywords. Use this when no dedicated tool fits. Returns operation ids, methods, paths and summaries; pass an id to get_api_operation, then to call_api_read or call_api_write.",
        annotations(read_only_hint = true)
    )]
    async fn search_api_operations(
        &self,
        Parameters(p): Parameters<SearchOperationsParams>,
    ) -> CallToolResult {
        let limit = p.limit.unwrap_or(DEFAULT_LIMIT).min(MAX_LIMIT) as usize;
        let hits: Vec<&Operation> = catalog::search(&p.query)
            .into_iter()
            .filter(|op| !handles_credentials(op))
            .collect();
        let operations: Vec<Value> = hits
            .iter()
            .take(limit)
            .map(|op| {
                json!({ "id": op.id, "method": op.method, "path": op.path, "summary": op.summary })
            })
            .collect();
        self.call_json(
            |_| async move { Ok(json!({ "total": hits.len(), "operations": operations })) },
        )
        .await
    }

    #[tool(
        description = "Describe one Zendesk API operation: its path and query parameters, an example request body, and a description. Also says whether to run it with call_api_read or call_api_write.",
        annotations(read_only_hint = true)
    )]
    async fn get_api_operation(
        &self,
        Parameters(p): Parameters<GetOperationParams>,
    ) -> CallToolResult {
        self.call_json(|_| async move {
            let op = operation(&p.operation_id)?;
            let mut described = serde_json::to_value(op)?;
            described["tool"] = tool_for(op).into();
            Ok(described)
        })
        .await
    }

    #[tool(
        description = "Run a read-only (GET) Zendesk API operation found with search_api_operations. Responses can be large: pass `fields` to keep only the keys you need, and page with the operation's paging parameters (e.g. per_page or page[size]).",
        annotations(read_only_hint = true)
    )]
    async fn call_api_read(&self, Parameters(p): Parameters<CallReadParams>) -> CallToolResult {
        let op = match operation(&p.operation_id) {
            Ok(op) if op.is_read() => op,
            Ok(op) => return refusal(format!("{} is not a read; use call_api_write.", op.id)),
            Err(e) => return refusal(format!("{e:#}")),
        };
        self.call_operation(op, p.params, None, p.fields).await
    }

    #[tool(
        description = "Run a Zendesk API operation that writes (POST, PUT, PATCH or DELETE), found with search_api_operations. This may create, change or delete Zendesk data and may notify customers. Call get_api_operation first to see the parameters and an example body.",
        annotations(destructive_hint = true)
    )]
    async fn call_api_write(&self, Parameters(p): Parameters<CallWriteParams>) -> CallToolResult {
        let op = match operation(&p.operation_id) {
            Ok(op) if !op.is_read() => op,
            Ok(op) => return refusal(format!("{} is a read; use call_api_read.", op.id)),
            Err(e) => return refusal(format!("{e:#}")),
        };
        self.call_operation(op, p.params, p.body, None).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{body_json, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn server(mock: &MockServer) -> ZendeskServer {
        let client = ZendeskClient::with_base_url(
            "acme",
            Auth::bearer("t"),
            reqwest::Client::new(),
            format!("{}/api/v2", mock.uri()),
        );
        ZendeskServer::with_login(Login::Shared(Arc::new(client)), reqwest::Client::new())
    }

    /// The text of a tool result, and whether it is an error.
    fn text(result: &CallToolResult) -> (String, bool) {
        let result = serde_json::to_value(result).unwrap();
        (
            result["content"][0]["text"].as_str().unwrap().to_string(),
            result["isError"] == true,
        )
    }

    fn json_of(result: &CallToolResult) -> Value {
        let (text, is_error) = text(result);
        assert!(!is_error, "{text}");
        serde_json::from_str(&text).unwrap()
    }

    fn read(id: &str, params: Value) -> Parameters<CallReadParams> {
        Parameters(serde_json::from_value(json!({ "operation_id": id, "params": params })).unwrap())
    }

    #[tokio::test]
    async fn search_finds_show_ticket_and_honours_limit() {
        let server = server(&MockServer::start().await);
        let found = |query: &str, limit: Option<u64>| {
            let p = SearchOperationsParams {
                query: query.into(),
                limit,
            };
            server.search_api_operations(Parameters(p))
        };
        let result = json_of(&found("show ticket", None).await);
        let ops = result["operations"].as_array().unwrap();
        let show = ops.iter().find(|o| o["id"] == "ShowTicket").unwrap();
        assert_eq!(show["method"], "GET");
        assert_eq!(show["path"], "/api/v2/tickets/{ticket_id}");
        assert!(result["total"].as_u64().unwrap() >= ops.len() as u64);

        let one = json_of(&found("ticket", Some(1)).await);
        assert_eq!(one["operations"].as_array().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn get_operation_names_the_tool_to_call_it_with() {
        let server = server(&MockServer::start().await);
        let get = |id: &str| {
            server.get_api_operation(Parameters(GetOperationParams {
                operation_id: id.into(),
            }))
        };
        let show = json_of(&get("showticket").await);
        assert_eq!(show["id"], "ShowTicket");
        assert_eq!(show["tool"], "call_api_read");
        assert!(
            show["params"]
                .as_array()
                .unwrap()
                .iter()
                .any(|p| p["name"] == "ticket_id")
        );
        assert_eq!(
            json_of(&get("UpdateTicket").await)["tool"],
            "call_api_write"
        );

        let (message, is_error) = text(&get("NoSuchOperation").await);
        assert!(is_error);
        assert!(message.contains("search_api_operations"), "{message}");
    }

    #[tokio::test]
    async fn call_api_read_sends_path_and_query_parameters() {
        let mock = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v2/tickets/1"))
            .and(query_param("include", "users"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({ "ticket": { "id": 1 } })),
            )
            .expect(1)
            .mount(&mock)
            .await;
        let result = server(&mock)
            .call_api_read(read(
                "ShowTicket",
                json!({ "ticket_id": 1, "include": "users" }),
            ))
            .await;
        let (text, is_error) = text(&result);
        assert!(!is_error, "{text}");
        assert_eq!(text, r#"{"ticket":{"id":1}}"#);
    }

    #[tokio::test]
    async fn call_api_read_trims_to_fields() {
        let mock = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v2/tickets"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "tickets": [
                    { "id": 1, "subject": "a", "description": "long" },
                    { "id": 2, "subject": "b", "description": "long" },
                ],
                "organization": { "id": 9, "name": "Acme" },
                "meta": { "has_more": true, "id": "keep" },
                "links": { "next": "x", "id": "keep" },
                "count": 2,
            })))
            .mount(&mock)
            .await;
        let p: CallReadParams = serde_json::from_value(
            json!({ "operation_id": "ListTickets", "fields": ["id", "subject"] }),
        )
        .unwrap();
        let result = json_of(&server(&mock).call_api_read(Parameters(p)).await);
        assert_eq!(
            result,
            json!({
                "tickets": [{ "id": 1, "subject": "a" }, { "id": 2, "subject": "b" }],
                "organization": { "id": 9 },
                "meta": { "has_more": true, "id": "keep" },
                "links": { "next": "x", "id": "keep" },
                "count": 2,
            })
        );

        let mut array = json!([{ "id": 1, "x": 2 }, 5]);
        trim_response(&mut array, &["id".into()]);
        assert_eq!(array, json!([{ "id": 1 }, 5]));

        // No fields keeps everything rather than nothing.
        let p: CallReadParams =
            serde_json::from_value(json!({ "operation_id": "ListTickets", "fields": [] })).unwrap();
        let result = json_of(&server(&mock).call_api_read(Parameters(p)).await);
        assert_eq!(result["tickets"][0]["description"], "long");
    }

    #[tokio::test]
    async fn each_call_tool_refuses_the_other_kind_of_operation() {
        let mock = MockServer::start().await;
        let server = server(&mock);
        let (message, is_error) =
            text(&server.call_api_read(read("CreateTicket", json!({}))).await);
        assert!(is_error);
        assert!(message.contains("call_api_write"), "{message}");

        let write = CallWriteParams {
            operation_id: "ShowTicket".into(),
            params: None,
            body: None,
        };
        let (message, is_error) = text(&server.call_api_write(Parameters(write)).await);
        assert!(is_error);
        assert!(message.contains("call_api_read"), "{message}");

        let (message, is_error) = text(&server.call_api_read(read("Nope", json!({}))).await);
        assert!(is_error);
        assert!(message.contains("search_api_operations"), "{message}");
        // Nothing reached Zendesk.
        assert!(mock.received_requests().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn call_api_write_sends_the_body() {
        let mock = MockServer::start().await;
        let body = json!({ "ticket": { "status": "solved" } });
        Mock::given(method("PUT"))
            .and(path("/api/v2/tickets/7"))
            .and(body_json(&body))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({ "ticket": { "id": 7 } })),
            )
            .expect(1)
            .mount(&mock)
            .await;
        let write = CallWriteParams {
            operation_id: "UpdateTicket".into(),
            params: Some(json!({ "ticket_id": 7 }).as_object().unwrap().clone()),
            body: Some(body),
        };
        let result = server(&mock).call_api_write(Parameters(write)).await;
        assert_eq!(json_of(&result), json!({ "ticket": { "id": 7 } }));
    }

    #[tokio::test]
    async fn call_api_write_reports_success_for_an_empty_body() {
        let mock = MockServer::start().await;
        Mock::given(method("DELETE"))
            .and(path("/api/v2/tickets/7"))
            .respond_with(ResponseTemplate::new(204))
            .expect(1)
            .mount(&mock)
            .await;
        let write = CallWriteParams {
            operation_id: "DeleteTicket".into(),
            params: Some(json!({ "ticket_id": 7 }).as_object().unwrap().clone()),
            body: None,
        };
        let result = server(&mock).call_api_write(Parameters(write)).await;
        assert_eq!(
            json_of(&result),
            json!({ "message": "DELETE /api/v2/tickets/7 succeeded" })
        );
    }

    #[tokio::test]
    async fn credential_operations_are_hidden_and_refused() {
        let mock = MockServer::start().await;
        let server = server(&mock);
        let p = SearchOperationsParams {
            query: "webhook signing secret".into(),
            limit: Some(100),
        };
        let found = json_of(&server.search_api_operations(Parameters(p)).await);
        let ops = found["operations"].as_array().unwrap();
        assert!(!ops.is_empty());
        assert!(
            ops.iter()
                .all(|op| !op["path"].as_str().unwrap().contains("/signing_secret")),
            "{found}"
        );

        let read = read("ShowWebhookSigningSecret", json!({ "webhook_id": "w" }));
        let (message, is_error) = text(&server.call_api_read(read).await);
        assert!(is_error);
        assert!(message.contains("zendesk CLI"), "{message}");
        let get = GetOperationParams {
            operation_id: "CreateApiToken".into(),
        };
        assert!(text(&server.get_api_operation(Parameters(get)).await).1);
        assert!(mock.received_requests().await.unwrap().is_empty());

        // Every guard still names something, so a renamed path cannot slip through.
        for marker in CREDENTIAL_PATHS {
            assert!(
                catalog::operations()
                    .iter()
                    .any(|op| op.path.contains(marker)),
                "{marker}"
            );
        }
    }
}
