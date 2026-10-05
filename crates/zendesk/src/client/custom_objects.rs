use anyhow::Result;
use serde_json::{Map, Value, json};

use super::*;

const OBJECT_KEYS: [&str; 6] = [
    "key",
    "title",
    "title_pluralized",
    "description",
    "created_at",
    "updated_at",
];

const RECORD_KEYS: [&str; 6] = [
    "id",
    "name",
    "external_id",
    "custom_object_fields",
    "created_at",
    "updated_at",
];

fn field_detail(field: &Value) -> Value {
    let mut out = pick(
        field,
        &[
            "key",
            "title",
            "type",
            "required",
            "description",
            "relationship_target_type",
        ],
        &[],
    );
    out["custom_field_options"] = match field["custom_field_options"].as_array() {
        Some(options) if !options.is_empty() => Value::Array(
            options
                .iter()
                .map(|o| pick(o, &["name", "value"], &[]))
                .collect(),
        ),
        _ => Value::Null,
    };
    out
}

impl ZendeskClient {
    pub async fn list_custom_objects(&self) -> Result<Value> {
        async {
            let data = self.api_get("custom_objects.json", &[]).await?;
            Ok(pick_all(&data, "custom_objects", &OBJECT_KEYS, &[]))
        }
        .await
        .map_err(ctx("Failed to list custom objects"))
    }

    pub async fn get_custom_object(&self, key: &str) -> Result<Value> {
        async {
            let key = segment(key)?;
            let data = self
                .api_get(&format!("custom_objects/{key}.json"), &[])
                .await?;
            let fields = self
                .get_paged(
                    &format!("custom_objects/{key}/fields.json"),
                    "custom_object_fields",
                )
                .await?;
            Ok(json!({
                "object": pick(object(&data, "custom_object")?, &OBJECT_KEYS, &[]),
                "fields": fields.iter().map(field_detail).collect::<Vec<_>>(),
            }))
        }
        .await
        .map_err(ctx(format!("Failed to get custom object {key}")))
    }

    /// One cursor page of records. Without `query` and `filter` this lists the records;
    /// with only `query` it is a text search; with a `filter` it is Zendesk's filtered
    /// search, a POST that only reads.
    pub async fn search_custom_object_records(
        &self,
        key: &str,
        query: Option<&str>,
        filter: Option<Map<String, Value>>,
        sort: Option<&str>,
        page_size: u64,
        after_cursor: Option<&str>,
    ) -> Result<Value> {
        async {
            let base = format!("custom_objects/{}/records", segment(key)?);
            let mut params: Vec<(&str, &(dyn std::fmt::Display + Sync))> = Vec::new();
            if let Some(query) = &query {
                params.push(("query", query));
            }
            if let Some(sort) = &sort {
                params.push(("sort", sort));
            }
            let data = if let Some(filter) = filter {
                let size = page_size.min(100);
                params.push(("page[size]", &size));
                if let Some(after) = &after_cursor {
                    params.push(("page[after]", after));
                }
                self.api_post_bytes(
                    &format!("{base}/search.json"),
                    &params,
                    "application/json",
                    serde_json::to_vec(&json!({ "filter": filter }))?,
                )
                .await?
            } else {
                let path = if query.is_some() {
                    format!("{base}/search.json")
                } else {
                    format!("{base}.json")
                };
                self.get_cursor_page(&path, &params, page_size, after_cursor)
                    .await?
            };
            let mut out = json!({
                "records": pick_all(&data, "custom_object_records", &RECORD_KEYS, &[]),
                "has_more": data["meta"]["has_more"].as_bool().unwrap_or(false),
                "after_cursor": data["meta"]["after_cursor"],
            });
            if let Some(count) = data.get("count").filter(|c| !c.is_null()) {
                out["count"] = count.clone();
            }
            Ok(out)
        }
        .await
        .map_err(ctx(format!(
            "Failed to search records of custom object {key}"
        )))
    }

    pub async fn get_custom_object_record(&self, key: &str, record_id: &str) -> Result<Value> {
        async {
            let path = format!(
                "custom_objects/{}/records/{}.json",
                segment(key)?,
                segment(record_id)?
            );
            let data = self.api_get(&path, &[]).await?;
            Ok(pick(
                object(&data, "custom_object_record")?,
                &RECORD_KEYS,
                &[],
            ))
        }
        .await
        .map_err(ctx(format!(
            "Failed to get record {record_id} of custom object {key}"
        )))
    }
}

