//! Help Center: articles, categories, sections and translations.

use std::collections::{BTreeSet, HashMap};

use anyhow::{Context, Result, bail};
use serde_json::{Map, Value, json};

use super::*;

/// Inputs of `search_articles`; dates are `YYYY-MM-DD`.
#[derive(Clone, Copy, Default)]
pub struct ArticleSearch<'a> {
    pub query: Option<&'a str>,
    pub locale: Option<&'a str>,
    pub category: Option<u64>,
    pub section: Option<u64>,
    pub label_names: &'a [String],
    pub sort_by: Option<&'a str>,
    pub sort_order: Option<&'a str>,
    pub created_after: Option<&'a str>,
    pub created_before: Option<&'a str>,
    pub updated_after: Option<&'a str>,
    pub updated_before: Option<&'a str>,
    pub per_page: u64,
    pub page: u64,
}

/// The article shape `get_article`, `create_article` and `update_article` return.
fn article_detail(article: &Value) -> Value {
    let mut out = pick(
        article,
        &[
            "id",
            "title",
            "body",
            "author_id",
            "section_id",
            "locale",
            "source_locale",
            "html_url",
            "created_at",
            "updated_at",
            "edited_at",
            "position",
            "vote_sum",
            "vote_count",
            "label_names",
        ],
        &["label_names"],
    );
    for key in ["draft", "promoted"] {
        out[key] = article.get(key).cloned().unwrap_or(json!(false));
    }
    out
}

