use super::*;

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct SearchArticlesParams {
    /// Search query string to find relevant articles
    query: String,
    /// Optional locale filter (e.g., 'en-us', 'fr', 'es')
    locale: Option<String>,
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

#[tool_router(router = help_center_router, vis = "pub(super)")]
impl ZendeskServer {
    #[tool(
        description = "Search Zendesk help center articles by query string",
        annotations(read_only_hint = true)
    )]
    async fn search_articles(
        &self,
        Parameters(p): Parameters<SearchArticlesParams>,
    ) -> CallToolResult {
        self.call_json(|c| async move {
            c.search_articles(&p.query, p.locale.as_deref(), p.per_page, p.page)
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
}
