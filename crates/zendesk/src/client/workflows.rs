use anyhow::{Result, anyhow, bail};
use serde_json::{Map, Value, json};

use super::*;

const SATISFACTION_SCORES: [&str; 11] = [
    "offered",
    "unoffered",
    "received",
    "received_with_comment",
    "received_without_comment",
    "good",
    "good_with_comment",
    "good_without_comment",
    "bad",
    "bad_with_comment",
    "bad_without_comment",
];

const MACRO_DETAIL_KEYS: [&str; 9] = [
    "id",
    "title",
    "description",
    "active",
    "position",
    "restriction",
    "actions",
    "created_at",
    "updated_at",
];

/// The `{id, value}` custom fields of the macro preview whose value differs from the
/// current ticket's. The preview carries them as `custom_fields` or `fields`; the spec
/// shows `fields` as a single object in its example.
fn changed_custom_fields(current: &Value, after: &Value) -> Vec<Value> {
    let previewed = match (&after["custom_fields"], &after["fields"]) {
        (Value::Array(fields), _) | (_, Value::Array(fields)) => fields.clone(),
        (_, field @ Value::Object(_)) => vec![field.clone()],
        _ => Vec::new(),
    };
    let current_fields = current["custom_fields"].as_array();
    previewed
        .iter()
        .filter(|f| f["id"].is_u64())
        .filter(|f| {
            let existing = current_fields
                .into_iter()
                .flatten()
                .find(|c| c["id"] == f["id"]);
            existing.is_none_or(|c| c["value"] != f["value"])
        })
        .map(|f| pick(f, &["id", "value"], &[]))
        .collect()
}

/// The comment a macro adds, as a ticket-update comment: `body`/`html_body`, or the
/// `channel:all` entry of `scoped_body`. Private unless the macro says public; `scoped_body`
/// itself is never sent.
fn macro_comment(comment: &Value) -> Option<Value> {
    let mut out = Map::new();
    for key in ["body", "html_body"] {
        if let Some(v) = comment.get(key).filter(|v| v.is_string()) {
            out.insert(key.into(), v.clone());
        }
    }
    if out.is_empty() {
        let all = comment["scoped_body"]
            .as_array()?
            .iter()
            .find(|e| e[0] == "channel:all" && e[1].is_string())?;
        out.insert("body".into(), all[1].clone());
    }
    out.insert(
        "public".into(),
        json!(comment["public"].as_bool().unwrap_or(false)),
    );
    Some(Value::Object(out))
}

impl ZendeskClient {
    pub async fn list_views(&self) -> Result<Value> {
        async {
            let views = self.get_paged("views.json", "views").await?;
            Ok(pick_all(
                &json!({ "views": views }),
                "views",
                &["id", "title", "active", "position"],
                &[],
            ))
        }
        .await
        .map_err(ctx("Failed to list views"))
    }

    pub async fn execute_view(
        &self,
        view_id: u64,
        page: u64,
        per_page: u64,
        sort_by: Option<&str>,
        sort_order: Option<&str>,
    ) -> Result<Value> {
        async {
            let per_page = per_page.min(100);
            let mut params: Vec<(&str, &(dyn std::fmt::Display + Sync))> =
                vec![("page", &page), ("per_page", &per_page)];
            if let Some(sort_by) = &sort_by {
                params.push(("sort_by", sort_by));
            }
            if let Some(sort_order) = &sort_order {
                params.push(("sort_order", sort_order));
            }
            let data = self
                .api_get(&format!("views/{view_id}/tickets.json"), &params)
                .await?;
            let tickets = pick_all(&data, "tickets", &TICKET_SUMMARY_KEYS, &[]);
            Ok(json!({
                "count": tickets.as_array().map_or(0, Vec::len),
                "tickets": tickets,
                "has_more": !data["next_page"].is_null(),
            }))
        }
        .await
        .map_err(ctx(format!("Failed to execute view {view_id}")))
    }

