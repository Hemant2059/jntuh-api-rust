use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use futures::stream::{FuturesUnordered, StreamExt};
use reqwest::Client;
use scraper::{Html, Selector};

use crate::extract_code::ExamCodes;
use crate::types::*;

#[derive(Clone)]
struct CodeInfo {
    code: String,
    exam_type: String,
    year: i32,
}

pub struct AllResultService {
    client: Client,
    exam_codes: Arc<ExamCodes>,
    url: String,
}

impl AllResultService {
    pub fn new(client: Client, exam_codes: Arc<ExamCodes>) -> Self {
        Self {
            client,
            exam_codes,
            url: "http://results.jntuh.ac.in/results/resultAction".to_string(),
        }
    }

    fn regulation(roll: &str) -> String {
        let yr = 2000 + roll[..2].parse::<i32>().unwrap_or(0);
        let deg = Self::degree(roll);
        if yr >= 2023 || (yr == 2022 && roll.as_bytes().get(4) != Some(&b'5')) {
            "R22".into()
        } else if deg == "btech" {
            "R18".into()
        } else {
            "R17".into()
        }
    }

    fn degree(roll: &str) -> String {
        if roll.as_bytes().get(5) == Some(&b'A') {
            "btech".into()
        } else {
            "bpharmacy".into()
        }
    }

    fn admission_year(roll: &str) -> i32 {
        2000 + roll[..2].parse::<i32>().unwrap_or(0)
    }

    /// Max plausible semester index (0-7) for a student today.
    /// Regular: sem N starts ~ (admission_year + N/2). Lateral joins in 2nd year.
    /// Adds 1 sem buffer so just-declared results are included, future sems skipped.
    fn max_sem_index(roll: &str) -> usize {
        use chrono::Datelike;
        let now = chrono::Local::now();
        let cur_year = now.year();
        let cur_month = now.month(); // 1-12; odd sem results ~Jan, even ~Jul
        let adm = Self::admission_year(roll);
        let lateral = roll.as_bytes().get(4) == Some(&b'5');
        // Academic year offset: Jul-Jun. Before July, spring sem still belongs to prev cycle.
        let years_done = (cur_year - adm) as usize;
        // Each academic year = 2 sems. +1 buffer for just-published results.
        let mut max = years_done * 2 + if cur_month >= 7 { 1 } else { 0 } + 1;
        if lateral {
            // Lateral skips 1-1,1-2 but timeline shifts by ~1 year of coursework
            max = max.saturating_sub(0);
        }
        max.min(7)
    }

