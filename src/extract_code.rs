use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use parking_lot::RwLock;
use reqwest::Client;
use scraper::{Html, Selector};
use tokio::sync::Mutex;

use crate::types::ExamCodesMap;

#[derive(Debug, Clone)]
pub struct ExamCodes {
    inner: Arc<RwLock<ExamCodesMap>>,
    last_refresh: Arc<Mutex<Instant>>,
    disk_path: String,
}

impl ExamCodes {
    pub fn new(disk_path: &str) -> Self {
        Self {
            inner: Arc::new(RwLock::new(HashMap::new())),
            last_refresh: Arc::new(Mutex::new(Instant::now())),
            disk_path: disk_path.to_string(),
        }
    }

    pub async fn initialize(&self, client: &Client) {
        let disk_codes = Self::load_from_disk(&self.disk_path);
        let disk_count: usize = disk_codes.values()
            .flat_map(|r| r.values())
            .flat_map(|s| s.values())
            .flat_map(|y| y.values())
            .flat_map(|t| t.values())
            .map(|c| c.len())
            .sum();

        let scraped_codes = Self::fetch_from_jntuh(client).await;
        let scraped_count: usize = scraped_codes.values()
            .flat_map(|r| r.values())
            .flat_map(|s| s.values())
            .flat_map(|y| y.values())
            .flat_map(|t| t.values())
            .map(|c| c.len())
            .sum();

        let merged = Self::merge_codes(scraped_codes, disk_codes);
        let total_count: usize = merged.values()
            .flat_map(|r| r.values())
            .flat_map(|s| s.values())
            .flat_map(|y| y.values())
            .flat_map(|t| t.values())
            .map(|c| c.len())
            .sum();

        {
            let mut cache = self.inner.write();
            *cache = merged;
        }
        *self.last_refresh.lock().await = Instant::now();

        tracing::info!("Loaded {} exam codes (disk: {}, jntuh: {})", total_count, disk_count, scraped_count);
    }

    fn load_from_disk(path: &str) -> ExamCodesMap {
        if !Path::new(path).exists() {
            tracing::warn!("exam_codes.json not found at {}", path);
            return HashMap::new();
        }
        let content = match std::fs::read_to_string(path) {
            Ok(c) => c,
            Err(e) => {
                tracing::error!("Failed to read {}: {}", path, e);
                return HashMap::new();
            }
        };

        let raw: serde_json::Value = match serde_json::from_str(&content) {
            Ok(v) => v,
            Err(e) => {
                tracing::error!("Invalid JSON in {}: {}", path, e);
                return HashMap::new();
            }
        };

        let data = raw.get("data").unwrap_or(&raw);

        match serde_json::from_value(data.clone()) {
            Ok(parsed) => parsed,
            Err(e) => {
                tracing::error!("Failed to parse exam codes structure: {}", e);
                HashMap::new()
            }
        }
    }

    pub async fn fetch_from_jntuh(client: &Client) -> ExamCodesMap {
        let html = match client
            .get("http://results.jntuh.ac.in/jsp/home.jsp")
            .header("User-Agent", "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36")
            .timeout(std::time::Duration::from_secs(10))
            .send()
            .await
        {
            Ok(resp) if resp.status().is_success() => match resp.text().await {
                Ok(t) => t,
                Err(_) => return HashMap::new(),
            },
            Ok(resp) => {
                tracing::warn!("JNTUH home page returned {}", resp.status());
                return HashMap::new();
            }
            Err(e) => {
                tracing::warn!("Failed to fetch JNTUH home page: {}", e);
                return HashMap::new();
            }
        };

        tokio::task::spawn_blocking(move || Self::process_jntuh_html(&html))
            .await
            .unwrap_or_default()
    }