    pub async fn list_ticket_fields(&self) -> Result<Value> {
        async {
            let data = self.api_get("ticket_fields.json", &[]).await?;
            let mut fields = pick_all(
                &data,
                "ticket_fields",
                &["id", "title", "type", "active", "required"],
                &[],
            );
            for (field, raw) in fields
                .as_array_mut()
                .into_iter()
                .flatten()
                .zip(data["ticket_fields"].as_array().into_iter().flatten())
            {
                let options = pick_all(raw, "custom_field_options", &["name", "value"], &[]);
                field["custom_field_options"] = match options.as_array() {
                    Some(o) if !o.is_empty() => options,
                    _ => Value::Null,
                };
            }
            Ok(fields)
        }
        .await
        .map_err(ctx("Failed to list ticket fields"))
    }

    pub async fn list_ticket_forms(&self) -> Result<Value> {
        async {
            let data = self.api_get("ticket_forms.json", &[]).await?;
            Ok(pick_all(
                &data,
                "ticket_forms",
                &[
                    "id",
                    "name",
                    "display_name",
                    "active",
                    "default",
                    "ticket_field_ids",
                ],
                &["ticket_field_ids"],
            ))
        }
        .await
        .map_err(ctx("Failed to list ticket forms"))
    }

    pub async fn list_macros(&self, active_only: bool) -> Result<Value> {
        async {
            let path = if active_only {
                "macros/active.json"
            } else {
                "macros.json"
            };
            let macros = self.get_paged(path, "macros").await?;
            Ok(pick_all(
                &json!({ "macros": macros }),
                "macros",
                &["id", "title", "description", "active"],
                &[],
            ))
        }
        .await
        .map_err(ctx("Failed to list macros"))
    }

    pub async fn apply_macro(&self, ticket_id: u64, macro_id: u64) -> Result<Value> {
        async {
            let data = self
                .api_get(
                    &format!("tickets/{ticket_id}/macros/{macro_id}/apply.json"),
                    &[],
                )
                .await?;
            let result = &data["result"];
            Ok(json!({
                "ticket_changes": result.get("ticket").cloned().unwrap_or(json!({})),
                "comment": result["ticket"]["comment"],
            }))
        }
        .await
        .map_err(ctx(format!(
            "Failed to apply macro {macro_id} to ticket {ticket_id}"
        )))
    }

    pub async fn get_view_counts(&self, view_ids: &[u64]) -> Result<Value> {
        async {
            if view_ids.is_empty() || view_ids.len() > 20 {
                bail!(
                    "view_ids must contain 1 to 20 view IDs, got {}",
                    view_ids.len()
                );
            }
            let ids = view_ids
                .iter()
                .map(u64::to_string)
                .collect::<Vec<_>>()
                .join(",");
            let data = self
                .api_get("views/count_many.json", &[("ids", &ids)])
                .await?;
            Ok(pick_all(
                &data,
                "view_counts",
                &["view_id", "value", "pretty", "fresh"],
                &[],
            ))
        }
        .await
        .map_err(ctx("Failed to get view counts"))
    }

    pub async fn get_macro(&self, macro_id: u64) -> Result<Value> {
        async {
            let data = self
                .api_get(&format!("macros/{macro_id}.json"), &[])
                .await?;
            Ok(pick(
                object(&data, "macro")?,
                &MACRO_DETAIL_KEYS,
                &["actions"],
            ))
        }
        .await
        .map_err(ctx(format!("Failed to get macro {macro_id}")))
    }

    /// One page (up to 100) of macros whose title matches `query`; the endpoint is
    /// offset-only.
    pub async fn search_macros(&self, query: &str) -> Result<Value> {
        async {
            let data = self
                .api_get(
                    "macros/search.json",
                    &[("query", &query), ("per_page", &100)],
                )
                .await?;
            Ok(pick_all(
                &data,
                "macros",
                &["id", "title", "description", "active", "actions"],
                &["actions"],
            ))
        }
        .await
        .map_err(ctx("Failed to search macros"))
    }