    pub async fn get_all_results(&self, roll: &str) -> AllResultResponse {
        let admission_year = Self::admission_year(roll);
        let degree = Self::degree(roll);
        let regulation = Self::regulation(roll);
        let all_semesters = ["1-1", "1-2", "2-1", "2-2", "3-1", "3-2", "4-1", "4-2"];
        let lateral = roll.as_bytes().get(4) == Some(&b'5');
        let max_idx = Self::max_sem_index(roll);

        let mut details = StudentDetails {
            name: None,
            roll_no: roll.to_string(),
            father_name: None,
            college_code: None,
            regulation: Some(regulation.clone()),
            error: None,
        };

        let mut all_results: BTreeMap<String, Vec<DetailedExamEntry>> = BTreeMap::new();
        let mut sem_tasks = Vec::new();

        for (idx, sem) in all_semesters.iter().enumerate() {
            if idx > max_idx {
                continue; // skip future sems — saves ~10-20 wasted JNTUH hits
            }
            if lateral && (*sem == "1-1" || *sem == "1-2") {
                continue;
            }
            let codes = self.exam_codes.get_semester_codes(&degree, &regulation, sem);
            if codes.is_empty() {
                continue;
            }

            let mut sorted_years: Vec<i32> = codes
                .keys()
                .filter_map(|y| y.parse::<i32>().ok())
                .filter(|y| *y >= admission_year)
                .collect();
            sorted_years.sort_unstable();

            let mut to_fetch = Vec::new();
            let mut seen = HashSet::new();
            for y in &sorted_years {
                if let Some(types) = codes.get(&y.to_string()) {
                    for et in &["regular", "supply"] {
                        if let Some(c) = types.get(*et) {
                            for code in c {
                                if seen.insert(code.clone()) {
                                    to_fetch.push(CodeInfo {
                                        code: code.clone(),
                                        exam_type: et.to_string(),
                                        year: *y,
                                    });
                                }
                            }
                        }
                    }
                }
            }
            if to_fetch.is_empty() {
                continue;
            }

            let sn = sem.to_string();
            let c = self.client.clone();
            let u = self.url.clone();
            let d = degree.clone();
            let r = roll.to_string();

            sem_tasks.push(tokio::spawn(async move {
                let entries = Self::fetch_sem(&c, &u, &to_fetch, &d, &r).await;
                (sn, entries)
            }));
        }

        for task in sem_tasks {
            let (sem, mut entries) = match task.await {
                Ok(r) => r,
                Err(_) => continue,
            };
            if entries.is_empty() {
                continue;
            }

            // Same miscategorization guard as sem_result: drop attempts whose
            // subjects positively belong to another semester.
            if entries.len() > 1
                && entries
                    .iter()
                    .any(|e| attempt_matches_sem(&e.result, &sem, &degree, &regulation))
            {
                entries.retain(|e| attempt_matches_sem(&e.result, &sem, &degree, &regulation));
            }
            if entries.is_empty() {
                continue;
            }

            entries.sort_by_key(|e| {
                (
                    e.year,
                    match e.exam_type.as_str() {
                        "regular" => 0,
                        "supply" => 1,
                        _ => 2,
                    },
                    e.exam_code.clone(),
                )
            });

            // No relabeling: preserve true regular/supply from exam_codes.json.
            if details.name.is_none() {
                if let Some(first) = entries.first() {
                    if first.details.name.is_some() {
                        details.name.clone_from(&first.details.name);
                        details.father_name.clone_from(&first.details.father_name);
                        details.college_code.clone_from(&first.details.college_code);
                    }
                }
            }

            all_results.insert(sem, entries);
        }

        AllResultResponse {
            details,
            results: all_results,
        }
    }

    async fn fetch_sem(
        client: &Client,
        base: &str,
        infos: &[CodeInfo],
        degree: &str,
        roll: &str,
    ) -> Vec<DetailedExamEntry> {
        let sem = Arc::new(tokio::sync::Semaphore::new(5));
        let mut unordered = FuturesUnordered::new();

        for info in infos {
            let sem = sem.clone();
            let c = client.clone();
            let u = base.to_string();
            let d = degree.to_string();
            let r = roll.to_string();
            let code = info.code.clone();
            let et = info.exam_type.clone();
            let yr = info.year;
            unordered.push(tokio::spawn(async move {
                let _p = sem.acquire_owned().await.ok()?;
                Self::fetch_one(&c, &u, &code, &et, yr, &d, &r).await.into()
            }));
        }

        let mut out = Vec::new();
        while let Some(res) = unordered.next().await {
            match res {
                Ok(Some(mut v)) => out.append(&mut v),
                Ok(None) => {}
                Err(e) => tracing::debug!("allresult task error: {}", e),
            }
        }
        out
    }

