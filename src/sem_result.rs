use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use reqwest::Client;
use scraper::{Html, Selector};
use tokio::sync::Mutex;

use crate::extract_code::ExamCodes;
use crate::types::*;

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
            cache_ttl: Duration::from_secs(86400),
        }
    }

    fn regulation(roll_number: &str) -> String {
        let yr = 2000 + roll_number[..2].parse::<i32>().unwrap_or(0);
        let deg = Self::degree(roll_number);
        if yr >= 2023 || (yr == 2022 && roll_number.as_bytes().get(4) != Some(&b'5')) {
            "R22".into()
        } else if deg == "btech" { "R18".into() } else { "R17".into() }
    }

    fn degree(roll_number: &str) -> String {
        if roll_number.as_bytes().get(5) == Some(&b'A') { "btech".into() } else { "bpharmacy".into() }
    }

    fn admission_year(roll_number: &str) -> i32 {
        2000 + roll_number[..2].parse::<i32>().unwrap_or(0)
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
        let key = (roll_number.to_string(), sem.to_string());

        {
            let cache = self.cache.lock().await;
            if let Some((res, time)) = cache.get(&key) {
                if time.elapsed() < self.cache_ttl {
                    return res.clone();
                }
            }
        }

        let result = self.scrape_result(roll_number, sem).await;

        let cache_result = result.clone();
        let mut cache = self.cache.lock().await;
        cache.insert(key, (cache_result, Instant::now()));

        result
    }

    pub async fn clear_cache(&self) {
        let mut cache = self.cache.lock().await;
        let n = cache.len();
        cache.clear();
        tracing::info!("Cleared {} cached results", n);
    }

    async fn scrape_result(&self, roll_number: &str, sem: &str) -> CombinedSemesterResult {
        let admission_year = Self::admission_year(roll_number);
        let degree = Self::degree(roll_number);
        let regulation = Self::regulation(roll_number);

        let sem_codes = self.exam_codes.get_semester_codes(&degree, &regulation, sem);
        if sem_codes.is_empty() {
            return Self::error_result(roll_number, &regulation, "No regular exam codes found");
        }

        let has_regular = sem_codes.values().any(|t| t.get("regular").map_or(false, |c| !c.is_empty()));
        if !has_regular {
            return Self::error_result(roll_number, &regulation, "No regular exam codes found");
        }

        if roll_number.as_bytes().get(4) == Some(&b'5') && (sem == "1-1" || sem == "1-2") {
            return Self::error_result(roll_number, &regulation, "No data available");
        }

        let sorted_years: Vec<i32> = sem_codes.keys()
            .filter_map(|y| y.parse::<i32>().ok())
            .filter(|y| *y >= admission_year)
            .collect();

        let mut codes_to_fetch = Vec::new();
        let mut seen = HashSet::new();

        for year in &sorted_years {
            if let Some(types) = sem_codes.get(&year.to_string()) {
                for et in &["regular", "supply", "rcrv"] {
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

        all_results.sort_by_key(|r| (r.year, match r.exam_type.as_str() {
            "regular" => 0, "supply" => 1, _ => 2,
        }));

        let mut first_regular = false;
        for r in &mut all_results {
            if r.exam_type != "rcrv" {
                if !first_regular { r.exam_type = "regular".into(); first_regular = true; }
                else { r.exam_type = "supply".into(); }
            }
        }

        let history = all_results.clone();
        let details = history[0].result.details.clone();
        let mut combined = HashMap::new();

        for exam in &history {
            match exam.exam_type.as_str() {
                "regular" => combined.extend(exam.result.result.clone()),
                "supply" => {
                    for (sub, data) in &exam.result.result {
                        if (data.grade != "F" && data.grade != "Ab") && combined.contains_key(sub) {
                            combined.insert(sub.clone(), data.clone());
                        }
                    }
                }
                _ => {}
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

    async fn fetch_parallel(&self, codes: &[ExamCodeInfo], degree: &str, roll: &str) -> Vec<ExamAttempt> {
        let sem = Arc::new(tokio::sync::Semaphore::new(10));
        let mut tasks = Vec::new();

        for info in codes {
            let permit = sem.clone().acquire_owned().await;
            let client = self.client.clone();
            let url = self.url.clone();
            let deg = degree.to_string();
            let rn = roll.to_string();
            let code = info.code.clone();
            let et = info.exam_type.clone();
            let yr = info.year;

            tasks.push(tokio::spawn(async move {
                let _p = permit;
                Self::fetch_one(&client, &url, &code, &et, yr, &deg, &rn).await
            }));
        }

        let mut out = Vec::new();
        for t in tasks {
            if let Ok(mut v) = t.await {
                out.append(&mut v);
            }
        }
        out
    }

    async fn fetch_one(
        client: &Client, base: &str, code: &str, exam_type: &str, year: i32,
        degree: &str, roll: &str,
    ) -> Vec<ExamAttempt> {
        for attempt in 0..2 {
            let url = format!(
                "{}?examCode={}&etype=r16&result=null&grad=null&type=intgrade&degree={}&htno={}",
                base, code, degree, roll
            );

            let resp = match client.get(&url)
                .header("User-Agent", "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36")
                .timeout(Duration::from_secs(10))
                .send().await
            {
                Ok(r) if r.status().is_success() => r,
                _ => {
                    if attempt == 0 { tokio::time::sleep(Duration::from_millis(300)).await; continue; }
                    return vec![];
                }
            };

            let bytes = match resp.bytes().await {
                Ok(b) if b.len() > 100 => b,
                _ => {
                    if attempt == 0 { tokio::time::sleep(Duration::from_millis(300)).await; continue; }
                    return vec![];
                }
            };

            let html = String::from_utf8_lossy(&bytes);
            let data = match Self::parse_html(&html) {
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

            if exam_type != "rcrv" {
                let rurl = format!(
                    "{}?examCode={}&etype=r16&result=gradercrv&grad=null&type=intgrade&degree={}&htno={}",
                    base, code, degree, roll
                );
                if let Ok(rr) = client.get(&rurl)
                    .header("User-Agent", "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36")
                    .timeout(Duration::from_secs(10))
                    .send().await
                {
                    if rr.status().is_success() {
                        if let Ok(b) = rr.bytes().await {
                            if b.len() > 100 {
                                let h = String::from_utf8_lossy(&b);
                                if let Some(rd) = Self::parse_html(&h) {
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

            return results;
        }

        vec![]
    }

    pub fn parse_html(html: &str) -> Option<SemesterResult> {
        let doc = Html::parse_document(html);
        if doc.select(&Selector::parse("form[id='myForm']").unwrap()).next().is_some() {
            return None;
        }

        let tables: Vec<_> = doc.select(&Selector::parse("table").unwrap()).collect();
        if tables.len() < 2 { return None; }

        let tr = Selector::parse("tr").unwrap();
        let td = Selector::parse("td").unwrap();
        let d_rows: Vec<_> = tables[0].select(&tr).collect();
        let r_rows: Vec<_> = tables[1].select(&tr).collect();
        if d_rows.len() < 2 || r_rows.is_empty() { return None; }

        let d0: Vec<_> = d_rows[0].select(&td).collect();
        let d1: Vec<_> = d_rows[1].select(&td).collect();

        let details = StudentDetails {
            name: d0.get(3).map(|c| c.text().collect::<String>().trim().to_string()),
            roll_no: d0.get(1).map(|c| c.text().collect::<String>().trim().to_string()).unwrap_or_default(),
            father_name: d1.get(1).map(|c| c.text().collect::<String>().trim().to_string()),
            college_code: d1.get(3).map(|c| c.text().collect::<String>().trim().to_string()),
            regulation: None,
            error: None,
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
                rcrv: cells.last().map_or(false, |c| c.text().collect::<String>().contains("Change in Grade")),
            });
        }

        Some(SemesterResult { details, result })
    }

    fn calculate_sgpa(res: &HashMap<String, SubjectResult>, degree: &str, reg: &str) -> GpaDetails {
        let gp = |g: &str| -> f64 {
            // R22+ B.Pharmacy uses O/A/B/C/D grading (no + grades)
            // All other degrees/regulations use O/A+/A/B+/B/C grading
            if degree == "bpharmacy" && (reg == "R22" || reg == "R25") {
                match g { "O" => 10.0, "A" => 9.0, "B" => 8.0, "C" => 7.0, "D" => 6.0, "F" | "Ab" => 0.0, _ => 0.0 }
            } else {
                match g { "O" => 10.0, "A+" => 9.0, "A" => 8.0, "B+" => 7.0, "B" => 6.0, "C" => 5.0, "F" | "Ab" => 0.0, _ => 0.0 }
            }
        };

        let mut pts = 0.0_f64;
        let mut cre = 0.0_f64;
        let mut status = "PASS";

        for d in res.values() {
            let c: f64 = d.credits.parse().unwrap_or(0.0);
            if d.grade.trim() == "F" || d.grade.trim() == "Ab" { status = "FAIL"; }
            pts += gp(d.grade.trim()) * c;
            cre += c;
        }

        let sgpa = if cre > 0.0 { format!("{:.2}", pts / cre) } else { "0.00".into() };

        GpaDetails { sgpa, credits: cre, status: status.into() }
    }
}