    /// Applies a macro for real. The preview endpoint returns the whole ticket as it would
    /// be after the macro, so only the fields that differ from the current ticket are
    /// written back, plus the macro's comment, in a `safe_update` that Zendesk rejects
    /// with a 409 if the ticket changed since it was read. `macro_ids` records the macro
    /// in the audit.
    pub async fn execute_macro(&self, ticket_id: u64, macro_id: u64) -> Result<Value> {
        async {
            let path = format!("tickets/{ticket_id}.json");
            let current = self.api_get(&path, &[]).await?;
            let current = object(&current, "ticket")?;
            let updated_at = current["updated_at"]
                .as_str()
                .ok_or_else(|| anyhow!("Zendesk ticket has no updated_at"))?;
            let preview = self
                .api_get(
                    &format!("tickets/{ticket_id}/macros/{macro_id}/apply.json"),
                    &[],
                )
                .await?;
            let after = &preview["result"]["ticket"];

            let mut ticket = Map::new();
            for key in [
                "status",
                "priority",
                "type",
                "subject",
                "assignee_id",
                "group_id",
                "tags",
                "custom_status_id",
                "ticket_form_id",
                "brand_id",
                "due_at",
                "requester_id",
            ] {
                if let Some(v) = after.get(key).filter(|v| !v.is_null())
                    && current.get(key) != Some(v)
                {
                    ticket.insert(key.into(), v.clone());
                }
            }
            let changed_fields = changed_custom_fields(current, after);
            if !changed_fields.is_empty() {
                ticket.insert("custom_fields".into(), Value::Array(changed_fields));
            }
            if let Some(comment) = macro_comment(&after["comment"]) {
                ticket.insert("comment".into(), comment);
            }
            ticket.insert("macro_ids".into(), json!([macro_id]));
            ticket.insert("safe_update".into(), json!(true));
            ticket.insert("updated_stamp".into(), json!(updated_at));
            let data = self.api_put(&path, &json!({ "ticket": ticket })).await?;
            full_ticket(&data)
        }
        .await
        .map_err(ctx(format!(
            "Failed to execute macro {macro_id} on ticket {ticket_id}"
        )))
    }

    /// Triggers, active ones only by default. Zendesk lists `category_id` only on the
    /// unfiltered endpoint, so a category filter goes there with `active=true`.
    pub async fn list_triggers(
        &self,
        active_only: bool,
        category_id: Option<&str>,
    ) -> Result<Value> {
        async {
            let mut params: Vec<(&str, &(dyn Display + Sync))> = Vec::new();
            let path = match (active_only, &category_id) {
                (true, None) => "triggers/active.json",
                (true, Some(_)) => {
                    params.push(("active", &true));
                    "triggers.json"
                }
                (false, _) => "triggers.json",
            };
            if let Some(category_id) = &category_id {
                params.push(("category_id", category_id));
            }
            let triggers = self.get_paged_with(path, &params, "triggers").await?;
            Ok(pick_all(
                &json!({ "triggers": triggers }),
                "triggers",
                &[
                    "id",
                    "title",
                    "active",
                    "category_id",
                    "position",
                    "description",
                    "updated_at",
                ],
                &[],
            ))
        }
        .await
        .map_err(ctx("Failed to list triggers"))
    }

    pub async fn get_trigger(&self, trigger_id: u64) -> Result<Value> {
        async {
            let data = self
                .api_get(&format!("triggers/{trigger_id}.json"), &[])
                .await?;
            Ok(pick(
                object(&data, "trigger")?,
                &[
                    "id",
                    "title",
                    "description",
                    "active",
                    "category_id",
                    "position",
                    "conditions",
                    "actions",
                    "created_at",
                    "updated_at",
                ],
                &["actions"],
            ))
        }
        .await
        .map_err(ctx(format!("Failed to get trigger {trigger_id}")))
    }