    async fn fetch_one(
        client: &Client,
        base: &str,
        code: &str,
        exam_type: &str,
        year: i32,
        degree: &str,
        roll: &str,
    ) -> Vec<DetailedExamEntry> {
        const BACKOFF: &[u64] = &[300, 1000, 2000];
        let mut data_opt = None;
        let mut url_used = String::new();

        for (attempt, backoff) in BACKOFF.iter().enumerate() {
            let url = format!(
                "{}?examCode={}&etype=r16&result=null&grad=null&type=intgrade&degree={}&htno={}",
                base, code, degree, roll
            );
            url_used = url.clone();

            let resp = match client
                .get(&url)
                .header(
                    "User-Agent",
                    "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36",
                )
                .timeout(Duration::from_secs(15))
                .send()
                .await
            {
                Ok(r) if r.status().is_success() => r,
                _ => {
                    if attempt + 1 < BACKOFF.len() {
                        tokio::time::sleep(Duration::from_millis(*backoff)).await;
                        continue;
                    }
                    return vec![];
                }
            };

            let bytes = match resp.bytes().await {
                Ok(b) if b.len() > 100 => b,
                _ => {
                    if attempt + 1 < BACKOFF.len() {
                        tokio::time::sleep(Duration::from_millis(*backoff)).await;
                        continue;
                    }
                    return vec![];
                }
            };

            let html = String::from_utf8_lossy(&bytes);
            match Self::parse_html(&html) {
                Some(d) => {
                    data_opt = Some(d);
                    break;
                }
                None => return vec![],
            }
        }

        let data = match data_opt {
            Some(d) => d,
            None => return vec![],
        };

        let mut results = vec![DetailedExamEntry {
            exam_code: code.to_string(),
            exam_type: exam_type.to_string(),
            year,
            result_url: url_used,
            result: data.result.clone(),
            details: data.details.clone(),
        }];

        // RCRV best-effort, kept only when it differs
        let rurl = format!(
            "{}?examCode={}&etype=r16&result=gradercrv&grad=null&type=intgrade&degree={}&htno={}",
            base, code, degree, roll
        );
        if let Ok(rr) = client
            .get(&rurl)
            .header(
                "User-Agent",
                "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36",
            )
            .timeout(Duration::from_secs(10))
            .send()
            .await
        {
            if rr.status().is_success() {
                if let Ok(b) = rr.bytes().await {
                    if b.len() > 100 {
                        let h = String::from_utf8_lossy(&b);
                        if let Some(rd) = Self::parse_html(&h) {
                            let differs = rd.result.iter().any(|(k, v)| {
                                data.result
                                    .get(k)
                                    .is_none_or(|cur| cur.grade != v.grade || v.rcrv)
                            });
                            if differs && !rd.result.is_empty() {
                                results.push(DetailedExamEntry {
                                    exam_code: code.to_string(),
                                    exam_type: "rcrv".into(),
                                    year,
                                    result_url: rurl,
                                    result: rd.result,
                                    details: rd.details,
                                });
                            }
                        }
                    }
                }
            }
        }

        results
    }

    fn parse_html(html: &str) -> Option<SemesterResult> {
        let doc = Html::parse_document(html);
        if doc
            .select(&Selector::parse("form[id='myForm']").unwrap())
            .next()
            .is_some()
        {
            return None;
        }

        let tables: Vec<_> = doc.select(&Selector::parse("table").unwrap()).collect();
        if tables.len() < 2 {
            return None;
        }

        let tr = Selector::parse("tr").unwrap();
        let td = Selector::parse("td").unwrap();
        let d_rows: Vec<_> = tables[0].select(&tr).collect();
        let r_rows: Vec<_> = tables[1].select(&tr).collect();
        if d_rows.len() < 2 || r_rows.is_empty() {
            return None;
        }

        let d0: Vec<_> = d_rows[0].select(&td).collect();
        let d1: Vec<_> = d_rows[1].select(&td).collect();

        let details = StudentDetails {
            name: d0.get(3).map(|c| c.text().collect::<String>().trim().to_string()),
            roll_no: d0
                .get(1)
                .map(|c| c.text().collect::<String>().trim().to_string())
                .unwrap_or_default(),
            father_name: d1.get(1).map(|c| c.text().collect::<String>().trim().to_string()),
            college_code: d1.get(3).map(|c| c.text().collect::<String>().trim().to_string()),
            regulation: None,
            error: None,
        };

        let mut result = HashMap::new();
        for row in r_rows.iter().skip(1) {
            let cells: Vec<_> = row.select(&td).collect();
            if cells.len() < 7 {
                continue;
            }
            let sub = cells[0].text().collect::<String>().trim().to_string();
            if sub.is_empty() || sub == "SUBJECT CODE" {
                continue;
            }
            result.insert(
                sub,
                SubjectResult {
                    name: cells[1].text().collect::<String>().trim().to_string(),
                    internal: cells[2].text().collect::<String>().trim().to_string(),
                    external: cells[3].text().collect::<String>().trim().to_string(),
                    total: cells[4].text().collect::<String>().trim().to_string(),
                    grade: cells[5].text().collect::<String>().trim().to_string(),
                    credits: cells[6].text().collect::<String>().trim().to_string(),
                    rcrv: cells
                        .last()
                        .is_some_and(|c| c.text().collect::<String>().contains("Change in Grade")),
                },
            );
        }

        if result.is_empty() {
            return None;
        }

        Some(SemesterResult { details, result })
    }
}
