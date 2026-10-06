//! Zendesk API client and credentials, shared by `zendesk-mcp-server` and `zendesk`.

pub mod auth;
pub mod authorize;
pub mod catalog;
pub mod client;
pub mod config;
pub mod oauth;
pub mod tokens;

pub use client::{ArticleSearch, CreateTicket, ZendeskClient};