impl ZendeskClient {
    /// Up to `max_articles` articles in the sections the caller can view, grouped by
    /// section ID. The second value is true when the cap cut the listing short.
    pub async fn get_all_articles(&self, max_articles: usize) -> Result<(Value, bool)> {
        async {
            let mut truncated = false;
            // One more than the cap, to tell "exactly the cap" from "cut short".
            let mut sections = self
                .get_cursor_paged(
                    "help_center/sections.json",
                    &[],
                    "sections",
                    max_articles.saturating_add(1),
                )
                .await?;
            if sections.len() > max_articles {
                sections.truncate(max_articles);
                truncated = true;
            }
            // A section's articles live under its own locale; the locale-less path only
            // serves the default one, so non-English help centers came back empty
            // (upstream issue #10).
            let locale_of: HashMap<u64, Option<&str>> = sections
                .iter()
                .filter_map(|s| Some((s["id"].as_u64()?, s["locale"].as_str())))
                .collect();
            let locales: BTreeSet<Option<&str>> = locale_of.values().copied().collect();
            // One listing per locale rather than one per section: tens of requests, not
            // hundreds.
            let mut by_section: HashMap<u64, Vec<Value>> = HashMap::new();
            let mut collected = 0;
            'locales: for locale in locales {
                let path = format!("{}/articles.json", help_center_path(locale)?);
                let budget = (max_articles - collected).saturating_add(1);
                for a in self
                    .get_cursor_paged(&path, &[], "articles", budget)
                    .await?
                {
                    let Some(section_id) = a["section_id"].as_u64() else {
                        continue;
                    };
                    // Skip translations whose section is listed under another locale, and
                    // articles in sections the caller cannot view.
                    if locale_of.get(&section_id) == Some(&locale) {
                        if collected == max_articles {
                            truncated = true;
                            break 'locales;
                        }
                        collected += 1;
                        let mut out = pick(&a, &["id", "title", "body", "updated_at"], &[]);
                        out["url"] = a["html_url"].clone();
                        by_section.entry(section_id).or_default().push(out);
                    }
                }
            }
            let mut kb = Map::new();
            for section in &sections {
                let articles = section["id"]
                    .as_u64()
                    .and_then(|id| by_section.remove(&id))
                    .unwrap_or_default();
                // Keyed by ID: names repeat across categories, and a name key let a later
                // section overwrite an earlier one.
                kb.insert(
                    section["id"].to_string(),
                    json!({
                        "name": section["name"],
                        "description": section["description"],
                        "articles": articles,
                    }),
                );
            }
            anyhow::Ok((Value::Object(kb), truncated))
        }
        .await
        .context("Failed to fetch knowledge base")
    }

    /// Search is capped by Zendesk at 1,000 results. Needs a query, category, section or
    /// label names.
    ///
    /// # Errors
    ///
    /// Fails without a request when none of those is given, or `sort_by` or `sort_order`
    /// is not one Zendesk accepts.
    pub async fn search_articles(&self, search: &ArticleSearch<'_>) -> Result<Value> {
        async {
            let ArticleSearch {
                query,
                locale,
                category,
                section,
                label_names,
                sort_by,
                sort_order,
                created_after,
                created_before,
                updated_after,
                updated_before,
                per_page,
                page,
            } = *search;
            if query.is_none_or(str::is_empty)
                && category.is_none()
                && section.is_none()
                && label_names.is_empty()
            {
                bail!("Give at least one of query, category, section or label_names");
            }
            if let Some(sort_by) = sort_by
                && !["created_at", "updated_at"].contains(&sort_by)
            {
                bail!("Invalid sort_by '{sort_by}'. Allowed: created_at, updated_at");
            }
            if let Some(sort_order) = sort_order
                && !["asc", "desc"].contains(&sort_order)
            {
                bail!("Invalid sort_order '{sort_order}'. Allowed: asc, desc");
            }
            let per_page = per_page.min(MAX_PAGE_SIZE);
            let labels = label_names.join(",");
            let mut params: Vec<(&str, &(dyn Display + Sync))> =
                vec![("per_page", &per_page), ("page", &page)];
            if let Some(v) = &query {
                params.push(("query", v));
            }
            if let Some(v) = &category {
                params.push(("category", v));
            }
            if let Some(v) = &section {
                params.push(("section", v));
            }
            if !label_names.is_empty() {
                params.push(("label_names", &labels));
            }
            let extras = [
                ("locale", locale.filter(|l| !l.is_empty())),
                ("sort_by", sort_by),
                ("sort_order", sort_order),
                ("created_after", created_after),
                ("created_before", created_before),
                ("updated_after", updated_after),
                ("updated_before", updated_before),
            ];
            for (key, value) in &extras {
                if let Some(v) = value {
                    params.push((key, v));
                }
            }
            let data = self
                .api_get("help_center/articles/search.json", &params)
                .await?;
            let mut articles = pick_all(
                &data,
                "results",
                &[
                    "id",
                    "title",
                    "body",
                    "author_id",
                    "section_id",
                    "locale",
                    "html_url",
                    "created_at",
                    "updated_at",
                    "snippet",
                    "vote_sum",
                ],
                &[],
            );
            for (article, raw) in articles
                .as_array_mut()
                .into_iter()
                .flatten()
                .zip(data["results"].as_array().into_iter().flatten())
            {
                for key in ["draft", "promoted"] {
                    article[key] = raw.get(key).cloned().unwrap_or(json!(false));
                }
                article["label_names"] = raw.get("label_names").cloned().unwrap_or(json!([]));
            }
            let count = articles.as_array().map_or(0, Vec::len);
            anyhow::Ok(json!({
                "articles": articles,
                "query": query,
                "page": page,
                "per_page": per_page,
                "count": count,
                "total_count": data.get("count").cloned().unwrap_or_else(|| json!(count)),
                "has_more": !data["next_page"].is_null(),
            }))
        }
        .await
        .context("Failed to search articles")
    }

    /// One page of articles, from the whole help center or one section, without bodies.
    pub async fn list_articles(
        &self,
        section_id: Option<u64>,
        locale: Option<&str>,
        page: u64,
        per_page: u64,
    ) -> Result<Value> {
        async {
            let per_page = per_page.min(MAX_PAGE_SIZE);
            let section = section_id
                .map(|id| format!("/sections/{id}"))
                .unwrap_or_default();
            let path = format!("{}{section}/articles.json", help_center_path(locale)?);
            let data = self
                .api_get(&path, &[("page", &page), ("per_page", &per_page)])
                .await?;
            let articles = pick_all(
                &data,
                "articles",
                &[
                    "id",
                    "title",
                    "section_id",
                    "html_url",
                    "draft",
                    "updated_at",
                ],
                &[],
            );
            let count = articles.as_array().map_or(0, Vec::len);
            anyhow::Ok(json!({
                "articles": articles,
                "page": page,
                "per_page": per_page,
                "count": count,
                "total_count": data.get("count").cloned().unwrap_or_else(|| json!(count)),
                "has_more": !data["next_page"].is_null(),
            }))
        }
        .await
        .context("Failed to list articles")
    }

    pub async fn get_article(&self, article_id: u64, locale: Option<&str>) -> Result<Value> {
        async {
            let path = format!("{}/articles/{article_id}.json", help_center_path(locale)?);
            let data = self.api_get(&path, &[]).await?;
            anyhow::Ok(article_detail(object(&data, "article")?))
        }
        .await
        .with_context(|| format!("Failed to get article {article_id}"))
    }

    pub async fn list_categories(&self, locale: Option<&str>) -> Result<Value> {
        async {
            let path = format!("{}/categories.json", help_center_path(locale)?);
            let categories = self.get_paged(&path, "categories").await?;
            anyhow::Ok(pick_each(
                &categories,
                &[
                    "id",
                    "name",
                    "description",
                    "locale",
                    "position",
                    "html_url",
                    "updated_at",
                ],
                &[],
            ))
        }
        .await
        .context("Failed to list categories")
    }

    pub async fn list_sections(
        &self,
        category_id: Option<u64>,
        locale: Option<&str>,
    ) -> Result<Value> {
        async {
            let category = category_id
                .map(|id| format!("/categories/{id}"))
                .unwrap_or_default();
            let path = format!("{}{category}/sections.json", help_center_path(locale)?);
            let sections = self.get_paged(&path, "sections").await?;
            anyhow::Ok(pick_each(
                &sections,
                &[
                    "id",
                    "name",
                    "description",
                    "category_id",
                    "parent_section_id",
                    "locale",
                    "position",
                    "html_url",
                    "updated_at",
                ],
                &[],
            ))
        }
        .await
        .context("Failed to list sections")
    }

    /// Every locale version of an article, without bodies.
    pub async fn list_article_translations(&self, article_id: u64) -> Result<Value> {
        async {
            let translations = self
                .get_paged(
                    &format!("help_center/articles/{article_id}/translations.json"),
                    "translations",
                )
                .await?;
            let mut out = pick_each(
                &translations,
                &["id", "locale", "title", "html_url", "updated_at"],
                &[],
            );
            for (t, raw) in out.as_array_mut().into_iter().flatten().zip(&translations) {
                for key in ["draft", "outdated"] {
                    t[key] = raw.get(key).cloned().unwrap_or(json!(false));
                }
            }
            anyhow::Ok(out)
        }
        .await
        .with_context(|| format!("Failed to list translations of article {article_id}"))
    }

    /// Creates an article in `section_id`. `article` holds the article fields; a Markdown
    /// `body` is converted to HTML.
    pub async fn create_article(
        &self,
        section_id: u64,
        mut article: Map<String, Value>,
        notify_subscribers: bool,
    ) -> Result<Value> {
        async {
            article.retain(|_, v| !v.is_null());
            if let Some(body) = article.get("body").and_then(Value::as_str) {
                article.insert("body".into(), markdown_to_html(body).into());
            }
            let data = self
                .api_post(
                    &format!("help_center/sections/{section_id}/articles.json"),
                    &json!({"article": article, "notify_subscribers": notify_subscribers}),
                )
                .await?;
            anyhow::Ok(article_detail(object(&data, "article")?))
        }
        .await
        .with_context(|| format!("Failed to create article in section {section_id}"))
    }

    /// Updates one locale's `translation` (title, body, draft; needs `locale`) and/or the
    /// article's own `article` fields, then returns the refreshed article.
    ///
    /// # Errors
    ///
    /// Fails without a request when both maps are empty or `locale` is missing while
    /// `translation` is set. If the translation is saved but the article update then
    /// fails, the error says so.
    pub async fn update_article(
        &self,
        article_id: u64,
        locale: Option<&str>,
        mut translation: Map<String, Value>,
        mut article: Map<String, Value>,
    ) -> Result<Value> {
        async {
            translation.retain(|_, v| !v.is_null());
            article.retain(|_, v| !v.is_null());
            if translation.is_empty() && article.is_empty() {
                bail!("Nothing to update: give at least one field to change");
            }
            let locale = locale.filter(|l| !l.is_empty());
            if !translation.is_empty() && locale.is_none() {
                bail!("locale is required to change title, body or draft");
            }
            if let Some(body) = translation.get("body").and_then(Value::as_str) {
                translation.insert("body".into(), markdown_to_html(body).into());
            }
            let mut translated = false;
            if let Some(locale) = locale
                && !translation.is_empty()
            {
                let path = format!(
                    "help_center/articles/{article_id}/translations/{}.json",
                    segment(locale)?
                );
                self.api_put(&path, &json!({ "translation": translation }))
                    .await?;
                translated = true;
            }
            if !article.is_empty() {
                self.api_put(
                    &format!("help_center/articles/{article_id}.json"),
                    &json!({ "article": article }),
                )
                .await
                .map_err(|e| {
                    if translated {
                        e.context("the translation was updated, but updating the article failed")
                    } else {
                        e
                    }
                })?;
            }
            self.get_article(article_id, locale).await
        }
        .await
        .with_context(|| format!("Failed to update article {article_id}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::test_support::*;
    use wiremock::matchers::{method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn knowledge_base_lists_articles_once_per_section_locale() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v2/help_center/sections.json"))
            .respond_with(json_page(
                "sections",
                json!([
                    {"id": 1, "name": "FAQ", "description": "a", "locale": "en-us"},
                    {"id": 2, "name": "FAQ", "description": "b", "locale": "en-us"},
                    {"id": 3, "name": "Ajuda", "description": "c", "locale": "pt-br"}
                ]),
                None,
            ))
            .mount(&server)
            .await;
        let article = |id: u64, section_id: u64| {
            json!({"id": id, "section_id": section_id, "title": "t", "body": "b",
                "updated_at": "u", "html_url": "h"})
        };
        // Article 30 is in a section the caller cannot view; the pt-br listing also holds a
        // translation of article 10, whose section is listed under en-us.
        for (locale, articles) in [
            (
                "en-us",
                json!([article(10, 1), article(20, 2), article(30, 9)]),
            ),
            ("pt-br", json!([article(40, 3), article(10, 1)])),
        ] {
            Mock::given(method("GET"))
                .and(path(format!("/api/v2/help_center/{locale}/articles.json")))
                .and(query_param("page[size]", "100"))
                .respond_with(json_page("articles", articles, None))
                .expect(1)
                .mount(&server)
                .await;
        }
        let (kb, truncated) = client(&server).get_all_articles(500).await.unwrap();
        assert!(!truncated);
        let ids = |section: &str| -> Vec<Value> {
            kb[section]["articles"]
                .as_array()
                .unwrap()
                .iter()
                .map(|a| a["id"].clone())
                .collect()
        };
        assert_eq!(kb["1"]["name"], "FAQ");
        assert_eq!(kb["2"]["name"], "FAQ");
        assert_eq!(ids("1"), [json!(10)]);
        assert_eq!(ids("2"), [json!(20)]);
        assert_eq!(ids("3"), [json!(40)]);
        assert_eq!(
            kb["1"]["articles"][0],
            json!({"id": 10, "title": "t", "body": "b", "updated_at": "u", "url": "h"})
        );
    }

    #[tokio::test]
    async fn knowledge_base_stops_at_max_articles_and_says_so() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v2/help_center/sections.json"))
            .respond_with(json_page(
                "sections",
                json!([{"id": 1, "name": "FAQ", "description": "a", "locale": "en-us"}]),
                None,
            ))
            .mount(&server)
            .await;
        let article = |id: u64| json!({"id": id, "section_id": 1, "html_url": "h"});
        let next = format!(
            "{}/api/v2/help_center/en-us/articles.json?page%5Bafter%5D=2",
            server.uri()
        );
        Mock::given(method("GET"))
            .and(path("/api/v2/help_center/en-us/articles.json"))
            .and(query_param("page[after]", "2"))
            .respond_with(cursor_articles(json!([article(3), article(4)]), None))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v2/help_center/en-us/articles.json"))
            .respond_with(cursor_articles(json!([article(1), article(2)]), Some(next)))
            .mount(&server)
            .await;
        let c = client(&server);
        let (kb, truncated) = c.get_all_articles(3).await.unwrap();
        assert!(truncated);
        assert_eq!(kb["1"]["articles"].as_array().unwrap().len(), 3);
        // Exactly the total is not a cut.
        let (kb, truncated) = c.get_all_articles(4).await.unwrap();
        assert!(!truncated);
        assert_eq!(kb["1"]["articles"].as_array().unwrap().len(), 4);
    }

    fn cursor_articles(items: Value, next: Option<String>) -> ResponseTemplate {
        let mut body = json!({ "meta": { "has_more": next.is_some() } });
        body["articles"] = items;
        body["links"]["next"] = next.into();
        ResponseTemplate::new(200).set_body_json(body)
    }

    #[tokio::test]
    async fn list_articles_pages_one_section_or_all_without_bodies() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v2/help_center/pt-br/sections/5/articles.json"))
            .and(query_param("page", "2"))
            .and(query_param("per_page", "100"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "articles": [{"id": 1, "title": "t", "body": "<p>long</p>", "section_id": 5,
                    "html_url": "h", "draft": false, "updated_at": "u"}],
                "count": 250, "next_page": "https://x/next"
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v2/help_center/articles.json"))
            .respond_with(json_page("articles", json!([]), None))
            .mount(&server)
            .await;
        let c = client(&server);
        let out = c
            .list_articles(Some(5), Some("pt-br"), 2, 500)
            .await
            .unwrap();
        assert_eq!(
            out["articles"],
            json!([{"id": 1, "title": "t", "section_id": 5, "html_url": "h",
                "draft": false, "updated_at": "u"}])
        );
        assert_eq!(out["per_page"], 100);
        assert_eq!(out["total_count"], 250);
        assert_eq!(out["has_more"], true);
        let out = c.list_articles(None, None, 1, 25).await.unwrap();
        assert_eq!(out["count"], 0);
        assert_eq!(out["has_more"], false);
    }

    #[tokio::test]
    async fn navigation_listings_use_locale_and_category_paths() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v2/help_center/fr/categories.json"))
            .respond_with(json_page(
                "categories",
                json!([{"id": 1, "name": "c", "locale": "fr", "url": "u"}]),
                None,
            ))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v2/help_center/categories/1/sections.json"))
            .respond_with(json_page(
                "sections",
                json!([{"id": 2, "name": "s", "category_id": 1, "parent_section_id": null}]),
                None,
            ))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v2/help_center/sections.json"))
            .respond_with(json_page("sections", json!([]), None))
            .mount(&server)
            .await;
        let c = client(&server);
        let cats = c.list_categories(Some("fr")).await.unwrap();
        assert_eq!(cats[0]["name"], "c");
        assert!(cats[0].get("url").is_none());
        let secs = c.list_sections(Some(1), None).await.unwrap();
        assert_eq!(secs[0]["category_id"], 1);
        assert_eq!(c.list_sections(None, None).await.unwrap(), json!([]));
    }

    #[tokio::test]
    async fn article_translations_omit_bodies_and_default_flags() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v2/help_center/articles/9/translations.json"))
            .respond_with(json_page(
                "translations",
                json!([{"id": 1, "locale": "en-us", "title": "t", "draft": true, "body": "<p>x</p>"},
                       {"id": 2, "locale": "fr", "title": "u", "outdated": true}]),
                None,
            ))
            .mount(&server)
            .await;
        let out = client(&server).list_article_translations(9).await.unwrap();
        assert_eq!(out[0]["draft"], true);
        assert_eq!(out[0]["outdated"], false);
        assert_eq!(out[1]["outdated"], true);
        assert!(out[0].get("body").is_none());
    }

    #[tokio::test]
    async fn create_article_posts_html_body_and_returns_article() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v2/help_center/sections/4/articles.json"))
            .respond_with(ResponseTemplate::new(201).set_body_json(json!({"article": {
                "id": 77, "title": "T", "draft": true, "section_id": 4
            }})))
            .expect(1)
            .mount(&server)
            .await;
        let article = serde_json::from_value(json!({
            "title": "T", "body": "**hi**", "locale": "en-us", "draft": true,
            "label_names": ["a"], "user_segment_id": null
        }))
        .unwrap();
        let out = client(&server)
            .create_article(4, article, false)
            .await
            .unwrap();
        assert_eq!(out["id"], 77);
        assert_eq!(out["draft"], true);
        let requests = server.received_requests().await.unwrap();
        let body: Value = serde_json::from_slice(&requests[0].body).unwrap();
        assert!(
            body["article"]["body"]
                .as_str()
                .unwrap()
                .contains("<strong>hi</strong>")
        );
        assert_eq!(body["article"]["draft"], true);
        assert_eq!(body["article"]["label_names"], json!(["a"]));
        assert!(body["article"].get("user_segment_id").is_none());
        assert_eq!(body["notify_subscribers"], false);
    }

    #[tokio::test]
    async fn update_article_splits_translation_and_metadata_puts() {
        let server = MockServer::start().await;
        Mock::given(method("PUT"))
            .and(path("/api/v2/help_center/articles/9/translations/fr.json"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"translation": {}})))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("PUT"))
            .and(path("/api/v2/help_center/articles/9.json"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"article": {}})))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v2/help_center/fr/articles/9.json"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"article": {
                "id": 9, "title": "New", "draft": false
            }})))
            .mount(&server)
            .await;
        let c = client(&server);
        let map = |v: Value| v.as_object().unwrap().clone();
        let out = c
            .update_article(
                9,
                Some("fr"),
                map(json!({"title": "New", "draft": false})),
                map(json!({"promoted": true})),
            )
            .await
            .unwrap();
        assert_eq!(out["title"], "New");
        let requests = server.received_requests().await.unwrap();
        let bodies: Vec<Value> = requests
            .iter()
            .filter(|r| r.method.as_str() == "PUT")
            .map(|r| serde_json::from_slice(&r.body).unwrap())
            .collect();
        assert_eq!(
            bodies[0],
            json!({"translation": {"title": "New", "draft": false}})
        );
        assert_eq!(bodies[1], json!({"article": {"promoted": true}}));
        let err = c
            .update_article(9, None, map(json!({"title": "x"})), Map::new())
            .await
            .unwrap_err();
        assert!(format!("{err:#}").contains("locale is required"), "{err}");
        let err = c
            .update_article(9, Some("fr"), Map::new(), Map::new())
            .await
            .unwrap_err();
        assert!(format!("{err:#}").contains("Nothing to update"), "{err}");
    }

    #[tokio::test]
    async fn update_article_says_when_only_the_translation_was_written() {
        let server = MockServer::start().await;
        Mock::given(method("PUT"))
            .and(path("/api/v2/help_center/articles/9/translations/fr.json"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"translation": {}})))
            .mount(&server)
            .await;
        Mock::given(method("PUT"))
            .and(path("/api/v2/help_center/articles/9.json"))
            .respond_with(ResponseTemplate::new(500).set_body_string("boom"))
            .mount(&server)
            .await;
        let map = |v: Value| v.as_object().unwrap().clone();
        let err = client(&server)
            .update_article(
                9,
                Some("fr"),
                map(json!({"title": "New"})),
                map(json!({"promoted": true})),
            )
            .await
            .unwrap_err();
        let text = format!("{err:#}");
        assert!(
            text.contains("the translation was updated, but updating the article failed"),
            "{text}"
        );
        assert!(text.contains("HTTP 500"), "{text}");
    }

    #[tokio::test]
    async fn search_articles_sends_filters_and_returns_snippets() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v2/help_center/articles/search.json"))
            .and(query_param("section", "5"))
            .and(query_param("label_names", "a,b"))
            .and(query_param("sort_by", "updated_at"))
            .and(query_param("updated_after", "2026-01-01"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "results": [{"id": 1, "title": "t", "snippet": "<em>x</em>", "vote_sum": 3,
                    "promoted": true, "label_names": ["a"], "result_type": "article"}],
                "count": 1, "next_page": null
            })))
            .expect(1)
            .mount(&server)
            .await;
        let c = client(&server);
        let labels = ["a".to_string(), "b".to_string()];
        let search = ArticleSearch {
            section: Some(5),
            label_names: &labels,
            sort_by: Some("updated_at"),
            updated_after: Some("2026-01-01"),
            per_page: 25,
            page: 1,
            ..Default::default()
        };
        let out = c.search_articles(&search).await.unwrap();
        assert!(out["query"].is_null());
        assert_eq!(out["has_more"], false);
        assert!(out.get("next_page").is_none());
        let a = &out["articles"][0];
        assert_eq!(a["snippet"], "<em>x</em>");
        assert_eq!(a["promoted"], true);
        assert_eq!(a["label_names"], json!(["a"]));
        assert_eq!(a["vote_sum"], 3);
        assert!(a.get("result_type").is_none());
    }

    #[tokio::test]
    async fn search_articles_rejects_invalid_searches_without_a_request() {
        let server = MockServer::start().await;
        for bad in [
            ArticleSearch {
                per_page: 25,
                page: 1,
                ..Default::default()
            },
            ArticleSearch {
                query: Some("q"),
                sort_by: Some("title"),
                ..Default::default()
            },
            ArticleSearch {
                query: Some("q"),
                sort_order: Some("up"),
                ..Default::default()
            },
        ] {
            assert!(client(&server).search_articles(&bad).await.is_err());
        }
        assert!(server.received_requests().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn get_article_without_an_article_object_is_an_error() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
            .mount(&server)
            .await;
        let err = client(&server).get_article(1, None).await.unwrap_err();
        assert!(format!("{err:#}").contains("no 'article' object"), "{err}");
    }

    #[tokio::test]
    async fn locales_that_could_change_the_route_are_rejected() {
        let c = offline_client();
        assert!(c.get_article(1, Some("..")).await.is_err());
        assert!(c.list_categories(Some("a/b")).await.is_err());
    }
}
