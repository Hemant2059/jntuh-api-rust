use std::collections::BTreeMap;
use std::sync::Arc;

use crate::sem_result::SemResultService;
use crate::types::*;

pub struct AcademicService {
    sem_results: Arc<SemResultService>,
}

impl AcademicService {
    pub fn new(sem_results: Arc<SemResultService>) -> Self {
        Self { sem_results }
    }

    fn regulation(roll: &str) -> String {
        let yr = 2000 + roll[..2].parse::<i32>().unwrap_or(0);
        let deg = if roll.as_bytes().get(5) == Some(&b'A') {
            "btech"
        } else {
            "bpharmacy"
        };
        if yr >= 2023 || (yr == 2022 && roll.as_bytes().get(4) != Some(&b'5')) {
            "R22".into()
        } else if deg == "btech" {
            "R18".into()
        } else {
            "R17".into()
        }
    }

    fn max_sem_index(roll: &str) -> usize {
        use chrono::Datelike;
        let now = chrono::Local::now();
        let adm = 2000 + roll[..2].parse::<i32>().unwrap_or(0);
        let years_done = (now.year() - adm) as usize;
        let mut max = years_done * 2 + if now.month() >= 7 { 1 } else { 0 } + 1;
        max = max.min(7);
        max
    }

    pub async fn get_full_result(&self, roll: &str) -> AcademicResponse {
        self.get_full_result_with_refresh(roll, false).await
    }

    pub async fn get_full_result_with_refresh(
        &self,
        roll: &str,
        force_refresh: bool,
    ) -> AcademicResponse {
        let all = ["1-1", "1-2", "2-1", "2-2", "3-1", "3-2", "4-1", "4-2"];
        let reg = Self::regulation(roll);
        let lateral = SemResultService::is_lateral(roll);
        let max_idx = Self::max_sem_index(roll);

        let mut tasks = Vec::new();
        for (idx, sem) in all.iter().enumerate() {
            if idx > max_idx {
                continue;
            }
            if lateral && (*sem == "1-1" || *sem == "1-2") {
                continue;
            }
            let svc = self.sem_results.clone();
            let r = roll.to_string();
            let s = sem.to_string();
            tasks.push(tokio::spawn(async move {
                let res = svc.get_result_with_refresh(&r, &s, force_refresh).await;
                (s, res)
            }));
        }

        let mut sems: BTreeMap<String, SemesterSummary> = BTreeMap::new();
        let mut details = StudentDetails {
            name: None,
            roll_no: roll.to_string(),
            father_name: None,
            college_code: None,
            regulation: Some(reg.clone()),
            error: None,
        };

        let mut total_pts = 0.0_f64;
        let mut total_cre = 0.0_f64;

        for task in tasks {
            let (sem, res) = match task.await {
                Ok(r) => r,
                Err(_) => continue,
            };
            if res.result.is_empty() {
                continue;
            }

            if details.name.is_none() {
                details.name = res.details.name.clone();
                details.father_name = res.details.father_name.clone();
                details.college_code = res.details.college_code.clone();
            }

            let gpa = res.gpa_details.as_ref().cloned().unwrap_or(GpaDetails {
                sgpa: "0.00".into(),
                credits: 0.0,
                status: "FAIL".into(),
            });

            let failed: Vec<String> = if gpa.status == "FAIL" {
                res.result
                    .iter()
                    .filter(|(_, d)| d.grade == "F" || d.grade == "Ab")
                    .map(|(k, _)| k.clone())
                    .collect()
            } else {
                vec![]
            };

            let sgpa_disp = if gpa.status == "PASS" {
                gpa.sgpa.clone()
            } else {
                "FAIL".into()
            };

            sems.insert(
                sem,
                SemesterSummary {
                    result: res.result,
                    sgpa: sgpa_disp,
                    failed_subjects: failed,
                    history: res.history,
                },
            );

            if gpa.status == "PASS" {
                if let Ok(v) = gpa.sgpa.parse::<f64>() {
                    total_pts += v * gpa.credits;
                    total_cre += gpa.credits;
                }
            }
        }

        if details.name.is_none() {
            details.error = Some("No details found.".into());
        }

        let cgpa = if total_cre > 0.0 {
            format!("{:.2}", (total_pts / total_cre * 100.0).round() / 100.0)
        } else {
            "0.00".into()
        };

        AcademicResponse {
            details,
            semesters: sems,
            cgpa,
        }
    }
}
