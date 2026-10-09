//! Custom object tools: object definitions and record search.

use super::*;

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct CustomObjectKeyParams {
    /// The custom object key from list_custom_objects
    key: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct SearchCustomObjectRecordsParams {
    /// The custom object key from list_custom_objects
    key: String,
    /// Text search over text fields only; use filter for other field types
    query: Option<String>,
    /// Zendesk filter, e.g. {"custom_object_fields.status": {"$eq": "open"}} or {"$and": [...]}
    filter: Option<serde_json::Map<String, Value>>,
    /// Without query or filter: id, updated_at, -id, -updated_at. Otherwise: name, created_at, updated_at, or the same with a leading -
    sort: Option<String>,
    /// Number of records per page (max 100)
    #[serde(default = "per_page_25")]
    page_size: u64,
    /// The after_cursor of the previous page
    after_cursor: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct CustomObjectRecordParams {
    /// The custom object key from list_custom_objects
    key: String,
    /// The record ID
    record_id: String,
}

#[tool_router(router = custom_objects_router, vis = "pub(super)")]
impl ZendeskServer {
    #[tool(
        description = "List the custom objects of the account (key, title, title_pluralized, description, created_at, updated_at). Custom objects are account-defined record types (products, orders, assets) linked to tickets through lookup fields. If the account has no custom objects this fails with 403 or 404; treat that as 'not available'.",
        annotations(read_only_hint = true)
    )]
    async fn list_custom_objects(&self) -> CallToolResult {
        self.call_json(|c| async move { c.list_custom_objects().await })
            .await
    }

    #[tool(
        description = "Get a custom object by key together with its fields (key, title, type, required, description, custom_field_options, relationship_target_type). Use it to learn the field keys before searching records.",
        annotations(read_only_hint = true)
    )]
    async fn get_custom_object(
        &self,
        Parameters(p): Parameters<CustomObjectKeyParams>,
    ) -> CallToolResult {
        self.call_json(|c| async move { c.get_custom_object(&p.key).await })
            .await
    }

    #[tool(
        description = "List or search the records of a custom object, one cursor page at a time. With neither query nor filter it lists records; query is a text search that covers text fields only; use filter for other field types. Returns records, count, has_more and after_cursor. Non-admin agents may get 403 on listing or text search for objects with cascading permissions and must use filter.",
        annotations(read_only_hint = true)
    )]
    async fn search_custom_object_records(
        &self,
        Parameters(p): Parameters<SearchCustomObjectRecordsParams>,
    ) -> CallToolResult {
        self.call_json(|c| async move {
            c.search_custom_object_records(
                &p.key,
                p.query.as_deref(),
                p.filter,
                p.sort.as_deref(),
                p.page_size,
                p.after_cursor.as_deref(),
            )
            .await
        })
        .await
    }

    #[tool(
        description = "Get one custom object record by key and record ID (id, name, external_id, custom_object_fields, created_at, updated_at).",
        annotations(read_only_hint = true)
    )]
    async fn get_custom_object_record(
        &self,
        Parameters(p): Parameters<CustomObjectRecordParams>,
    ) -> CallToolResult {
        self.call_json(|c| async move { c.get_custom_object_record(&p.key, &p.record_id).await })
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::{args, assert_tool_ok, sent, server_on};
    use super::*;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn filtered_record_search_posts_the_filter_with_paging_in_the_query() {
        let mock = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v2/custom_objects/orders/records/search.json"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "custom_object_records": [],
                "meta": { "has_more": false },
            })))
            .mount(&mock)
            .await;
        let result = server_on(&mock)
            .search_custom_object_records(args(json!({
                "key": "orders",
                "filter": { "custom_object_fields.status": { "$eq": "open" } },
                "sort": "-updated_at",
                "page_size": 500,
                "after_cursor": "abc",
            })))
            .await;
        assert_tool_ok(&result);
        let requests = mock.received_requests().await.unwrap();
        let query: Vec<(String, String)> = requests[0]
            .url
            .query_pairs()
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .collect();
        for pair in [
            ("sort", "-updated_at"),
            ("page[size]", "100"),
            ("page[after]", "abc"),
        ] {
            assert!(
                query.contains(&(pair.0.to_string(), pair.1.to_string())),
                "{query:?}"
            );
        }
        assert_eq!(
            sent(&mock).await[0].2,
            json!({ "filter": { "custom_object_fields.status": { "$eq": "open" } } })
        );
    }

    #[tokio::test]
    async fn unfiltered_record_search_lists_with_a_get() {
        let mock = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v2/custom_objects/orders/records.json"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "custom_object_records": [],
                "meta": { "has_more": false },
            })))
            .mount(&mock)
            .await;
        let result = server_on(&mock)
            .search_custom_object_records(args(json!({ "key": "orders" })))
            .await;
        assert_tool_ok(&result);
        assert_eq!(sent(&mock).await[0].0, "GET");
    }
}
