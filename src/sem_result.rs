use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::stream::{FuturesUnordered, StreamExt};
use reqwest::Client;
use scraper::{Html, Selector};
use tokio::sync::Mutex;

use crate::extract_code::ExamCodes;
use crate::types::*;

#[derive(Clone)]
struct ExamCodeInfo {
    code: String,
    exam_type: String,
    year: i32,
}

pub struct SemResultService {
    client: Client,
    exam_codes: Arc<ExamCodes>,
    url: String,
    cache: Arc<Mutex<HashMap<(String, String), (CombinedSemesterResult, Instant)>>>,
    cache_ttl: Duration,
}

impl SemResultService {
    pub fn new(client: Client, exam_codes: Arc<ExamCodes>) -> Self {
        Self {
            client,
            exam_codes,
            url: "http://results.jntuh.ac.in/results/resultAction".to_string(),
            cache: Arc::new(Mutex::new(HashMap::new())),
            cache_ttl: Duration::from_secs(3600),
        }
    }

    fn regulation(roll_number: &str) -> String {
        let yr = 2000 + roll_number[..2].parse::<i32>().unwrap_or(0);
        let deg = Self::degree(roll_number);
        if yr >= 2023 || (yr == 2022 && roll_number.as_bytes().get(4) != Some(&b'5')) {
            "R22".into()
        } else if deg == "btech" {
            "R18".into()
        } else {
            "R17".into()
        }
    }

    fn degree(roll_number: &str) -> String {
        if roll_number.as_bytes().get(5) == Some(&b'A') {
            "btech".into()
        } else {
            "bpharmacy".into()
        }
    }

    fn admission_year(roll_number: &str) -> i32 {
        2000 + roll_number[..2].parse::<i32>().unwrap_or(0)
    }

    pub fn is_lateral(roll: &str) -> bool {
        roll.as_bytes().get(4) == Some(&b'5')
    }

    fn error_result(roll_number: &str, regulation: &str, error: &str) -> CombinedSemesterResult {
        CombinedSemesterResult {
            details: StudentDetails {
                name: None,
                roll_no: roll_number.to_string(),
                father_name: None,
                college_code: None,
                regulation: Some(regulation.to_string()),
                error: Some(error.to_string()),
            },
            result: HashMap::new(),
            gpa_details: None,
            sgpa: None,
            history: vec![],
        }
    }

    pub async fn get_result(&self, roll_number: &str, sem: &str) -> CombinedSemesterResult {
        self.get_result_with_refresh(roll_number, sem, false).await
    }

    pub async fn get_result_with_refresh(
        &self,
        roll_number: &str,
        sem: &str,
        force_refresh: bool,
    ) -> CombinedSemesterResult {
        let key = (roll_number.to_string(), sem.to_string());

        if !force_refresh {
            let cache = self.cache.lock().await;
            if let Some((res, time)) = cache.get(&key) {
                if time.elapsed() < self.cache_ttl {
                    return res.clone();
                }
            }
        }

        let result = self.scrape_result(roll_number, sem).await;

        // Only cache successful results; don't cache errors long-term
        if result.details.name.is_some() || !result.result.is_empty() {
            let mut cache = self.cache.lock().await;
            cache.insert(key, (result.clone(), Instant::now()));
        }

        result
    }

    pub async fn clear_cache(&self) {
        let mut cache = self.cache.lock().await;
        let n = cache.len();
        cache.clear();
        tracing::info!("Cleared {} cached results", n);
    }

    pub async fn clear_cache_scoped(&self, htno: Option<&str>, sem: Option<&str>) -> usize {
        let mut cache = self.cache.lock().await;
        let before = cache.len();
        match (htno, sem) {
            (Some(h), Some(s)) => {
                cache.remove(&(h.to_string(), s.to_string()));
            }
            (Some(h), None) => {
                cache.retain(|(rh, _), _| rh != h);
            }
            (None, Some(s)) => {
                cache.retain(|(_, rs), _| rs != s);
            }
            (None, None) => cache.clear(),
        }
        let removed = before - cache.len();
        tracing::info!("Scoped cache clear removed {} entries", removed);
        removed
    }

