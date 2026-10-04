use super::*;

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct SearchArticlesParams {
    /// Search query string to find relevant articles
    query: Option<String>,
    /// Optional locale filter (e.g., 'en-us', 'fr', 'es')
    locale: Option<String>,
    /// Only articles in this category ID
    category: Option<u64>,
    /// Only articles in this section ID
    section: Option<u64>,
    /// Only articles with these labels (sent comma-separated)
    label_names: Option<Vec<String>>,
    /// created_at or updated_at (defaults to relevance)
    sort_by: Option<String>,
    /// asc or desc (defaults to desc)
    sort_order: Option<String>,
    /// Only articles created after this date (YYYY-MM-DD)
    created_after: Option<String>,
    /// Only articles created before this date (YYYY-MM-DD)
    created_before: Option<String>,
    /// Only articles updated after this date (YYYY-MM-DD)
    updated_after: Option<String>,
    /// Only articles updated before this date (YYYY-MM-DD)
    updated_before: Option<String>,
    /// Number of results per page (max 100)
    #[serde(default = "per_page_25")]
    per_page: u64,
    /// Page number (1-based)
    #[serde(default = "page_1")]
    page: u64,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct ListArticlesParams {
    /// Only list the articles in this section
    section_id: Option<u64>,
    /// Optional locale (e.g., 'en-us', 'fr', 'es'); defaults to the help center's default locale
    locale: Option<String>,
    /// Number of articles per page (max 100)
    #[serde(default = "per_page_25")]
    per_page: u64,
    /// Page number (1-based)
    #[serde(default = "page_1")]
    page: u64,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct ArticleParams {
    /// The ID of the article to retrieve
    article_id: u64,
    /// Optional locale (e.g., 'en-us', 'fr', 'es')
    locale: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct ListCategoriesParams {
    /// Optional locale (e.g., 'en-us', 'fr', 'es'); defaults to the help center's default locale
    locale: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct ListSectionsParams {
    /// Only list the sections in this category
    category_id: Option<u64>,
    /// Optional locale (e.g., 'en-us', 'fr', 'es'); defaults to the help center's default locale
    locale: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct ArticleIdParams {
    /// The ID of the article
    article_id: u64,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct CreateArticleParams {
    /// The section to create the article in
    section_id: u64,
    /// The article title
    title: String,
    /// The article body, as Markdown or HTML
    body: String,
    /// Locale of the article (e.g., 'en-us'); must be enabled for the help center
    locale: String,
    /// Permission group that can edit and publish the article (defaults to the admins group)
    permission_group_id: Option<u64>,
    /// User segment that can view the article (omit to make it visible to everyone)
    user_segment_id: Option<u64>,
    /// Labels to attach to the article
    label_names: Option<Vec<String>>,
    /// Create as a draft (defaults to true); false publishes immediately
    #[serde(default = "default_true")]
    draft: bool,
    /// Notify subscribers of the section and article (defaults to false)
    #[serde(default)]
    notify_subscribers: bool,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct UpdateArticleParams {
    /// The ID of the article to update
    article_id: u64,
    /// Locale of the version to edit; required with title, body or draft (e.g., 'en-us')
    locale: Option<String>,
    /// New title for this locale
    title: Option<String>,
    /// New body for this locale, as Markdown or HTML
    body: Option<String>,
    /// false publishes this locale, true unpublishes it
    draft: Option<bool>,
    /// Move the article to this section
    section_id: Option<u64>,
    /// Promote the article (shown as featured)
    promoted: Option<bool>,
    /// Position of the article within its section
    position: Option<i64>,
    /// Replace the article's labels
    label_names: Option<Vec<String>>,
    /// User segment that can view the article
    user_segment_id: Option<u64>,
    /// Permission group that can edit and publish the article
    permission_group_id: Option<u64>,
}

#[tool_router(router = help_center_router, vis = "pub(super)")]
impl ZendeskServer {
    #[tool(
        description = "Search Zendesk help center articles by text and/or filters. Give at least one of query, category, section or label_names. Returns each match with a snippet (matching text in <em> tags), promoted, label_names and vote_sum. Zendesk returns at most 1,000 results per search.",
        annotations(read_only_hint = true)
    )]
    async fn search_articles(
        &self,
        Parameters(p): Parameters<SearchArticlesParams>,
    ) -> CallToolResult {
        self.call_json(|c| async move {
            let labels = p.label_names.unwrap_or_default();
            c.search_articles(&ArticleSearch {
                query: p.query.as_deref(),
                locale: p.locale.as_deref(),
                category: p.category,
                section: p.section,
                label_names: &labels,
                sort_by: p.sort_by.as_deref(),
                sort_order: p.sort_order.as_deref(),
                created_after: p.created_after.as_deref(),
                created_before: p.created_before.as_deref(),
                updated_after: p.updated_after.as_deref(),
                updated_before: p.updated_before.as_deref(),
                per_page: p.per_page,
                page: p.page,
            })
            .await
        })
        .await
    }

    #[tool(
        description = "List Zendesk help center articles one page at a time, optionally only those in one section. Returns titles and links without bodies; use get_article for an article's full text.",
        annotations(read_only_hint = true)
    )]
    async fn list_articles(&self, Parameters(p): Parameters<ListArticlesParams>) -> CallToolResult {
        self.call_json(|c| async move {
            c.list_articles(p.section_id, p.locale.as_deref(), p.page, p.per_page)
                .await
        })
        .await
    }

    #[tool(
        description = "Get a specific Zendesk help center article by its ID",
        annotations(read_only_hint = true)
    )]
    async fn get_article(&self, Parameters(p): Parameters<ArticleParams>) -> CallToolResult {
        self.call_json(|c| async move { c.get_article(p.article_id, p.locale.as_deref()).await })
            .await
    }

    #[tool(
        description = "List Help Center categories (the top level of the knowledge base), as {id, name, description, locale, position, html_url, updated_at}. Use to find the category_id for list_sections or search_articles.",
        annotations(read_only_hint = true)
    )]
    async fn list_categories(
        &self,
        Parameters(p): Parameters<ListCategoriesParams>,
    ) -> CallToolResult {
        self.call_json(|c| async move { c.list_categories(p.locale.as_deref()).await })
            .await
    }

    #[tool(
        description = "List Help Center sections, optionally only those in one category. Returns {id, name, description, category_id, parent_section_id, locale, position, html_url, updated_at}. Use to find the section_id for create_article or list_articles.",
        annotations(read_only_hint = true)
    )]
    async fn list_sections(&self, Parameters(p): Parameters<ListSectionsParams>) -> CallToolResult {
        self.call_json(|c| async move { c.list_sections(p.category_id, p.locale.as_deref()).await })
            .await
    }

    #[tool(
        description = "List every locale version of an article with its draft and outdated state, without bodies. Use get_article with a locale to read one version's text.",
        annotations(read_only_hint = true)
    )]
    async fn list_article_translations(
        &self,
        Parameters(p): Parameters<ArticleIdParams>,
    ) -> CallToolResult {
        self.call_json(|c| async move { c.list_article_translations(p.article_id).await })
            .await
    }

    #[tool(
        description = "Create a Help Center article in a section. It is a draft unless draft is false. permission_group_id defaults to the admins group when omitted; omitting user_segment_id makes it visible to everyone. Publish later with update_article draft=false.",
        annotations(destructive_hint = false)
    )]
    async fn create_article(
        &self,
        Parameters(p): Parameters<CreateArticleParams>,
    ) -> CallToolResult {
        self.call_json(|c| async move {
            let mut article = serde_json::Map::new();
            article.insert("title".into(), p.title.into());
            article.insert("body".into(), p.body.into());
            article.insert("locale".into(), p.locale.into());
            article.insert("draft".into(), p.draft.into());
            article.insert("permission_group_id".into(), json!(p.permission_group_id));
            article.insert("user_segment_id".into(), json!(p.user_segment_id));
            article.insert("label_names".into(), json!(p.label_names));
            let created = c
                .create_article(p.section_id, article, p.notify_subscribers)
                .await?;
            Ok(wrapped("Article created", "article", created))
        })
        .await
    }

    #[tool(
        description = "Edit the text of one locale (title, body, draft; needs locale) and/or the article's metadata (section_id, promoted, position, label_names, user_segment_id, permission_group_id). Set draft to false to publish, true to unpublish. Returns the updated article.",
        annotations(destructive_hint = false, idempotent_hint = true)
    )]
    async fn update_article(
        &self,
        Parameters(p): Parameters<UpdateArticleParams>,
    ) -> CallToolResult {
        self.call_json(|c| async move {
            let translation = serde_json::Map::from_iter([
                ("title".to_string(), json!(p.title)),
                ("body".to_string(), json!(p.body)),
                ("draft".to_string(), json!(p.draft)),
            ]);
            let article = serde_json::Map::from_iter([
                ("section_id".to_string(), json!(p.section_id)),
                ("promoted".to_string(), json!(p.promoted)),
                ("position".to_string(), json!(p.position)),
                ("label_names".to_string(), json!(p.label_names)),
                ("user_segment_id".to_string(), json!(p.user_segment_id)),
                (
                    "permission_group_id".to_string(),
                    json!(p.permission_group_id),
                ),
            ]);
            let updated = c
                .update_article(p.article_id, p.locale.as_deref(), translation, article)
                .await?;
            Ok(wrapped("Article updated", "article", updated))
        })
        .await
    }
}
