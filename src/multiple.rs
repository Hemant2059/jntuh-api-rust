use std::collections::HashMap;
use std::sync::Arc;

use crate::sem_result::SemResultService;
use crate::types::*;

const DIGITS: &[u8] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ";

pub fn generate_roll_numbers(start: &str, end: &str) -> Vec<String> {
    let prefix = &start[..start.len() - 3];
    let s = start.as_bytes();
    let e = end.as_bytes();
    let sl = start.len();

    let start_f = s[sl - 3];
    let start_m = s[sl - 2];
    let start_l = (s[sl - 1] - b'0') as i32;

    let end_f = e[sl - 3];
    let end_m = e[sl - 2];
    let end_l = (e[sl - 1] - b'0') as i32;

    let mut out = Vec::new();
    let mut f = start_f;
    let mut mi = DIGITS.iter().position(|&c| c == start_m).unwrap_or(0);
    let mut l = start_l;

    loop {
        out.push(format!("{}{}{}{}", prefix, f as char, DIGITS[mi] as char, l));

        if f == end_f && DIGITS[mi] == end_m && l == end_l {
            break;
        }

        l += 1;
        if l > 9 { l = 0; mi += 1; }
        if mi >= DIGITS.len() { mi = 0; f += 1; }
    }

    out
}

pub struct ClassResultService {
    sem_results: Arc<SemResultService>,
}

impl ClassResultService {
    pub fn new(sem_results: Arc<SemResultService>) -> Self {
        Self { sem_results }
    }

    pub async fn fetch_all_students(
        &self, roll_numbers: &[String], semester: &str, concurrency: usize,
    ) -> HashMap<String, ClassResultEntry> {
        let sem = Arc::new(tokio::sync::Semaphore::new(concurrency.clamp(1, 50)));
        let mut tasks = Vec::new();

        for rn in roll_numbers {
            let p = sem.clone().acquire_owned().await;
            let svc = self.sem_results.clone();
            let r = rn.clone();
            let s = semester.to_string();
            let r_arg = r.clone();
            let s_arg = s.clone();
            tasks.push(tokio::spawn(async move {
                let _p = p;
                (r, svc.get_result(&r_arg, &s_arg).await)
            }));
        }

        let mut map: HashMap<String, ClassResultEntry> = HashMap::new();
        for task in tasks {
            let (roll, res) = match task.await {
                Ok(r) => r,
                Err(_) => continue,
            };

            let entry = if res.result.is_empty() && res.history.is_empty() {
                ClassResultEntry {
                    details: StudentDetails {
                        name: None, roll_no: roll.clone(), father_name: None,
                        college_code: None, regulation: None,
                        error: Some("No result found".into()),
                    },
                    result: HashMap::new(), history: vec![], sgpa: "0.00".into(),
                }
            } else {
                ClassResultEntry {
                    details: res.details,
                    result: res.result,
                    history: res.history,
                    sgpa: res.sgpa.unwrap_or_else(|| "0.00".into()),
                }
            };
            map.insert(roll, entry);
        }

        let mut sorted: Vec<_> = map.into_iter().collect();
        sorted.sort_by_key(|(k, _)| k.clone());
        sorted.into_iter().collect()
    }
}
