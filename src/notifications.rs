use std::time::{SystemTime, UNIX_EPOCH};
use std::sync::Mutex;
use reqwest::Client;
use scraper::{Html, Selector};
use regex::Regex;

use crate::types::Notification;

pub struct NotificationsService {
    client: Client,
    cache: Mutex<Option<CachedNotifications>>,
}

struct CachedNotifications {
    data: Vec<Notification>,
    fetched_at: u64,
}

impl NotificationsService {
    pub fn new(client: Client) -> Self {
        Self {
            client,
            cache: Mutex::new(None),
        }
    }

    pub async fn get_notifications(&self, force_refresh: bool) -> Vec<Notification> {
        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();

        if !force_refresh {
            if let Ok(cache) = self.cache.lock() {
                if let Some(c) = cache.as_ref() {
                    if now < c.fetched_at + 3600 {
                        return c.data.clone();
                    }
                }
            }
        }

        let html = match self.client
            .get("http://results.jntuh.ac.in/jsp/home.jsp")
            .header("User-Agent", "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36")
            .timeout(std::time::Duration::from_secs(10))
            .send().await
        {
            Ok(r) if r.status().is_success() => match r.text().await { Ok(t) => t, _ => return self.cached_or_empty() },
            _ => return self.cached_or_empty(),
        };

        let notifications = tokio::task::spawn_blocking(move || parse_notifications(&html))
            .await
            .unwrap_or_default();

        if let Ok(mut cache) = self.cache.lock() {
            *cache = Some(CachedNotifications {
                data: notifications.clone(),
                fetched_at: now,
            });
        }

        notifications
    }

    fn cached_or_empty(&self) -> Vec<Notification> {
        if let Ok(cache) = self.cache.lock() {
            if let Some(c) = cache.as_ref() {
                return c.data.clone();
            }
        }
        vec![]
    }
}

fn roman_to_arabic(s: &str) -> &str {
    match s.to_uppercase().as_str() {
        "I" => "1", "II" => "2", "III" => "3", "IV" => "4",
        _ => s,
    }
}

fn parse_notifications(html: &str) -> Vec<Notification> {
    let doc = Html::parse_document(html);
    let mut out = Vec::new();

    let table_sel = Selector::parse("table").unwrap();
    let row_sel = Selector::parse("tr").unwrap();
    let td_sel = Selector::parse("td").unwrap();
    let link_sel = Selector::parse("a").unwrap();

    let re_slug = Regex::new(r"[^a-z0-9\s-]").unwrap();
    let re_roman = Regex::new(r"(?i)\b(I|II|III|IV)\s*-\s*(I|II)\s*(Year|Semester)?").unwrap();
    let re_regulation = Regex::new(r"\(?(R\d{2,3})\)?").unwrap();
    let re_exam_date = Regex::new(r"([A-Za-z]+[- ]\d{4})").unwrap();

    // Iterate ALL tables (matching Python behavior)
    for table in doc.select(&table_sel) {
        for row in table.select(&row_sel) {
            let tds: Vec<_> = row.select(&td_sel).collect();
            if tds.len() < 2 { continue; }

            // Python looks for link in FIRST td, date in SECOND td
            let link = tds[0].select(&link_sel).next()
                .and_then(|a| a.value().attr("href"))
                .map(|h| {
                    if h.starts_with("http") { h.to_string() }
                    else { format!("http://results.jntuh.ac.in{}", h) }
                })
                .unwrap_or_default();
            let title = tds[0].text().collect::<String>().trim().to_string();
            let date = tds[1].text().collect::<String>().trim().to_string();

            // Skip if no link found or title is empty
            if link.is_empty() || title.is_empty() { continue; }

            let upper = title.to_uppercase();

            let degree = if upper.contains("B.TECH") || upper.contains("B.TECH") { "btech" }
                else if upper.contains("B.PHARMACY") || upper.contains("B.PHARM") { "bpharmacy" }
                else if upper.contains("M.TECH") || upper.contains("M.TECH") { "mtech" }
                else if upper.contains("MBA") { "mba" }
                else if upper.contains("MCA") { "mca" }
                else { "other" };

            let semester = re_roman.captures(&title)
                .map(|c| {
                    let y = c.get(1).map(|m| roman_to_arabic(m.as_str())).unwrap_or("");
                    let s = c.get(2).map(|m| roman_to_arabic(m.as_str())).unwrap_or("");
                    format!("{}-{}", y, s)
                })
                .unwrap_or_else(|| "N/A".into());

            let regulation = re_regulation.captures(&title)
                .map(|c| c.get(1).map(|m| m.as_str().to_string()).unwrap_or_else(|| "N/A".into()))
                .unwrap_or_else(|| "N/A".into());

            let exam_date = re_exam_date.captures(&title)
                .map(|c| c.get(1).map(|m| m.as_str().replace(' ', "-")).unwrap_or_else(|| "N/A".into()))
                .unwrap_or_else(|| "N/A".into());

            let exam_type = if upper.contains("SUPPLEMENTARY") || upper.contains("SUPPLY") { "supplementary" }
                else { "regular" };

            let is_rcrv = upper.contains("RC/RV") || upper.contains("REVALUATION") || upper.contains("RC RV");

            let category = if is_rcrv { "rcrv" } else { "result" };

            let short_title = title.replacen("Results for ", "", 1).trim().to_string();

            let slug = {
                let lower = title.to_lowercase();
                let cleaned = re_slug.replace_all(&lower, "");
                let slugged = cleaned.trim().replace(' ', "-");
                slugged.trim_matches('-').to_string()
            };

            out.push(Notification {
                title,
                short_title,
                slug,
                url: link,
                exam_date,
                publish_date: date,
                degree: degree.into(),
                semester,
                regulation,
                exam_type: exam_type.into(),
                category: category.into(),
                is_rcrv,
            });
        }
    }

    out
}