    fn process_jntuh_html(html: &str) -> ExamCodesMap {
        let document = Html::parse_document(html);
        let mut exam_codes: ExamCodesMap = HashMap::new();

        let table_sel = Selector::parse("table").unwrap();
        let row_sel = Selector::parse("tr").unwrap();
        let link_sel = Selector::parse("a").unwrap();
        let td_sel = Selector::parse("td").unwrap();

        let tables: Vec<_> = document.select(&table_sel).collect();
        let degree_keys = ["btech", "bpharmacy"];

        for (table_idx, degree) in degree_keys.iter().enumerate() {
            if table_idx >= tables.len() {
                continue;
            }

            for row in tables[table_idx].select(&row_sel) {
                let link = match row.select(&link_sel).next() {
                    Some(l) => l,
                    None => continue,
                };

                let href = match link.value().attr("href") {
                    Some(h) => h,
                    None => continue,
                };

                let exam_code = match extract_exam_code(href) {
                    Some(c) => c,
                    None => continue,
                };

                let result_text = row.text().collect::<String>();

                let tds: Vec<_> = row.select(&td_sel).collect();
                let year = tds.get(1)
                    .and_then(|td| extract_year(&td.text().collect::<String>()));

                let year = match year {
                    Some(y) => y,
                    None => continue,
                };

                for regulation in &["R18", "R22"] {
                    if !result_text.contains(regulation) {
                        continue;
                    }

                    let semester = match categorize_exam_code(&result_text) {
                        Some(s) => s,
                        None => continue,
                    };

                    let exam_type = get_exam_type(&result_text);

                    exam_codes
                        .entry(degree.to_string())
                        .or_default()
                        .entry(regulation.to_string())
                        .or_default()
                        .entry(semester)
                        .or_default()
                        .entry(year.clone())
                        .or_insert_with(|| {
                            let mut m = HashMap::new();
                            m.insert("regular".to_string(), Vec::new());
                            m.insert("supply".to_string(), Vec::new());
                            m
                        })
                        .entry(exam_type)
                        .or_default()
                        .push(exam_code.clone());
                }
            }
        }

        exam_codes
    }

    pub async fn refresh(&self, client: &Client) {
        let disk_codes = Self::load_from_disk(&self.disk_path);
        let scraped_codes = Self::fetch_from_jntuh(client).await;
        let merged = Self::merge_codes(scraped_codes, disk_codes);

        let total_count: usize = merged.values()
            .flat_map(|r| r.values())
            .flat_map(|s| s.values())
            .flat_map(|y| y.values())
            .flat_map(|t| t.values())
            .map(|c| c.len())
            .sum();

        {
            let mut cache = self.inner.write();
            *cache = merged;
        }
        *self.last_refresh.lock().await = Instant::now();

        tracing::info!("Refreshed exam codes: {} total", total_count);
    }

    pub fn get_semester_codes(&self, degree: &str, regulation: &str, semester: &str)
        -> HashMap<String, HashMap<String, Vec<String>>>
    {
        self.inner.read()
            .get(degree)
            .and_then(|r| r.get(regulation))
            .and_then(|s| s.get(semester))
            .cloned()
            .unwrap_or_default()
    }

    fn merge_codes(primary: ExamCodesMap, fallback: ExamCodesMap) -> ExamCodesMap {
        let mut result = fallback;
        for (deg, regs) in primary {
            for (reg, sems) in regs {
                for (sem, years) in sems {
                    for (yr, types) in years {
                        for (t, codes) in types {
                            let entry = result
                                .entry(deg.clone()).or_default()
                                .entry(reg.clone()).or_default()
                                .entry(sem.clone()).or_default()
                                .entry(yr.clone()).or_default()
                                .entry(t).or_default();
                            for code in codes {
                                if !entry.contains(&code) {
                                    entry.push(code);
                                }
                            }
                            entry.sort();
                        }
                    }
                }
            }
        }
        result
    }

    pub async fn total_codes(&self) -> usize {
        self.inner.read().values()
            .flat_map(|r| r.values())
            .flat_map(|s| s.values())
            .flat_map(|y| y.values())
            .flat_map(|t| t.values())
            .map(|c| c.len())
            .sum()
    }
}

fn extract_exam_code(link: &str) -> Option<String> {
    let start = link.find("examCode=")?;
    let after = &link[start + 9..];
    let end = after.find(|c: char| !c.is_ascii_digit()).unwrap_or(after.len());
    if end > 0 { Some(after[..end].to_string()) } else { None }
}

fn categorize_exam_code(text: &str) -> Option<String> {
    const CATEGORIES: &[(&str, &str)] = &[
        (" I Year I ", "1-1"), (" I Year II ", "1-2"),
        (" II Year I ", "2-1"), (" II Year II ", "2-2"),
        (" III Year I ", "3-1"), (" III Year II ", "3-2"),
        (" IV Year I ", "4-1"), (" IV Year II ", "4-2"),
    ];
    CATEGORIES.iter()
        .find(|(key, _)| text.contains(key))
        .map(|(_, val)| val.to_string())
}

fn get_exam_type(text: &str) -> String {
    let lower = text.to_lowercase();
    if lower.contains("supplementary") || lower.contains("supply") || lower.contains("sup") {
        "supply".to_string()
    } else {
        "regular".to_string()
    }
}

fn extract_year(text: &str) -> Option<String> {
    text.as_bytes().windows(4)
        .find(|w| w.iter().all(|b| b.is_ascii_digit()))
        .map(|w| std::str::from_utf8(w).unwrap().to_string())
}
