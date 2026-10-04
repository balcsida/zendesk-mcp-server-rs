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
        description = "List the custom objects of the account (key, title, title_pluralized, description, created_at, updated_at). Custom objects are account-defined record types (products, orders, assets) linked to tickets through lookup fields; check active_features.custom_objects_activated in get_account_settings.",
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
        description = "List or search the records of a custom object, one cursor page at a time. With neither query nor filter it lists records; query is a text search that covers text fields only; use filter for other field types. Returns records, count, has_more and after_cursor.",
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
