use anyhow::{Result, bail};
use serde_json::Value;

use super::*;

impl ZendeskClient {
    pub async fn get_user(&self, user_id: u64) -> Result<Value> {
        async {
            let data = self.api_get(&format!("users/{user_id}.json"), &[]).await?;
            let user = object(&data, "user")?;
            let mut out = pick(
                user,
                &[
                    "id",
                    "name",
                    "email",
                    "role",
                    "phone",
                    "organization_id",
                    "time_zone",
                    "active",
                    "suspended",
                    "created_at",
                    "updated_at",
                    "tags",
                    "user_fields",
                    "notes",
                    "details",
                    "external_id",
                    "locale",
                    "last_login_at",
                    "ticket_restriction",
                    "verified",
                    "default_group_id",
                    "alias",
                ],
                &["tags"],
            );
            out["photo_url"] = user["photo"]["content_url"].clone();
            Ok(out)
        }
        .await
        .map_err(ctx(format!("Failed to get user {user_id}")))
    }

    pub async fn get_current_user(&self) -> Result<Value> {
        async {
            let data = self.api_get("users/me.json", &[]).await?;
            Ok(pick(
                object(&data, "user")?,
                &[
                    "id",
                    "name",
                    "email",
                    "role",
                    "organization_id",
                    "time_zone",
                    "default_group_id",
                    "custom_role_id",
                    "ticket_restriction",
                    "restricted_agent",
                    "shared_agent",
                    "locale",
                    "active",
                    "verified",
                ],
                &[],
            ))
        }
        .await
        .map_err(ctx("Failed to get current user"))
    }

    /// At least one of `query` and `external_id` is required. Zendesk returns at most
    /// 10,000 matches.
    pub async fn search_users(
        &self,
        query: Option<&str>,
        external_id: Option<&str>,
    ) -> Result<Value> {
        async {
            let mut params: Vec<(&str, &(dyn std::fmt::Display + Sync))> = Vec::new();
            if let Some(query) = &query {
                params.push(("query", query));
            }
            if let Some(external_id) = &external_id {
                params.push(("external_id", external_id));
            }
            if params.is_empty() {
                bail!("Give a query or an external_id");
            }
            let data = self.api_get("users/search.json", &params).await?;
            Ok(pick_all(
                &data,
                "users",
                &[
                    "id",
                    "name",
                    "email",
                    "role",
                    "organization_id",
                    "active",
                    "external_id",
                    "suspended",
                ],
                &[],
            ))
        }
        .await
        .map_err(ctx("User search failed"))
    }

    pub async fn get_organization(&self, organization_id: u64) -> Result<Value> {
        async {
            let data = self
                .api_get(&format!("organizations/{organization_id}.json"), &[])
                .await?;
            Ok(pick(
                object(&data, "organization")?,
                &[
                    "id",
                    "name",
                    "domain_names",
                    "details",
                    "notes",
                    "group_id",
                    "tags",
                    "created_at",
                    "updated_at",
                    "organization_fields",
                    "external_id",
                    "shared_tickets",
                    "shared_comments",
                ],
                &["domain_names", "tags"],
            ))
        }
        .await
        .map_err(ctx(format!("Failed to get organization {organization_id}")))
    }

    pub async fn search_organizations(&self, query: &str) -> Result<Value> {
        async {
            let data = self
                .api_get("organizations/autocomplete.json", &[("name", &query)])
                .await?;
            Ok(pick_all(
                &data,
                "organizations",
                &["id", "name", "domain_names"],
                &["domain_names"],
            ))
        }
        .await
        .map_err(ctx("Organization search failed"))
    }

    pub async fn list_groups(&self) -> Result<Value> {
        async {
            let data = self.api_get("groups/assignable.json", &[]).await?;
            Ok(pick_all(
                &data,
                "groups",
                &["id", "name", "description"],
                &[],
            ))
        }
        .await
        .map_err(ctx("Failed to list groups"))
    }
}

#[cfg(test)]
mod tests {
    use crate::zendesk::test_support::*;
    use serde_json::json;
    use wiremock::matchers::{method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn get_user_adds_profile_fields_and_never_authenticity_token() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v2/users/5.json"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"user": {
                "id": 5, "notes": "vip", "user_fields": {"plan": "gold"},
                "ticket_restriction": "assigned", "authenticity_token": "secret"
            }})))
            .mount(&server)
            .await;
        let out = client(&server).get_user(5).await.unwrap();
        assert_eq!(out["notes"], "vip");
        assert_eq!(out["user_fields"], json!({"plan": "gold"}));
        assert_eq!(out["ticket_restriction"], "assigned");
        assert!(out["external_id"].is_null());
        assert!(out.get("authenticity_token").is_none());
    }

    #[tokio::test]
    async fn search_users_by_external_id_and_requires_a_criterion() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v2/users/search.json"))
            .and(query_param("external_id", "crm-7"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"users": [
                {"id": 1, "external_id": "crm-7", "suspended": false, "extra": 1}
            ]})))
            .mount(&server)
            .await;
        let c = client(&server);
        let out = c.search_users(None, Some("crm-7")).await.unwrap();
        assert_eq!(out[0]["external_id"], "crm-7");
        assert_eq!(out[0]["suspended"], false);
        assert!(out[0].get("extra").is_none());
        let err = c.search_users(None, None).await.unwrap_err();
        assert!(err.to_string().contains("query or an external_id"), "{err}");
    }
}