    pub async fn get_sla_breaches(&self, days_back: u64, metric: Option<&str>) -> Result<Value> {
        async {
            let days = i64::try_from(days_back.min(MAX_DAYS_BACK))?;
            let start = chrono::Duration::try_days(days)
                .and_then(|d| chrono::Utc::now().checked_sub_signed(d))
                .ok_or_else(|| anyhow!("days_back {days_back} is out of range"))?
                .timestamp();
            let mut url = self.url(
                "incremental/ticket_metric_events.json",
                &[("start_time", &start)],
            )?;
            let mut breaches = Vec::new();
            let mut seen = HashSet::from([url.to_string()]);
            loop {
                let data = self.get_url(url.clone()).await?;
                for event in data["ticket_metric_events"]
                    .as_array()
                    .into_iter()
                    .flatten()
                {
                    if event["type"] == "breach" && metric.is_none_or(|m| event["metric"] == m) {
                        let mut breach = pick(event, &["ticket_id", "metric", "time"], &[]);
                        breach["instance_id"] = event["instance_id"].clone();
                        breaches.push(breach);
                    }
                }
                let Some(link) = data["next_page"].as_str() else {
                    break;
                };
                if data["end_of_stream"] == true {
                    break;
                }
                match self.next_page(&mut seen, link)? {
                    Some(next) => url = next,
                    None => break,
                }
            }

            let mut tickets = std::collections::HashSet::new();
            let mut by_metric = Map::new();
            for breach in &breaches {
                tickets.insert(breach["ticket_id"].to_string());
                let name = breach["metric"].as_str().unwrap_or_default().to_string();
                let count = by_metric.get(&name).and_then(Value::as_u64).unwrap_or(0);
                by_metric.insert(name, json!(count + 1));
            }
            Ok(json!({
                "total_breaches": breaches.len(),
                "unique_tickets": tickets.len(),
                "breaches": breaches,
                "by_metric": by_metric,
                "days_back": days_back,
            }))
        }
        .await
        .map_err(ctx("Failed to get SLA breaches"))
    }

    pub async fn get_sla_policies(&self) -> Result<Value> {
        async {
            let policies = self.get_paged("slas/policies.json", "sla_policies").await?;
            Ok(Value::Array(policies))
        }
        .await
        .map_err(ctx("Failed to get SLA policies"))
    }

    /// Custom ticket statuses; `status` on a ticket is only the category.
    pub async fn list_custom_statuses(&self, active_only: bool) -> Result<Value> {
        async {
            let params: &[(&str, &(dyn Display + Sync))] = if active_only {
                &[("active", &true)]
            } else {
                &[]
            };
            let data = self.api_get("custom_statuses.json", params).await?;
            Ok(pick_all(
                &data,
                "custom_statuses",
                &[
                    "id",
                    "status_category",
                    "agent_label",
                    "end_user_label",
                    "description",
                    "active",
                    "default",
                ],
                &[],
            ))
        }
        .await
        .map_err(ctx("Failed to list custom statuses"))
    }

