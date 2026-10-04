use anyhow::{Result, bail};
use serde_json::{Map, Value, json};

use super::*;

/// The shape `get_user` and the user write tools return.
fn user_detail(user: &Value) -> Value {
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
    out
}

/// The shape `get_organization` and `update_organization` return.
fn organization_detail(organization: &Value) -> Value {
    pick(
        organization,
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
    )
}

impl ZendeskClient {
    pub async fn get_user(&self, user_id: u64) -> Result<Value> {
        async {
            let data = self.api_get(&format!("users/{user_id}.json"), &[]).await?;
            Ok(user_detail(object(&data, "user")?))
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
            Ok(organization_detail(object(&data, "organization")?))
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

    /// Fetches in chunks of 100 ids, the `show_many` limit.
    pub async fn get_users_bulk(&self, user_ids: &[u64]) -> Result<Value> {
        async {
            let mut users = Vec::new();
            for chunk in user_ids.chunks(100) {
                let ids = chunk
                    .iter()
                    .map(u64::to_string)
                    .collect::<Vec<_>>()
                    .join(",");
                let data = self
                    .api_get("users/show_many.json", &[("ids", &ids)])
                    .await?;
                if let Value::Array(page) = pick_all(
                    &data,
                    "users",
                    &[
                        "id",
                        "name",
                        "email",
                        "role",
                        "organization_id",
                        "external_id",
                        "active",
                        "suspended",
                        "time_zone",
                        "locale",
                    ],
                    &[],
                ) {
                    users.extend(page);
                }
            }
            Ok(Value::Array(users))
        }
        .await
        .map_err(ctx("Bulk user fetch failed"))
    }

    pub async fn get_user_identities(&self, user_id: u64) -> Result<Value> {
        async {
            let items = self
                .get_paged(&format!("users/{user_id}/identities.json"), "identities")
                .await?;
            let data = json!({ "identities": items });
            let identities = pick_all(
                &data,
                "identities",
                &[
                    "id",
                    "type",
                    "value",
                    "primary",
                    "verified",
                    "verification_method",
                    "deliverable_state",
                    "undeliverable_count",
                ],
                &[],
            );
            Ok(json!({ "count": items.len(), "identities": identities }))
        }
        .await
        .map_err(ctx(format!("Failed to get identities of user {user_id}")))
    }

    pub async fn get_user_organizations(&self, user_id: u64) -> Result<Value> {
        async {
            let memberships = self
                .get_paged(
                    &format!("users/{user_id}/organization_memberships.json"),
                    "organization_memberships",
                )
                .await?;
            let organizations: Vec<Value> = memberships
                .iter()
                .map(|m| {
                    json!({
                        "id": m["organization_id"],
                        "name": m["organization_name"],
                        // Zendesk sends null instead of false.
                        "default": m["default"].as_bool().unwrap_or(false),
                        "view_tickets": m["view_tickets"],
                    })
                })
                .collect();
            Ok(json!({ "organizations": organizations }))
        }
        .await
        .map_err(ctx(format!(
            "Failed to get organizations of user {user_id}"
        )))
    }

    /// `fields` is the `user` body; it must hold `name` and `email`.
    pub async fn create_or_update_user(&self, fields: Map<String, Value>) -> Result<Value> {
        async {
            let data = self
                .api_post("users/create_or_update.json", &json!({ "user": fields }))
                .await?;
            Ok(user_detail(object(&data, "user")?))
        }
        .await
        .map_err(ctx("Failed to create or update user"))
    }

    pub async fn update_user(&self, user_id: u64, fields: Map<String, Value>) -> Result<Value> {
        async {
            if fields.is_empty() {
                bail!("Give at least one field to update");
            }
            let data = self
                .api_put(&format!("users/{user_id}.json"), &json!({ "user": fields }))
                .await?;
            Ok(user_detail(object(&data, "user")?))
        }
        .await
        .map_err(ctx(format!("Failed to update user {user_id}")))
    }

    pub async fn list_organization_users(
        &self,
        organization_id: u64,
        page: u64,
        per_page: u64,
    ) -> Result<Value> {
        async {
            let per_page = per_page.min(100);
            let data = self
                .api_get(
                    &format!("organizations/{organization_id}/users.json"),
                    &[("page", &page), ("per_page", &per_page)],
                )
                .await?;
            let users = pick_all(
                &data,
                "users",
                &[
                    "id",
                    "name",
                    "email",
                    "role",
                    "active",
                    "suspended",
                    "external_id",
                    "phone",
                ],
                &[],
            );
            Ok(json!({
                "count": users.as_array().map_or(0, Vec::len),
                "users": users,
                "page": page,
                "per_page": per_page,
                "has_more": !data["next_page"].is_null(),
            }))
        }
        .await
        .map_err(ctx(format!(
            "Failed to list users of organization {organization_id}"
        )))
    }

    pub async fn update_organization(
        &self,
        organization_id: u64,
        fields: Map<String, Value>,
    ) -> Result<Value> {
        async {
            if fields.is_empty() {
                bail!("Give at least one field to update");
            }
            let data = self
                .api_put(
                    &format!("organizations/{organization_id}.json"),
                    &json!({ "organization": fields }),
                )
                .await?;
            Ok(organization_detail(object(&data, "organization")?))
        }
        .await
        .map_err(ctx(format!(
            "Failed to update organization {organization_id}"
        )))
    }

    pub async fn get_group_members(&self, group_id: u64) -> Result<Value> {
        async {
            let items = self
                .get_paged(&format!("groups/{group_id}/users.json"), "users")
                .await?;
            let users = pick_all(
                &json!({ "users": items }),
                "users",
                &["id", "name", "email", "role", "active", "suspended"],
                &[],
            );
            Ok(json!({ "count": items.len(), "users": users }))
        }
        .await
        .map_err(ctx(format!("Failed to get members of group {group_id}")))
    }

    /// Brands use cursor pagination only; at most 1000 are returned.
    pub async fn list_brands(&self) -> Result<Value> {
        async {
            let brands = self
                .get_cursor_paged("brands.json", &[], "brands", 1000)
                .await?;
            Ok(pick_all(
                &json!({ "brands": brands }),
                "brands",
                &[
                    "id",
                    "name",
                    "subdomain",
                    "brand_url",
                    "default",
                    "active",
                    "has_help_center",
                    "help_center_state",
                    "ticket_form_ids",
                ],
                &["ticket_form_ids"],
            ))
        }
        .await
        .map_err(ctx("Failed to list brands"))
    }

    pub async fn get_account_settings(&self) -> Result<Value> {
        async {
            let data = self.api_get("account/settings.json", &[]).await?;
            let settings = object(&data, "settings")?;
            let mut out = pick(
                settings,
                &[
                    "active_features",
                    "brands",
                    "tickets",
                    "agents",
                    "localization",
                    "limits",
                    "routing",
                ],
                &[],
            );
            // The spec names this sub-object `user`; accept `users` too.
            out["users"] = [&settings["users"], &settings["user"]]
                .into_iter()
                .find(|v| !v.is_null())
                .cloned()
                .unwrap_or(Value::Null);
            Ok(out)
        }
        .await
        .map_err(ctx("Failed to get account settings"))
    }
}

#[cfg(test)]
mod tests {
    use crate::zendesk::test_support::*;
    use serde_json::{Map, Value, json};
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

    #[tokio::test]
    async fn users_bulk_chunks_ids_by_100_and_trims() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v2/users/show_many.json"))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                json!({"users": [{"id": 1, "name": "A", "locale": "en-US", "x": 1}]}),
            ))
            .mount(&server)
            .await;
        let ids: Vec<u64> = (1..=150).collect();
        let out = client(&server).get_users_bulk(&ids).await.unwrap();
        assert_eq!(out.as_array().unwrap().len(), 2);
        assert_eq!(out[0]["locale"], "en-US");
        assert!(out[0]["email"].is_null());
        assert!(out[0].get("x").is_none());
        let requests = server.received_requests().await.unwrap();
        assert_eq!(requests.len(), 2);
        assert!(requests[1].url.query().unwrap().contains("ids=101%2C102"));
    }

    #[tokio::test]
    async fn user_identities_are_trimmed_with_null_for_absent_keys() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v2/users/9/identities.json"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({"identities": [
                    {"id": 1, "type": "email", "value": "a@b.c", "primary": true,
                     "verified": true, "deliverable_state": "deliverable", "user_id": 9}
                ]})),
            )
            .mount(&server)
            .await;
        let out = client(&server).get_user_identities(9).await.unwrap();
        assert_eq!(out["count"], 1);
        assert_eq!(out["identities"][0]["deliverable_state"], "deliverable");
        assert!(out["identities"][0]["undeliverable_count"].is_null());
        assert!(out["identities"][0].get("user_id").is_none());
    }

    #[tokio::test]
    async fn user_organizations_use_membership_names_and_default_false() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v2/users/9/organization_memberships.json"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "organization_memberships": [
                    {"organization_id": 12, "organization_name": "First", "default": true, "view_tickets": true},
                    {"organization_id": 3, "organization_name": "Second", "default": null, "view_tickets": false}
                ]
            })))
            .mount(&server)
            .await;
        let out = client(&server).get_user_organizations(9).await.unwrap();
        assert_eq!(
            out["organizations"][0],
            json!({"id": 12, "name": "First", "default": true, "view_tickets": true})
        );
        assert_eq!(out["organizations"][1]["default"], false);
    }

    #[tokio::test]
    async fn create_or_update_user_posts_the_user_body() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v2/users/create_or_update.json"))
            .respond_with(
                ResponseTemplate::new(201).set_body_json(
                    json!({"user": {"id": 4, "email": "a@b.c", "role": "end-user"}}),
                ),
            )
            .mount(&server)
            .await;
        let fields = json!({"name": "A", "email": "a@b.c"})
            .as_object()
            .unwrap()
            .clone();
        let out = client(&server).create_or_update_user(fields).await.unwrap();
        assert_eq!(out["id"], 4);
        let requests = server.received_requests().await.unwrap();
        let body: Value = serde_json::from_slice(&requests[0].body).unwrap();
        assert_eq!(body, json!({"user": {"name": "A", "email": "a@b.c"}}));
    }

    #[tokio::test]
    async fn update_user_puts_the_body_and_rejects_empty_updates() {
        let server = MockServer::start().await;
        Mock::given(method("PUT"))
            .and(path("/api/v2/users/4.json"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({"user": {"id": 4, "notes": "n"}})),
            )
            .mount(&server)
            .await;
        let c = client(&server);
        let fields = json!({"notes": "n"}).as_object().unwrap().clone();
        assert_eq!(c.update_user(4, fields).await.unwrap()["notes"], "n");
        let requests = server.received_requests().await.unwrap();
        let body: Value = serde_json::from_slice(&requests[0].body).unwrap();
        assert_eq!(body, json!({"user": {"notes": "n"}}));
        assert!(c.update_user(4, Map::new()).await.is_err());
        assert_eq!(server.received_requests().await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn organization_users_page_and_has_more() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v2/organizations/7/users.json"))
            .and(query_param("page", "2"))
            .and(query_param("per_page", "100"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "users": [{"id": 1, "name": "A", "phone": "1", "x": 1}],
                "next_page": "https://x/next",
            })))
            .mount(&server)
            .await;
        let out = client(&server)
            .list_organization_users(7, 2, 500)
            .await
            .unwrap();
        assert_eq!(out["count"], 1);
        assert_eq!(out["per_page"], 100);
        assert_eq!(out["has_more"], true);
        assert_eq!(out["users"][0]["phone"], "1");
        assert!(out["users"][0].get("x").is_none());
    }

    #[tokio::test]
    async fn update_organization_puts_the_body_and_rejects_empty_updates() {
        let server = MockServer::start().await;
        Mock::given(method("PUT"))
            .and(path("/api/v2/organizations/7.json"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({"organization": {"id": 7, "domain_names": ["a.com"]}})),
            )
            .mount(&server)
            .await;
        let c = client(&server);
        let fields = json!({"domain_names": ["a.com"], "shared_tickets": true})
            .as_object()
            .unwrap()
            .clone();
        let out = c.update_organization(7, fields).await.unwrap();
        assert_eq!(out["domain_names"], json!(["a.com"]));
        let requests = server.received_requests().await.unwrap();
        let body: Value = serde_json::from_slice(&requests[0].body).unwrap();
        assert_eq!(
            body,
            json!({"organization": {"domain_names": ["a.com"], "shared_tickets": true}})
        );
        assert!(c.update_organization(7, Map::new()).await.is_err());
        assert_eq!(server.received_requests().await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn group_members_are_trimmed() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v2/groups/3/users.json"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(
                    json!({"users": [{"id": 1, "name": "A", "role": "agent", "x": 1}]}),
                ),
            )
            .mount(&server)
            .await;
        let out = client(&server).get_group_members(3).await.unwrap();
        assert_eq!(out["count"], 1);
        assert_eq!(out["users"][0]["role"], "agent");
        assert!(out["users"][0]["email"].is_null());
        assert!(out["users"][0].get("x").is_none());
    }

    #[tokio::test]
    async fn brands_follow_the_cursor_and_are_trimmed() {
        let server = MockServer::start().await;
        let next = format!("{}/api/v2/brands.json?page%5Bafter%5D=c1", server.uri());
        Mock::given(method("GET"))
            .and(path("/api/v2/brands.json"))
            .and(query_param("page[after]", "c1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "brands": [{"id": 2, "name": "Two"}], "meta": {"has_more": false}
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v2/brands.json"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "brands": [{"id": 1, "name": "One", "default": true, "logo": {}}],
                "meta": {"has_more": true}, "links": {"next": next}
            })))
            .mount(&server)
            .await;
        let out = client(&server).list_brands().await.unwrap();
        assert_eq!(out.as_array().unwrap().len(), 2);
        assert_eq!(out[0]["default"], true);
        assert_eq!(out[1]["ticket_form_ids"], json!([]));
        assert!(out[0].get("logo").is_none());
    }

    #[tokio::test]
    async fn account_settings_return_only_the_chosen_sections() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v2/account/settings.json"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({"settings": {
                    "active_features": {"custom_objects_activated": true},
                    "tickets": {"allow_ccs": true}, "user": {"tagging": true},
                    "billing": {"secret": 1}
                }})),
            )
            .mount(&server)
            .await;
        let out = client(&server).get_account_settings().await.unwrap();
        assert_eq!(out["active_features"]["custom_objects_activated"], true);
        assert_eq!(out["users"], json!({"tagging": true}));
        assert!(out["routing"].is_null());
        assert!(out.get("billing").is_none());
    }
}
