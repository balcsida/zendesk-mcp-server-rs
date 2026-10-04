use super::*;

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct UserIdParams {
    /// The user ID
    user_id: u64,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct SearchUsersParams {
    /// Name, email, notes, phone or other user property to search for
    query: Option<String>,
    /// Exact external_id to match (not a search expression)
    external_id: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct OrganizationIdParams {
    /// The organization ID
    organization_id: u64,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct SearchOrganizationsParams {
    /// Organization name to search for
    query: String,
}

#[tool_router(router = people_router, vis = "pub(super)")]
impl ZendeskServer {
    #[tool(
        description = "Get a Zendesk user by their ID. Use this to resolve requester_id or assignee_id from tickets.",
        annotations(read_only_hint = true)
    )]
    async fn get_user(&self, Parameters(p): Parameters<UserIdParams>) -> CallToolResult {
        self.call_json(|c| async move { c.get_user(p.user_id).await })
            .await
    }

    #[tool(
        description = "Get the currently authenticated Zendesk user",
        annotations(read_only_hint = true)
    )]
    async fn get_current_user(&self) -> CallToolResult {
        self.call_json(|c| async move { c.get_current_user().await })
            .await
    }

    #[tool(
        description = "Search Zendesk users by name, email or other properties (query) or by exact external_id; give at least one. Zendesk returns at most 10,000 matches.",
        annotations(read_only_hint = true)
    )]
    async fn search_users(&self, Parameters(p): Parameters<SearchUsersParams>) -> CallToolResult {
        self.call_json(|c| async move {
            c.search_users(p.query.as_deref(), p.external_id.as_deref())
                .await
        })
        .await
    }

    #[tool(
        description = "Get a Zendesk organization by its ID",
        annotations(read_only_hint = true)
    )]
    async fn get_organization(
        &self,
        Parameters(p): Parameters<OrganizationIdParams>,
    ) -> CallToolResult {
        self.call_json(|c| async move { c.get_organization(p.organization_id).await })
            .await
    }

    #[tool(
        description = "Search Zendesk organizations by name",
        annotations(read_only_hint = true)
    )]
    async fn search_organizations(
        &self,
        Parameters(p): Parameters<SearchOrganizationsParams>,
    ) -> CallToolResult {
        self.call_json(|c| async move { c.search_organizations(&p.query).await })
            .await
    }

    #[tool(
        description = "List assignable Zendesk groups for ticket routing",
        annotations(read_only_hint = true)
    )]
    async fn list_groups(&self) -> CallToolResult {
        self.call_json(|c| async move { c.list_groups().await })
            .await
    }
}
