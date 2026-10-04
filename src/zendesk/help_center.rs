use anyhow::Result;
use serde_json::{Map, Value, json};

use super::*;

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
            let article = data.get("article").cloned().unwrap_or(json!({}));
            let mut out = pick(
                &article,
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
            Ok(out)
        }
        .await
        .map_err(ctx(format!("Failed to get article {article_id}")))
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
}