#[cfg(test)]
mod tests {
    use crate::client::test_support::*;
    use serde_json::{Value, json};
    use wiremock::matchers::{method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn records_page() -> ResponseTemplate {
        ResponseTemplate::new(200).set_body_json(json!({
            "count": 1,
            "custom_object_records": [{"id": "r1", "name": "N", "external_id": null,
                "custom_object_fields": {"make": "Tesla"}, "url": "u"}],
            "meta": {"has_more": true, "after_cursor": "c2"},
        }))
    }

    #[tokio::test]
    async fn list_custom_objects_are_trimmed() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v2/custom_objects.json"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({"custom_objects": [
                {"key": "car", "title": "Car", "title_pluralized": "Cars", "url": "u"}]})),
            )
            .mount(&server)
            .await;
        let out = client(&server).list_custom_objects().await.unwrap();
        assert_eq!(out[0]["key"], "car");
        assert!(out[0]["description"].is_null());
        assert!(out[0].get("url").is_none());
    }

    #[tokio::test]
    async fn get_custom_object_joins_object_and_fields() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v2/custom_objects/car.json"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({"custom_object": {"key": "car", "title": "Car"}})),
            )
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v2/custom_objects/car/fields.json"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"custom_object_fields": [
                {"key": "color", "title": "Color", "type": "dropdown", "required": false, "id": 1,
                 "custom_field_options": [{"id": 5, "name": "Red", "value": "red"}]},
                {"key": "owner", "type": "lookup", "relationship_target_type": "zen:user",
                 "custom_field_options": []}
            ]})))
            .mount(&server)
            .await;
        let out = client(&server).get_custom_object("car").await.unwrap();
        assert_eq!(out["object"]["title"], "Car");
        assert_eq!(
            out["fields"][0]["custom_field_options"],
            json!([{"name": "Red", "value": "red"}])
        );
        assert!(out["fields"][0]["relationship_target_type"].is_null());
        assert!(out["fields"][0].get("id").is_none());
        assert!(out["fields"][1]["custom_field_options"].is_null());
        assert_eq!(out["fields"][1]["relationship_target_type"], "zen:user");
    }

    #[tokio::test]
    async fn records_without_query_or_filter_use_the_cursor_list() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v2/custom_objects/car/records.json"))
            .and(query_param("page[size]", "100"))
            .and(query_param("page[after]", "c1"))
            .and(query_param("sort", "-updated_at"))
            .respond_with(records_page())
            .mount(&server)
            .await;
        let out = client(&server)
            .search_custom_object_records("car", None, None, Some("-updated_at"), 500, Some("c1"))
            .await
            .unwrap();
        assert_eq!(out["count"], 1);
        assert_eq!(out["has_more"], true);
        assert_eq!(out["after_cursor"], "c2");
        assert_eq!(out["records"][0]["custom_object_fields"]["make"], "Tesla");
        assert!(out["records"][0].get("url").is_none());
    }

    #[tokio::test]
    async fn records_with_query_use_the_search_endpoint() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v2/custom_objects/car/records/search.json"))
            .and(query_param("query", "tes"))
            .respond_with(records_page())
            .mount(&server)
            .await;
        let out = client(&server)
            .search_custom_object_records("car", Some("tes"), None, None, 25, None)
            .await
            .unwrap();
        assert_eq!(out["records"][0]["id"], "r1");
    }

    #[tokio::test]
    async fn records_with_filter_post_the_filter_and_page_in_the_query() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v2/custom_objects/car/records/search.json"))
            .and(query_param("page[size]", "10"))
            .and(query_param("page[after]", "c1"))
            .and(query_param("query", "tes"))
            .respond_with(records_page())
            .mount(&server)
            .await;
        let filter = json!({"custom_object_fields.status": {"$eq": "open"}})
            .as_object()
            .unwrap()
            .clone();
        let out = client(&server)
            .search_custom_object_records("car", Some("tes"), Some(filter), None, 10, Some("c1"))
            .await
            .unwrap();
        assert_eq!(out["has_more"], true);
        let requests = server.received_requests().await.unwrap();
        let body: Value = serde_json::from_slice(&requests[0].body).unwrap();
        assert_eq!(
            body,
            json!({"filter": {"custom_object_fields.status": {"$eq": "open"}}})
        );
        assert_eq!(
            requests[0].headers.get("content-type").unwrap(),
            "application/json"
        );
    }

    #[tokio::test]
    async fn dot_segments_are_rejected() {
        let c = offline_client();
        assert!(c.get_custom_object("..").await.is_err());
        assert!(c.get_custom_object_record("k", "..").await.is_err());
    }

    #[tokio::test]
    async fn get_custom_object_record_is_trimmed_and_encodes_the_key() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v2/custom_objects/car/records/r1.json"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({"custom_object_record": {
                "id": "r1", "name": "N", "photo": {}, "custom_object_fields": {"make": "Tesla"}}})),
            )
            .mount(&server)
            .await;
        let c = client(&server);
        let out = c.get_custom_object_record("car", "r1").await.unwrap();
        assert_eq!(out["name"], "N");
        assert!(out["external_id"].is_null());
        assert!(out.get("photo").is_none());
        // A key with a slash must stay one path segment and so miss the mock.
        assert!(c.get_custom_object_record("car/../x", "r1").await.is_err());
        let requests = server.received_requests().await.unwrap();
        assert!(
            requests[1]
                .url
                .path()
                .starts_with("/api/v2/custom_objects/car%2F")
        );
    }
}
