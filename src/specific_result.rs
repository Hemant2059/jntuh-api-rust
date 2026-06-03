use std::collections::HashMap;
use std::time::Duration;

use reqwest::Client;
use scraper::{Html, Selector};

use crate::types::*;

pub struct SpecificResultService {
    client: Client,
    url: String,
}

impl SpecificResultService {
    pub fn new(client: Client) -> Self {
        Self {
            client,
            url: "http://results.jntuh.ac.in/results/resultAction".to_string(),
        }
    }

    pub async fn get_result(
        &self, exam_code: &str, etype: &str, result: &str, grad: &str,
        r#type: &str, degree: &str, htno: &str,
    ) -> serde_json::Value {
        let url = format!(
            "{}?examCode={}&etype={}&result={}&grad={}&type={}&degree={}&htno={}",
            self.url, exam_code, etype, result, grad, r#type, degree, htno
        );

        match self.client.get(&url)
            .header("User-Agent", "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36")
            .timeout(Duration::from_secs(10))
            .send().await
        {
            Ok(resp) if resp.status().is_success() => {
                match resp.bytes().await {
                    Ok(b) if b.len() > 100 => {
                        let html = String::from_utf8_lossy(&b);
                        match Self::parse_html(&html) {
                            Some(data) => serde_json::to_value(data).unwrap_or(serde_json::json!({"error": "parse error"})),
                            None => serde_json::json!({"error": "not found/invalid hall ticket for this result"}),
                        }
                    }
                    _ => serde_json::json!({"error": "not found/invalid hall ticket for this result"}),
                }
            }
            Ok(resp) => serde_json::json!({"error": format!("JNTUH server returned {}", resp.status())}),
            Err(e) => serde_json::json!({"error": format!("Request failed: {}", e)}),
        }
    }

    fn parse_html(html: &str) -> Option<SemesterResult> {
        let doc = Html::parse_document(html);
        if doc.select(&Selector::parse("form[id='myForm']").unwrap()).next().is_some() {
            return None;
        }

        let tables: Vec<_> = doc.select(&Selector::parse("table").unwrap()).collect();
        if tables.len() < 2 { return None; }

        let tr = Selector::parse("tr").unwrap();
        let td = Selector::parse("td").unwrap();

        let d_rows: Vec<_> = tables[0].select(&tr).collect();
        if d_rows.is_empty() { return None; }
        let r_rows: Vec<_> = tables[1].select(&tr).collect();

        let d0: Vec<_> = d_rows[0].select(&td).collect();
        let d1 = d_rows.get(1).map(|r| r.select(&td).collect::<Vec<_>>());

        let details = StudentDetails {
            name: d0.get(3).map(|c| c.text().collect::<String>().trim().to_string()),
            roll_no: d0.get(1).map(|c| c.text().collect::<String>().trim().to_string()).unwrap_or_default(),
            father_name: d1.as_ref().and_then(|c| c.get(1).map(|cell| cell.text().collect::<String>().trim().to_string())),
            college_code: d1.as_ref().and_then(|c| c.get(3).map(|cell| cell.text().collect::<String>().trim().to_string())),
            regulation: None, error: None,
        };

        let mut result = HashMap::new();
        for row in r_rows.iter().skip(1) {
            let cells: Vec<_> = row.select(&td).collect();
            if cells.len() < 7 { continue; }
            let sub = cells[0].text().collect::<String>().trim().to_string();
            if sub.is_empty() || sub == "SUBJECT CODE" { continue; }
            result.insert(sub, SubjectResult {
                name: cells[1].text().collect::<String>().trim().to_string(),
                internal: cells[2].text().collect::<String>().trim().to_string(),
                external: cells[3].text().collect::<String>().trim().to_string(),
                total: cells[4].text().collect::<String>().trim().to_string(),
                grade: cells[5].text().collect::<String>().trim().to_string(),
                credits: cells[6].text().collect::<String>().trim().to_string(),
                rcrv: cells.len() > 7 && cells.last().map_or(false, |c| c.text().collect::<String>().contains("Change in Grade")),
            });
        }

        Some(SemesterResult { details, result })
    }
}
