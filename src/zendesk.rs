//! Zendesk REST API client.
//!
//! Every method returns `serde_json::Value` shaped exactly like the Python server's
//! output, so MCP clients see no difference after the rewrite.

use anyhow::Result;
use serde_json::Value;

use crate::auth::Auth;

/// 10 MB hard cap on attachments, against image bombs and token budget blowout.
pub const MAX_ATTACHMENT_BYTES: usize = 10 * 1024 * 1024;

/// Image types a tool may return. SVG is excluded: it can contain active content.
pub const ALLOWED_IMAGE_TYPES: [&str; 4] = ["image/jpeg", "image/png", "image/gif", "image/webp"];

/// Render Markdown (or plain text) to the HTML Zendesk stores as `html_body`.
///
/// CommonMark plus tables and strikethrough; a single newline becomes `<br>` so plain
/// text keeps its line breaks; raw HTML in the input is passed through for Zendesk to
/// sanitize server-side.
pub fn markdown_to_html(text: &str) -> String {
    let _ = text;
    todo!("worker: client")
}

/// A fetched, validated image attachment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attachment {
    pub content_type: String,
    /// Base64 (standard alphabet, padded) of the file bytes.
    pub data_base64: String,
}

/// Fields accepted when creating a ticket.
#[derive(Debug, Clone, Default)]
pub struct CreateTicket {
    pub subject: String,
    pub description: String,
    pub requester_id: Option<u64>,
    pub assignee_id: Option<u64>,
    pub priority: Option<String>,
    pub ticket_type: Option<String>,
    pub tags: Option<Vec<String>>,
    pub custom_fields: Option<Vec<Value>>,
}

#[derive(Clone)]
pub struct ZendeskClient {
    http: reqwest::Client,
    subdomain: String,
    /// `https://{subdomain}.zendesk.com/api/v2` in production; tests point it at a mock.
    base_url: String,
    auth: Auth,
}

impl ZendeskClient {
    pub fn new(subdomain: &str, auth: Auth, http: reqwest::Client) -> Self {
        let base_url = format!("https://{subdomain}.zendesk.com/api/v2");
        Self::with_base_url(subdomain, auth, http, base_url)
    }

    pub fn with_base_url(
        subdomain: &str,
        auth: Auth,
        http: reqwest::Client,
        base_url: String,
    ) -> Self {
        ZendeskClient {
            http,
            subdomain: subdomain.to_string(),
            base_url,
            auth,
        }
    }

    pub fn subdomain(&self) -> &str {
        &self.subdomain
    }

    pub async fn get_ticket(&self, ticket_id: u64) -> Result<Value> {
        let _ = ticket_id;
        todo!("worker: client")
    }

    pub async fn get_ticket_comments(&self, ticket_id: u64) -> Result<Value> {
        let _ = ticket_id;
        todo!("worker: client")
    }

    pub async fn get_ticket_attachment(&self, content_url: &str) -> Result<Attachment> {
        let _ = content_url;
        todo!("worker: client")
    }

    pub async fn post_comment(
        &self,
        ticket_id: u64,
        comment: &str,
        public: bool,
    ) -> Result<String> {
        let _ = (ticket_id, comment, public);
        todo!("worker: client")
    }

    pub async fn get_tickets(
        &self,
        page: u64,
        per_page: u64,
        sort_by: &str,
        sort_order: &str,
    ) -> Result<Value> {
        let _ = (page, per_page, sort_by, sort_order);
        todo!("worker: client")
    }

    pub async fn get_all_articles(&self) -> Result<Value> {
        todo!("worker: client")
    }

    pub async fn search_articles(
        &self,
        query: &str,
        locale: Option<&str>,
        per_page: u64,
        page: u64,
    ) -> Result<Value> {
        let _ = (query, locale, per_page, page);
        todo!("worker: client")
    }

