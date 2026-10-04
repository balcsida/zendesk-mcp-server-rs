use anyhow::{Result, bail};
use serde_json::{Map, Value, json};

use super::*;

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
    pub async fn get_all_articles(&self) -> Result<Value> {
        async {
            let mut kb = Map::new();
            for section in self
                .get_paged("help_center/sections.json", "sections")
                .await?
            {
                let id = &section["id"];
                // A section's articles live under its own locale; the locale-less path only
                // serves the default one, so non-English help centers came back empty
                // (upstream issue #10).
                let path = match section.get("locale").and_then(Value::as_str) {
                    Some(locale) => {
                        format!("help_center/{locale}/sections/{id}/articles.json")
                    }
                    None => format!("help_center/sections/{id}/articles.json"),
                };
                let articles = self.get_paged(&path, "articles").await?;
                let articles: Vec<Value> = articles
                    .iter()
                    .map(|a| {
                        let mut out = pick(a, &["id", "title", "body", "updated_at"], &[]);
                        out["url"] = a["html_url"].clone();
                        out
                    })
                    .collect();
                let name = section["name"].as_str().unwrap_or_default().to_string();
                kb.insert(
                    name,
                    json!({
                        "section_id": section["id"],
                        "description": section["description"],
                        "articles": articles,
                    }),
                );
            }
            Ok(Value::Object(kb))
        }
        .await
        .map_err(ctx("Failed to fetch knowledge base"))
    }

    pub async fn search_articles(
        &self,
        query: &str,
        locale: Option<&str>,
        per_page: u64,
        page: u64,
    ) -> Result<Value> {
        async {
            let per_page = per_page.min(100);
            let mut params: Vec<(&str, &(dyn Display + Sync))> =
                vec![("query", &query), ("per_page", &per_page), ("page", &page)];
            let locale = locale.filter(|l| !l.is_empty());
            if let Some(locale) = &locale {
                params.push(("locale", locale));
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
                ],
                &[],
            );
            for (article, raw) in articles
                .as_array_mut()
                .into_iter()
                .flatten()
                .zip(data["results"].as_array().into_iter().flatten())
            {
                article["draft"] = raw.get("draft").cloned().unwrap_or(json!(false));
            }
            let count = articles.as_array().map_or(0, Vec::len);
            Ok(json!({
                "articles": articles,
                "query": query,
                "page": page,
                "per_page": per_page,
                "count": count,
                "total_count": data.get("count").cloned().unwrap_or(json!(count)),
                "next_page": data["next_page"],
                "previous_page": data["previous_page"],
            }))
        }
        .await
        .map_err(ctx("Failed to search articles"))
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
            let per_page = per_page.min(100);
            let section = section_id
                .map(|id| format!("/sections/{id}"))
                .unwrap_or_default();
            let path = format!("{}{section}/articles.json", help_center_path(locale));
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
            Ok(json!({
                "articles": articles,
                "page": page,
                "per_page": per_page,
                "count": count,
                "total_count": data.get("count").cloned().unwrap_or(json!(count)),
                "has_more": !data["next_page"].is_null(),
            }))
        }
        .await
        .map_err(ctx("Failed to list articles"))
    }

    pub async fn get_article(&self, article_id: u64, locale: Option<&str>) -> Result<Value> {
        async {
            let path = format!("{}/articles/{article_id}.json", help_center_path(locale));
            let data = self.api_get(&path, &[]).await?;
            Ok(article_detail(data.get("article").unwrap_or(&Value::Null)))
        }
        .await
        .map_err(ctx(format!("Failed to get article {article_id}")))
    }

    pub async fn list_categories(&self, locale: Option<&str>) -> Result<Value> {
        async {
            let path = format!("{}/categories.json", help_center_path(locale));
            let categories = self.get_paged(&path, "categories").await?;
            Ok(pick_all(
                &json!({ "categories": categories }),
                "categories",
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
        .map_err(ctx("Failed to list categories"))
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
            let path = format!("{}{category}/sections.json", help_center_path(locale));
            let sections = self.get_paged(&path, "sections").await?;
            Ok(pick_all(
                &json!({ "sections": sections }),
                "sections",
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
        .map_err(ctx("Failed to list sections"))
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
            let mut out = pick_all(
                &json!({ "translations": translations }),
                "translations",
                &["id", "locale", "title", "html_url", "updated_at"],
                &[],
            );
            for (t, raw) in out.as_array_mut().into_iter().flatten().zip(&translations) {
                for key in ["draft", "outdated"] {
                    t[key] = raw.get(key).cloned().unwrap_or(json!(false));
                }
            }
            Ok(out)
        }
        .await
        .map_err(ctx(format!(
            "Failed to list translations of article {article_id}"
        )))
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
            Ok(article_detail(object(&data, "article")?))
        }
        .await
        .map_err(ctx(format!(
            "Failed to create article in section {section_id}"
        )))
    }

    /// Updates one locale's `translation` (title, body, draft; needs `locale`) and/or the
    /// article's own `article` fields, then returns the refreshed article.
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
            if let Some(locale) = locale
                && !translation.is_empty()
            {
                let path = format!(
                    "help_center/articles/{article_id}/translations/{}.json",
                    help_center_path(Some(locale)).trim_start_matches("help_center/")
                );
                self.api_put(&path, &json!({ "translation": translation }))
                    .await?;
            }
            if !article.is_empty() {
                self.api_put(
                    &format!("help_center/articles/{article_id}.json"),
                    &json!({ "article": article }),
                )
                .await?;
            }
            self.get_article(article_id, locale).await
        }
        .await
        .map_err(ctx(format!("Failed to update article {article_id}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::zendesk::test_support::*;
    use wiremock::matchers::{method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

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
        assert!(err.to_string().contains("locale is required"), "{err}");
        let err = c
            .update_article(9, Some("fr"), Map::new(), Map::new())
            .await
            .unwrap_err();
        assert!(err.to_string().contains("Nothing to update"), "{err}");
    }
}