    /// One page of satisfaction ratings from the last `days_back` days, optionally
    /// filtered by `score`.
    pub async fn list_satisfaction_ratings(
        &self,
        score: Option<&str>,
        days_back: u64,
        page: u64,
        per_page: u64,
    ) -> Result<Value> {
        async {
            if let Some(score) = score
                && !SATISFACTION_SCORES.contains(&score)
            {
                bail!("Invalid score '{score}'. Allowed: {SATISFACTION_SCORES:?}");
            }
            let days = i64::try_from(days_back.min(MAX_DAYS_BACK))?;
            let start = chrono::Duration::try_days(days)
                .and_then(|d| chrono::Utc::now().checked_sub_signed(d))
                .ok_or_else(|| anyhow!("days_back {days_back} is out of range"))?
                .timestamp();
            let per_page = per_page.min(100);
            let mut params: Vec<(&str, &(dyn Display + Sync))> = vec![
                ("start_time", &start),
                ("page", &page),
                ("per_page", &per_page),
            ];
            if let Some(score) = &score {
                params.push(("score", score));
            }
            let data = self.api_get("satisfaction_ratings.json", &params).await?;
            let ratings = pick_all(
                &data,
                "satisfaction_ratings",
                &[
                    "id",
                    "score",
                    "comment",
                    "reason",
                    "reason_id",
                    "ticket_id",
                    "requester_id",
                    "assignee_id",
                    "group_id",
                    "created_at",
                    "updated_at",
                ],
                &[],
            );
            Ok(json!({
                "count": ratings.as_array().map_or(0, Vec::len),
                "ratings": ratings,
                "has_more": !data["next_page"].is_null(),
            }))
        }
        .await
        .map_err(ctx("Failed to list satisfaction ratings"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::test_support::*;
    use wiremock::matchers::{method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn view_counts_join_ids_and_reject_bad_sizes() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v2/views/count_many.json"))
            .and(query_param("ids", "25,78"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({"view_counts": [
                    {"view_id": 25, "value": 719, "pretty": "~700", "fresh": true, "url": "u"},
                    {"view_id": 78, "value": null, "pretty": "...", "fresh": false}
                ]})),
            )
            .expect(1)
            .mount(&server)
            .await;
        let c = client(&server);
        let out = c.get_view_counts(&[25, 78]).await.unwrap();
        assert_eq!(
            out[0],
            json!({"view_id": 25, "value": 719, "pretty": "~700", "fresh": true})
        );
        assert!(out[1]["value"].is_null());
        assert!(c.get_view_counts(&[]).await.is_err());
        assert!(c.get_view_counts(&[1; 21]).await.is_err());
    }

    #[tokio::test]
    async fn get_and_search_macros_include_actions() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v2/macros/25.json"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"macro": {
                "id": 25, "title": "Close", "active": true, "position": 4, "url": "u",
                "actions": [{"field": "status", "value": "solved"}]
            }})))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v2/macros/search.json"))
            .and(query_param("query", "close"))
            .respond_with(json_page(
                "macros",
                json!([{"id": 25, "title": "Close", "extra": 1}]),
                None,
            ))
            .mount(&server)
            .await;
        let c = client(&server);
        let m = c.get_macro(25).await.unwrap();
        assert_eq!(m["actions"][0]["field"], "status");
        assert_eq!(m["position"], 4);
        assert!(m.get("url").is_none());
        let found = c.search_macros("close").await.unwrap();
        assert_eq!(found[0]["id"], 25);
        assert_eq!(found[0]["actions"], json!([]));
        assert!(found[0].get("extra").is_none());
    }

    fn current_ticket() -> Value {
        json!({"ticket": {
            "id": 7, "subject": "Help", "status": "open", "priority": "high", "type": "question",
            "assignee_id": 3, "requester_id": 4, "group_id": 5, "brand_id": 6,
            "ticket_form_id": 8, "custom_status_id": 9, "due_at": null,
            "tags": ["vip", "billing"], "updated_at": "2026-01-02T03:04:05Z",
            "custom_fields": [{"id": 1, "value": "keep"}, {"id": 2, "value": "old"}],
            "url": "https://x/7.json"
        }})
    }

    async fn mount_macro_ticket(server: &MockServer, preview: Value) {
        Mock::given(method("GET"))
            .and(path("/api/v2/tickets/7.json"))
            .respond_with(ResponseTemplate::new(200).set_body_json(current_ticket()))
            .expect(1)
            .mount(server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v2/tickets/7/macros/25/apply.json"))
            .respond_with(ResponseTemplate::new(200).set_body_json(preview))
            .expect(1)
            .mount(server)
            .await;
        Mock::given(method("PUT"))
            .and(path("/api/v2/tickets/7.json"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"ticket": {
                "id": 7, "status": "solved"
            }})))
            .expect(1)
            .mount(server)
            .await;
    }

    async fn put_body(server: &MockServer) -> Value {
        let requests = server.received_requests().await.unwrap();
        let put = requests
            .iter()
            .find(|r| r.method.as_str() == "PUT")
            .unwrap();
        serde_json::from_slice(&put.body).unwrap()
    }

    #[tokio::test]
    async fn execute_macro_sends_only_what_the_preview_changes() {
        let server = MockServer::start().await;
        let mut after = current_ticket()["ticket"].clone();
        after["status"] = json!("solved");
        after["assignee_id"] = json!(11);
        after["custom_fields"] = json!([{"id": 1, "value": "keep"}, {"id": 2, "value": "new"}]);
        after["comment"] = json!({"body": "Done", "html_body": "<p>Done</p>",
            "scoped_body": [["channel:all", "Done"]]});
        mount_macro_ticket(&server, json!({"result": {"ticket": after}})).await;
        let out = client(&server).execute_macro(7, 25).await.unwrap();
        assert_eq!(out["status"], "solved");
        assert_eq!(
            put_body(&server).await,
            json!({"ticket": {
                "status": "solved", "assignee_id": 11,
                "custom_fields": [{"id": 2, "value": "new"}],
                "comment": {"body": "Done", "html_body": "<p>Done</p>", "public": false},
                "macro_ids": [25],
                "safe_update": true, "updated_stamp": "2026-01-02T03:04:05Z"
            }})
        );
    }

    #[tokio::test]
    async fn execute_macro_sends_tags_only_when_they_changed_and_the_all_channel_comment() {
        let server = MockServer::start().await;
        let after = json!({
            "tags": ["vip", "billing", "closed"],
            "fields": {"id": 2, "value": "old"},
            "comment": {"public": true, "scoped_body": [["channel:email", "Mail"], ["channel:all", "Hi"]]},
        });
        mount_macro_ticket(&server, json!({"result": {"ticket": after}})).await;
        client(&server).execute_macro(7, 25).await.unwrap();
        assert_eq!(
            put_body(&server).await,
            json!({"ticket": {
                "tags": ["vip", "billing", "closed"],
                "comment": {"body": "Hi", "public": true},
                "macro_ids": [25],
                "safe_update": true, "updated_stamp": "2026-01-02T03:04:05Z"
            }})
        );
    }

    #[tokio::test]
    async fn execute_macro_without_changes_still_records_the_macro() {
        let server = MockServer::start().await;
        let after = current_ticket()["ticket"].clone();
        mount_macro_ticket(&server, json!({"result": {"ticket": after}})).await;
        client(&server).execute_macro(7, 25).await.unwrap();
        assert_eq!(
            put_body(&server).await,
            json!({"ticket": {
                "macro_ids": [25],
                "safe_update": true, "updated_stamp": "2026-01-02T03:04:05Z"
            }})
        );
    }

    #[tokio::test]
    async fn apply_macro_reads_the_comment_from_the_previewed_ticket() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v2/tickets/7/macros/25/apply.json"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({"result": {"ticket": {
                    "status": "solved", "comment": {"body": "Done", "public": false}
                }}})),
            )
            .mount(&server)
            .await;
        let out = client(&server).apply_macro(7, 25).await.unwrap();
        assert_eq!(out["ticket_changes"]["status"], "solved");
        assert_eq!(out["comment"]["body"], "Done");
    }

    #[tokio::test]
    async fn list_views_collects_every_page() {
        let server = MockServer::start().await;
        let next = format!("{}/api/v2/views.json?page=2", server.uri());
        Mock::given(method("GET"))
            .and(query_param("page", "2"))
            .respond_with(json_page("views", json!([{"id": 2, "title": "B"}]), None))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .respond_with(json_page(
                "views",
                json!([{"id": 1, "title": "A"}]),
                Some(next),
            ))
            .mount(&server)
            .await;
        let out = client(&server).list_views().await.unwrap();
        assert_eq!(out.as_array().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn list_triggers_uses_active_endpoint_or_filters_by_category() {
        let server = MockServer::start().await;
        let trigger = json!([{"id": 1, "title": "t", "active": true, "category_id": "5",
            "position": 1, "conditions": {}, "updated_at": "u"}]);
        Mock::given(method("GET"))
            .and(path("/api/v2/triggers/active.json"))
            .respond_with(json_page("triggers", trigger.clone(), None))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v2/triggers.json"))
            .and(query_param("category_id", "5"))
            .and(query_param("active", "true"))
            .respond_with(json_page("triggers", trigger, None))
            .expect(1)
            .mount(&server)
            .await;
        let c = client(&server);
        let out = c.list_triggers(true, None).await.unwrap();
        assert_eq!(out[0]["category_id"], "5");
        assert!(out[0].get("conditions").is_none());
        c.list_triggers(true, Some("5")).await.unwrap();
    }

    #[tokio::test]
    async fn get_trigger_returns_conditions_and_actions() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v2/triggers/25.json"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"trigger": {
                "id": 25, "title": "Close", "conditions": {"all": [{"field": "status"}], "any": []},
                "actions": [{"field": "status", "value": "solved"}], "url": "u"
            }})))
            .mount(&server)
            .await;
        let out = client(&server).get_trigger(25).await.unwrap();
        assert_eq!(out["conditions"]["all"][0]["field"], "status");
        assert_eq!(out["actions"][0]["value"], "solved");
        assert!(out.get("url").is_none());
    }

    #[tokio::test]
    async fn list_macros_concatenates_pages() {
        let server = MockServer::start().await;
        let next = format!("{}/api/v2/macros/active.json?page=2", server.uri());
        Mock::given(method("GET"))
            .and(path("/api/v2/macros/active.json"))
            .and(query_param("page", "2"))
            .respond_with(json_page("macros", json!([{"id": 2, "title": "b"}]), None))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v2/macros/active.json"))
            .respond_with(json_page(
                "macros",
                json!([{"id": 1, "title": "a", "extra": true}]),
                Some(next),
            ))
            .mount(&server)
            .await;
        let out = client(&server).list_macros(true).await.unwrap();
        assert_eq!(out.as_array().unwrap().len(), 2);
        assert_eq!(out[0]["title"], "a");
        assert!(out[0].get("extra").is_none());
        assert_eq!(out[1]["id"], 2);
    }

    #[tokio::test]
    async fn custom_statuses_are_trimmed_and_filtered_to_active() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v2/custom_statuses.json"))
            .and(query_param("active", "true"))
            .respond_with(json_page(
                "custom_statuses",
                json!([{"id": 1, "status_category": "open", "agent_label": "A", "active": true,
                        "default": false, "raw_agent_label": "x"}]),
                None,
            ))
            .expect(1)
            .mount(&server)
            .await;
        let out = client(&server).list_custom_statuses(true).await.unwrap();
        assert_eq!(
            out[0],
            json!({"id": 1, "status_category": "open", "agent_label": "A", "end_user_label": null,
                   "description": null, "active": true, "default": false})
        );
    }

    #[tokio::test]
    async fn satisfaction_ratings_send_score_and_start_time() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v2/satisfaction_ratings.json"))
            .and(query_param("score", "bad_with_comment"))
            .and(query_param("per_page", "100"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "satisfaction_ratings": [{"id": 1, "score": "bad", "ticket_id": 9, "url": "u"}],
                "next_page": "https://x/next",
            })))
            .mount(&server)
            .await;
        let c = client(&server);
        let out = c
            .list_satisfaction_ratings(Some("bad_with_comment"), 30, 1, 500)
            .await
            .unwrap();
        assert_eq!(out["count"], 1);
        assert_eq!(out["has_more"], true);
        assert_eq!(out["ratings"][0]["ticket_id"], 9);
        assert!(out["ratings"][0].get("url").is_none());
        let requests = server.received_requests().await.unwrap();
        let start: i64 = requests[0]
            .url
            .query_pairs()
            .find(|(k, _)| k == "start_time")
            .unwrap()
            .1
            .parse()
            .unwrap();
        assert!((chrono::Utc::now().timestamp() - 30 * 86_400 - start).abs() < 60);
        let err = c
            .list_satisfaction_ratings(Some("great"), 30, 1, 25)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("Invalid score 'great'"), "{err}");
    }

    #[tokio::test]
    async fn execute_view_passes_sort_params() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v2/views/3/tickets.json"))
            .and(query_param("sort_by", "updated_at"))
            .and(query_param("sort_order", "desc"))
            .respond_with(json_page("tickets", json!([{"id": 1}]), None))
            .expect(1)
            .mount(&server)
            .await;
        let out = client(&server)
            .execute_view(3, 1, 25, Some("updated_at"), Some("desc"))
            .await
            .unwrap();
        assert_eq!(out["count"], 1);
    }

    #[tokio::test]
    async fn sla_breaches_filter_metric_and_sum_pages() {
        let server = MockServer::start().await;
        let next = format!(
            "{}/api/v2/incremental/ticket_metric_events.json?cursor=2",
            server.uri()
        );
        Mock::given(method("GET"))
            .and(path("/api/v2/incremental/ticket_metric_events.json"))
            .and(query_param("cursor", "2"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "ticket_metric_events": [
                    {"type": "breach", "metric": "reply_time", "ticket_id": 1, "time": "t3", "instance_id": 0},
                    {"type": "breach", "metric": "reply_time", "ticket_id": 2, "time": "t4", "instance_id": 0}
                ],
                "end_of_stream": true, "next_page": next
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v2/incremental/ticket_metric_events.json"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "ticket_metric_events": [
                    {"type": "breach", "metric": "reply_time", "ticket_id": 1, "time": "t1", "instance_id": 1},
                    {"type": "breach", "metric": "agent_work_time", "ticket_id": 1, "time": "t2", "instance_id": 1},
                    {"type": "activate", "metric": "reply_time", "ticket_id": 3, "time": "t0", "instance_id": 1}
                ],
                "end_of_stream": false, "next_page": next
            })))
            .mount(&server)
            .await;
        let out = client(&server)
            .get_sla_breaches(7, Some("reply_time"))
            .await
            .unwrap();
        assert_eq!(out["total_breaches"], 3);
        assert_eq!(out["unique_tickets"], 2);
        assert_eq!(out["by_metric"], json!({"reply_time": 3}));
        assert_eq!(out["days_back"], 7);
    }

    #[tokio::test]
    async fn get_sla_policies_merges_pages() {
        let server = MockServer::start().await;
        let next = format!("{}/api/v2/slas/policies.json?page=2", server.uri());
        Mock::given(method("GET"))
            .and(query_param("page", "2"))
            .respond_with(json_page("sla_policies", json!([{"id": 2}]), None))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .respond_with(json_page("sla_policies", json!([{"id": 1}]), Some(next)))
            .mount(&server)
            .await;
        let out = client(&server).get_sla_policies().await.unwrap();
        assert_eq!(out, json!([{"id": 1}, {"id": 2}]));
    }

    #[tokio::test]
    async fn sla_breaches_huge_days_back_does_not_panic() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "ticket_metric_events": [], "end_of_stream": true, "next_page": null
            })))
            .mount(&server)
            .await;
        let out = client(&server)
            .get_sla_breaches(u64::MAX, None)
            .await
            .unwrap();
        assert_eq!(out["total_breaches"], 0);
    }
}
