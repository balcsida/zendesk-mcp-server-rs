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

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct GroupIdParams {
    /// The group ID from list_groups
    group_id: u64,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct UserIdsParams {
    /// The user IDs to fetch (chunked into requests of 100)
    user_ids: Vec<u64>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct CreateOrUpdateUserParams {
    name: String,
    /// Matches an existing user by this email
    email: String,
    /// Also matches an existing user by this external ID
    external_id: Option<String>,
    phone: Option<String>,
    organization_id: Option<u64>,
    tags: Option<Vec<String>>,
    notes: Option<String>,
    details: Option<String>,
    /// Custom user field values as {"field_key": value}
    user_fields: Option<serde_json::Map<String, Value>>,
    /// Locale such as "en-US"
    locale: Option<String>,
    /// Time zone name such as "Europe/Budapest"
    time_zone: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct UpdateUserParams {
    /// The user ID
    user_id: u64,
    name: Option<String>,
    /// Added as a secondary identity; it does not replace the primary email
    email: Option<String>,
    phone: Option<String>,
    organization_id: Option<u64>,
    external_id: Option<String>,
    /// Replaces the whole tag list
    tags: Option<Vec<String>>,
    notes: Option<String>,
    details: Option<String>,
    /// Custom user field values as {"field_key": value}
    user_fields: Option<serde_json::Map<String, Value>>,
    /// Locale such as "en-US"
    locale: Option<String>,
    /// Time zone name such as "Europe/Budapest"
    time_zone: Option<String>,
    /// Name shown to end users
    alias: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct OrganizationUsersParams {
    /// The organization ID
    organization_id: u64,
    #[serde(default = "page_1")]
    page: u64,
    /// Number of users per page (max 100)
    #[serde(default = "per_page_25")]
    per_page: u64,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct UpdateOrganizationParams {
    /// The organization ID
    organization_id: u64,
    name: Option<String>,
    /// Replaces the whole list of domain names
    domain_names: Option<Vec<String>>,
    details: Option<String>,
    notes: Option<String>,
    external_id: Option<String>,
    /// New tickets from the organization's users go to this group
    group_id: Option<u64>,
    /// Replaces the whole tag list
    tags: Option<Vec<String>>,
    /// Custom organization field values as {"field_key": value}
    organization_fields: Option<serde_json::Map<String, Value>>,
    /// End users can see each other's tickets
    shared_tickets: Option<bool>,
    /// End users can comment on each other's tickets
    shared_comments: Option<bool>,
}

/// The request body fields that were set; unset (null) values are skipped.
fn set_fields<const N: usize>(pairs: [(&str, Value); N]) -> serde_json::Map<String, Value> {
    pairs
        .into_iter()
        .filter(|(_, v)| !v.is_null())
        .map(|(k, v)| (k.to_string(), v))
        .collect()
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
        description = "Search Zendesk users by name, email or other properties (query) or by exact external_id; give at least one. Returns the first page of up to 100 matches with has_more; narrow the query for more.",
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
        description = "Search Zendesk organizations whose name starts with the query. Returns the first page of matches with has_more; narrow the query for more.",
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

    #[tool(
        description = "Get many users by ID in one call (name, email, role, organization_id, external_id, active, suspended, time_zone, locale). Use it to resolve many requester or assignee IDs at once.",
        annotations(read_only_hint = true)
    )]
    async fn get_users_bulk(&self, Parameters(p): Parameters<UserIdsParams>) -> CallToolResult {
        self.call_json(|c| async move { c.get_users_bulk(&p.user_ids).await })
            .await
    }

    #[tool(
        description = "List the identities of a user: emails, phone numbers and other channels, with their verification and deliverability state.",
        annotations(read_only_hint = true)
    )]
    async fn get_user_identities(&self, Parameters(p): Parameters<UserIdParams>) -> CallToolResult {
        self.call_json(|c| async move { c.get_user_identities(p.user_id).await })
            .await
    }

    #[tool(
        description = "List the organizations a user belongs to. A user can belong to several; `default` marks the primary one.",
        annotations(read_only_hint = true)
    )]
    async fn get_user_organizations(
        &self,
        Parameters(p): Parameters<UserIdParams>,
    ) -> CallToolResult {
        self.call_json(|c| async move { c.get_user_organizations(p.user_id).await })
            .await
    }

    #[tool(
        description = "Create a user, or update the existing one that matches the email or external_id. A new user is an end user; this tool never changes the role.",
        annotations(destructive_hint = true, idempotent_hint = true)
    )]
    async fn create_or_update_user(
        &self,
        Parameters(p): Parameters<CreateOrUpdateUserParams>,
    ) -> CallToolResult {
        self.call_json(|c| async move {
            let fields = set_fields([
                ("name", json!(p.name)),
                ("email", json!(p.email)),
                ("external_id", json!(p.external_id)),
                ("phone", json!(p.phone)),
                ("organization_id", json!(p.organization_id)),
                ("tags", json!(p.tags)),
                ("notes", json!(p.notes)),
                ("details", json!(p.details)),
                ("user_fields", json!(p.user_fields)),
                ("locale", json!(p.locale)),
                ("time_zone", json!(p.time_zone)),
            ]);
            let user = c.create_or_update_user(fields).await?;
            Ok(wrapped("User created or updated", "user", user))
        })
        .await
    }

    #[tool(
        description = "Update a user's profile. Role, suspension and password changes are deliberately not supported. A new email is added as a secondary identity (Zendesk behaviour), not made primary.",
        annotations(destructive_hint = true, idempotent_hint = true)
    )]
    async fn update_user(&self, Parameters(p): Parameters<UpdateUserParams>) -> CallToolResult {
        self.call_json(|c| async move {
            let fields = set_fields([
                ("name", json!(p.name)),
                ("email", json!(p.email)),
                ("phone", json!(p.phone)),
                ("organization_id", json!(p.organization_id)),
                ("external_id", json!(p.external_id)),
                ("tags", json!(p.tags)),
                ("notes", json!(p.notes)),
                ("details", json!(p.details)),
                ("user_fields", json!(p.user_fields)),
                ("locale", json!(p.locale)),
                ("time_zone", json!(p.time_zone)),
                ("alias", json!(p.alias)),
            ]);
            let user = c.update_user(p.user_id, fields).await?;
            Ok(wrapped("User updated", "user", user))
        })
        .await
    }

    #[tool(
        description = "List the users of an organization (id, name, email, role, active, suspended, external_id, phone), one page at a time.",
        annotations(read_only_hint = true)
    )]
    async fn list_organization_users(
        &self,
        Parameters(p): Parameters<OrganizationUsersParams>,
    ) -> CallToolResult {
        self.call_json(|c| async move {
            c.list_organization_users(p.organization_id, p.page, p.per_page)
                .await
        })
        .await
    }

    #[tool(
        description = "Update an organization. domain_names and tags replace the whole list. Agents without extra permission can usually change only notes.",
        annotations(destructive_hint = true, idempotent_hint = true)
    )]
    async fn update_organization(
        &self,
        Parameters(p): Parameters<UpdateOrganizationParams>,
    ) -> CallToolResult {
        self.call_json(|c| async move {
            let fields = set_fields([
                ("name", json!(p.name)),
                ("domain_names", json!(p.domain_names)),
                ("details", json!(p.details)),
                ("notes", json!(p.notes)),
                ("external_id", json!(p.external_id)),
                ("group_id", json!(p.group_id)),
                ("tags", json!(p.tags)),
                ("organization_fields", json!(p.organization_fields)),
                ("shared_tickets", json!(p.shared_tickets)),
                ("shared_comments", json!(p.shared_comments)),
            ]);
            let organization = c.update_organization(p.organization_id, fields).await?;
            Ok(wrapped(
                "Organization updated",
                "organization",
                organization,
            ))
        })
        .await
    }

    #[tool(
        description = "List the members of a group (id, name, email, role, active, suspended). Group IDs come from list_groups.",
        annotations(read_only_hint = true)
    )]
    async fn get_group_members(&self, Parameters(p): Parameters<GroupIdParams>) -> CallToolResult {
        self.call_json(|c| async move { c.get_group_members(p.group_id).await })
            .await
    }

    #[tool(
        description = "List the brands of the account (id, name, subdomain, brand_url, default, active, has_help_center, help_center_state, ticket_form_ids). Maps brand_id on tickets to names; agents may see only their own brands.",
        annotations(read_only_hint = true)
    )]
    async fn list_brands(&self) -> CallToolResult {
        self.call_json(|c| async move { c.list_brands().await })
            .await
    }

    #[tool(
        description = "Get account settings: active_features (such as on_hold_status, business_hours, allow_ccs) and the brands, tickets, agents, localization, limits, routing and users defaults. Tells you which features are on; for custom objects, try list_custom_objects and treat 403 or 404 as 'not available'.",
        annotations(read_only_hint = true)
    )]
    async fn get_account_settings(&self) -> CallToolResult {
        self.call_json(|c| async move { c.get_account_settings().await })
            .await
    }
}