    async fn scrape_result(&self, roll_number: &str, sem: &str) -> CombinedSemesterResult {
        let admission_year = Self::admission_year(roll_number);
        let degree = Self::degree(roll_number);
        let regulation = Self::regulation(roll_number);

        if Self::is_lateral(roll_number) && (sem == "1-1" || sem == "1-2") {
            return Self::error_result(roll_number, &regulation, "No data available");
        }

        let sem_codes = self.exam_codes.get_semester_codes(&degree, &regulation, sem);
        if sem_codes.is_empty() {
            return Self::error_result(roll_number, &regulation, "No regular exam codes found");
        }

        let has_regular = sem_codes
            .values()
            .any(|t| t.get("regular").map_or(false, |c| !c.is_empty()));
        if !has_regular {
            return Self::error_result(roll_number, &regulation, "No regular exam codes found");
        }

        // Sorted years: deterministic order (was unsorted HashMap iteration before)
        let mut sorted_years: Vec<i32> = sem_codes
            .keys()
            .filter_map(|y| y.parse::<i32>().ok())
            .filter(|y| *y >= admission_year)
            .collect();
        sorted_years.sort_unstable();

        let mut codes_to_fetch = Vec::new();
        let mut seen = HashSet::new();

        for year in &sorted_years {
            if let Some(types) = sem_codes.get(&year.to_string()) {
                // Preserve original type from map; don't relabel later.
                for et in &["regular", "supply"] {
                    if let Some(codes) = types.get(*et) {
                        for code in codes {
                            if seen.insert(code.clone()) {
                                codes_to_fetch.push(ExamCodeInfo {
                                    code: code.clone(),
                                    exam_type: et.to_string(),
                                    year: *year,
                                });
                            }
                        }
                    }
                }
            }
        }

        if codes_to_fetch.is_empty() {
            return Self::error_result(roll_number, &regulation, "No exam codes to fetch");
        }

        let mut all_results = self.fetch_parallel(&codes_to_fetch, &degree, roll_number).await;
        if all_results.is_empty() {
            return Self::error_result(roll_number, &regulation, "Invalid Hallticket or No Results Found");
        }

        // Deterministic history order: year, then regular < supply < rcrv
        all_results.sort_by_key(|r| {
            (
                r.year,
                match r.exam_type.as_str() {
                    "regular" => 0,
                    "supply" => 1,
                    _ => 2,
                },
                r.exam_code.clone(),
            )
        });

        // Drop attempts miscategorized under this sem (same examCode listed under
        // two semesters on the JNTUH home page). Only filters when at least one
        // attempt matches, so unknown subject schemes never lose data.
        if all_results.len() > 1
            && all_results.iter().any(|a| {
                attempt_matches_sem(&a.result.result, sem, &degree, &regulation)
            })
        {
            let before = all_results.len();
            all_results.retain(|a| {
                attempt_matches_sem(&a.result.result, sem, &degree, &regulation)
            });
            if all_results.len() != before {
                tracing::info!(
                    "{} {}: dropped {} miscategorized attempts",
                    roll_number,
                    sem,
                    before - all_results.len()
                );
            }
        }

        let history = all_results.clone();
        let details = history[0].result.details.clone();
        let mut combined: HashMap<String, SubjectResult> = HashMap::new();

        // Pass 1: regular base
        for exam in &history {
            if exam.exam_type == "regular" {
                combined.extend(exam.result.result.clone());
            }
        }
        // Pass 2: supply overwrites only passed subjects (backlog clear)
        for exam in &history {
            if exam.exam_type == "supply" {
                for (sub, data) in &exam.result.result {
                    if data.grade != "F" && data.grade != "Ab" {
                        combined.insert(sub.clone(), data.clone());
                    } else if !combined.contains_key(sub) {
                        // Subject first appeared in supply (rare); keep F for backlog tracking
                        combined.insert(sub.clone(), data.clone());
                    }
                }
            }
        }
        // Pass 3: RCRV overlay — grade improvements apply on top
        for exam in &history {
            if exam.exam_type == "rcrv" {
                for (sub, data) in &exam.result.result {
                    let apply = match combined.get(sub) {
                        Some(cur) => {
                            cur.grade != data.grade
                                && (data.grade != "F" && data.grade != "Ab" || data.rcrv)
                        }
                        None => data.grade != "F" && data.grade != "Ab",
                    };
                    if apply {
                        let mut updated = data.clone();
                        updated.rcrv = true;
                        combined.insert(sub.clone(), updated);
                    } else if data.rcrv {
                        // Mark existing entry as having RCRV even if grade text same
                        if let Some(cur) = combined.get_mut(sub) {
                            cur.rcrv = true;
                        }
                    }
                }
            }
        }

        let gpa = Self::calculate_sgpa(&combined, &degree, &regulation);

        CombinedSemesterResult {
            details,
            result: combined,
            gpa_details: Some(gpa.clone()),
            sgpa: Some(gpa.sgpa),
            history,
        }
    }

