use anyhow::{Result, anyhow};
use serde_json::{Map, Value, json};

use super::*;

impl ZendeskClient {
    pub async fn list_views(&self) -> Result<Value> {
        async {
            let data = self.api_get("views.json", &[]).await?;
            Ok(pick_all(
                &data,
                "views",
                &["id", "title", "active", "position"],
                &[],
            ))
        }
        .await
        .map_err(ctx("Failed to list views"))
    }

    pub async fn execute_view(&self, view_id: u64, page: u64, per_page: u64) -> Result<Value> {
        async {
            let per_page = per_page.min(100);
            let data = self
                .api_get(
                    &format!("views/{view_id}/tickets.json"),
                    &[("page", &page), ("per_page", &per_page)],
                )
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
            let data = self.api_get(path, &[]).await?;
            Ok(pick_all(
                &data,
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
                "comment": result["comment"],
            }))
        }
        .await
        .map_err(ctx(format!(
            "Failed to apply macro {macro_id} to ticket {ticket_id}"
        )))
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::zendesk::test_support::*;
    use wiremock::matchers::{method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

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