    pub async fn get_article(&self, article_id: u64, locale: Option<&str>) -> Result<Value> {
        let _ = (article_id, locale);
        todo!("worker: client")
    }

    pub async fn create_ticket(&self, ticket: CreateTicket) -> Result<Value> {
        let _ = ticket;
        todo!("worker: client")
    }

    /// `fields` are the ticket attributes to set (subject, status, priority, type,
    /// assignee_id, requester_id, tags, custom_fields, due_at, ...). Null values are skipped.
    pub async fn update_ticket(
        &self,
        ticket_id: u64,
        fields: serde_json::Map<String, Value>,
    ) -> Result<Value> {
        let _ = (ticket_id, fields);
        todo!("worker: client")
    }

    pub async fn search(
        &self,
        query: &str,
        page: u64,
        per_page: u64,
        sort_by: &str,
        sort_order: &str,
    ) -> Result<Value> {
        let _ = (query, page, per_page, sort_by, sort_order);
        todo!("worker: client")
    }

    pub async fn get_user(&self, user_id: u64) -> Result<Value> {
        let _ = user_id;
        todo!("worker: client")
    }

    pub async fn get_current_user(&self) -> Result<Value> {
        todo!("worker: client")
    }

    pub async fn search_users(&self, query: &str) -> Result<Value> {
        let _ = query;
        todo!("worker: client")
    }

    pub async fn list_views(&self) -> Result<Value> {
        todo!("worker: client")
    }

    pub async fn execute_view(&self, view_id: u64, page: u64, per_page: u64) -> Result<Value> {
        let _ = (view_id, page, per_page);
        todo!("worker: client")
    }

    pub async fn list_ticket_fields(&self) -> Result<Value> {
        todo!("worker: client")
    }

    pub async fn get_organization(&self, organization_id: u64) -> Result<Value> {
        let _ = organization_id;
        todo!("worker: client")
    }

    pub async fn search_organizations(&self, query: &str) -> Result<Value> {
        let _ = query;
        todo!("worker: client")
    }

    pub async fn get_tickets_bulk(&self, ticket_ids: &[u64]) -> Result<Value> {
        let _ = ticket_ids;
        todo!("worker: client")
    }

    pub async fn list_groups(&self) -> Result<Value> {
        todo!("worker: client")
    }

    pub async fn merge_tickets(
        &self,
        target_id: u64,
        source_ids: &[u64],
        target_comment: &str,
        source_comment: &str,
    ) -> Result<Value> {
        let _ = (target_id, source_ids, target_comment, source_comment);
        todo!("worker: client")
    }

    pub async fn list_macros(&self, active_only: bool) -> Result<Value> {
        let _ = active_only;
        todo!("worker: client")
    }

    pub async fn apply_macro(&self, ticket_id: u64, macro_id: u64) -> Result<Value> {
        let _ = (ticket_id, macro_id);
        todo!("worker: client")
    }

    /// `role` must be one of `requested`, `assigned`, `ccd`.
    pub async fn get_user_tickets(
        &self,
        user_id: u64,
        role: &str,
        page: u64,
        per_page: u64,
    ) -> Result<Value> {
        let _ = (user_id, role, page, per_page);
        todo!("worker: client")
    }

    pub async fn list_ticket_forms(&self) -> Result<Value> {
        todo!("worker: client")
    }

    pub async fn delete_ticket(&self, ticket_id: u64) -> Result<()> {
        let _ = ticket_id;
        todo!("worker: client")
    }

    pub async fn get_ticket_metrics(&self, ticket_id: u64) -> Result<Value> {
        let _ = ticket_id;
        todo!("worker: client")
    }

    pub async fn get_sla_breaches(&self, days_back: u64, metric: Option<&str>) -> Result<Value> {
        let _ = (days_back, metric);
        todo!("worker: client")
    }

    pub async fn get_sla_policies(&self) -> Result<Value> {
        todo!("worker: client")
    }
}