    async fn fetch_parallel(
        &self,
        codes: &[ExamCodeInfo],
        degree: &str,
        roll: &str,
    ) -> Vec<ExamAttempt> {
        // Acquire semaphore INSIDE tasks (not before spawn) for true parallelism.
        let sem = Arc::new(tokio::sync::Semaphore::new(5));
        let mut unordered = FuturesUnordered::new();

        for info in codes {
            let sem = sem.clone();
            let client = self.client.clone();
            let url = self.url.clone();
            let deg = degree.to_string();
            let rn = roll.to_string();
            let code = info.code.clone();
            let et = info.exam_type.clone();
            let yr = info.year;
            unordered.push(tokio::spawn(async move {
                let _permit = sem.acquire_owned().await.ok()?;
                Self::fetch_one(&client, &url, &code, &et, yr, &deg, &rn).await.into()
            }));
        }

        let mut out = Vec::new();
        while let Some(res) = unordered.next().await {
            match res {
                Ok(Some(mut v)) => out.append(&mut v),
                Ok(None) => {}
                Err(e) => tracing::debug!("fetch task join error: {}", e),
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
    ) -> Vec<ExamAttempt> {
        const BACKOFF: &[u64] = &[300, 1000, 2000];
        let mut data_opt = None;

        for (attempt, backoff) in BACKOFF.iter().enumerate() {
            let url = format!(
                "{}?examCode={}&etype=r16&result=null&grad=null&type=intgrade&degree={}&htno={}",
                base, code, degree, roll
            );

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
                Ok(r) => {
                    tracing::debug!("examCode {} returned {}", code, r.status());
                    if attempt + 1 < BACKOFF.len() {
                        tokio::time::sleep(Duration::from_millis(*backoff)).await;
                        continue;
                    }
                    return vec![];
                }
                Err(e) => {
                    tracing::debug!("examCode {} attempt {} failed: {}", code, attempt, e);
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
                None => return vec![], // valid "no data" page — don't retry
            }
        }

        let data = match data_opt {
            Some(d) => d,
            None => return vec![],
        };

        let mut results = vec![ExamAttempt {
            exam_code: code.to_string(),
            exam_type: exam_type.to_string(),
            year,
            result: SemesterResultData {
                details: data.details.clone(),
                result: data.result.clone(),
            },
        }];

        // RCRV check is best-effort, single attempt (halves tail latency vs retrying).
        // Per spec: regular always needs RCRV; supply also needs its own RCRV.
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
                            // Only keep RCRV if it actually differs (avoids duplicate noise)
                            let differs = rd.result.iter().any(|(k, v)| {
                                data.result.get(k).map_or(true, |cur| {
                                    cur.grade != v.grade || v.rcrv
                                })
                            });
                            if differs && !rd.result.is_empty() {
                                results.push(ExamAttempt {
                                    exam_code: code.to_string(),
                                    exam_type: "rcrv".into(),
                                    year,
                                    result: SemesterResultData {
                                        details: rd.details,
                                        result: rd.result,
                                    },
                                });
                            }
                        }
                    }
                }
            }
        }

        results
    }

    pub fn parse_html(html: &str) -> Option<SemesterResult> {
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
                        .map_or(false, |c| c.text().collect::<String>().contains("Change in Grade")),
                },
            );
        }

        // Empty result table = no data for this hallticket/code
        if result.is_empty() {
            return None;
        }

        Some(SemesterResult { details, result })
    }

    fn calculate_sgpa(res: &HashMap<String, SubjectResult>, degree: &str, reg: &str) -> GpaDetails {
        let gp = |g: &str| -> f64 {
            // R22+ B.Pharmacy uses O/A/B/C/D grading (no + grades)
            // All other degrees/regulations use O/A+/A/B+/B/C grading
            if degree == "bpharmacy" && (reg == "R22" || reg == "R25") {
                match g {
                    "O" => 10.0,
                    "A" => 9.0,
                    "B" => 8.0,
                    "C" => 7.0,
                    "D" => 6.0,
                    "F" | "Ab" => 0.0,
                    _ => 0.0,
                }
            } else {
                match g {
                    "O" => 10.0,
                    "A+" => 9.0,
                    "A" => 8.0,
                    "B+" => 7.0,
                    "B" => 6.0,
                    "C" => 5.0,
                    "F" | "Ab" => 0.0,
                    _ => 0.0,
                }
            }
        };

        let mut pts = 0.0_f64;
        let mut cre = 0.0_f64;
        let mut status = "PASS";

        for d in res.values() {
            let c: f64 = d.credits.parse().unwrap_or(0.0);
            if d.grade.trim() == "F" || d.grade.trim() == "Ab" {
                status = "FAIL";
            }
            pts += gp(d.grade.trim()) * c;
            cre += c;
        }

        let sgpa = if cre > 0.0 {
            format!("{:.2}", pts / cre)
        } else {
            "0.00".into()
        };

        GpaDetails {
            sgpa,
            credits: cre,
            status: status.into(),
        }
    }
}
