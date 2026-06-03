use reqwest::Client;
use scraper::{Html, Selector};

use crate::types::Notification;

pub struct NotificationsService {
    client: Client,
}

impl NotificationsService {
    pub fn new(client: Client) -> Self {
        Self { client }
    }

    pub async fn get_notifications(&self, _force_refresh: bool) -> Vec<Notification> {
        let html = match self.client
            .get("http://results.jntuh.ac.in/jsp/home.jsp")
            .header("User-Agent", "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36")
            .timeout(std::time::Duration::from_secs(10))
            .send().await
        {
            Ok(r) if r.status().is_success() => match r.text().await { Ok(t) => t, _ => return vec![] },
            _ => return vec![],
        };

        tokio::task::spawn_blocking(move || {
            let doc = Html::parse_document(&html);
            let mut out = Vec::new();

            let table_sel = Selector::parse("table").unwrap();
            let row_sel = Selector::parse("tr").unwrap();
            let td_sel = Selector::parse("td").unwrap();
            let link_sel = Selector::parse("a").unwrap();

            if let Some(table) = doc.select(&table_sel).nth(2) {
                for row in table.select(&row_sel) {
                    let tds: Vec<_> = row.select(&td_sel).collect();
                    if tds.len() < 2 { continue; }

                    let date = tds[0].text().collect::<String>().trim().to_string();
                    let link = tds[1].select(&link_sel).next()
                        .and_then(|a| a.value().attr("href"))
                        .map(|h| h.to_string());
                    let title = tds[1].text().collect::<String>().trim().to_string();

                    if !title.is_empty() {
                        out.push(Notification { title, link, date: Some(date) });
                    }
                }
            }

            out
        }).await.unwrap_or_default()
    }
}
