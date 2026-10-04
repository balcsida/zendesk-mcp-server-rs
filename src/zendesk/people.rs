use anyhow::Result;
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
                ],
                &[],
            ))
        }
        .await
        .map_err(ctx("Failed to get current user"))
    }

    pub async fn search_users(&self, query: &str) -> Result<Value> {
        async {
            let data = self
                .api_get("users/search.json", &[("query", &query)])
                .await?;
            Ok(pick_all(
                &data,
                "users",
                &["id", "name", "email", "role", "organization_id", "active"],
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
