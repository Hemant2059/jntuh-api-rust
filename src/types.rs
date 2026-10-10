use std::collections::{BTreeMap, HashMap};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct SubjectResult {
    pub name: String,
    pub internal: String,
    pub external: String,
    pub total: String,
    pub grade: String,
    pub credits: String,
    pub rcrv: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct StudentDetails {
    #[serde(rename = "NAME")]
    pub name: Option<String>,
    #[serde(rename = "Roll_No")]
    pub roll_no: String,
    #[serde(rename = "FATHER_NAME")]
    pub father_name: Option<String>,
    #[serde(rename = "COLLEGE_CODE")]
    pub college_code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub regulation: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct SemesterResult {
    pub details: StudentDetails,
    pub result: HashMap<String, SubjectResult>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ExamAttempt {
    pub exam_code: String,
    #[serde(rename = "type")]
    pub exam_type: String,
    pub year: i32,
    pub result: SemesterResultData,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct SemesterResultData {
    pub details: StudentDetails,
    pub result: HashMap<String, SubjectResult>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct GpaDetails {
    pub sgpa: String,
    pub credits: f64,
    pub status: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct CombinedSemesterResult {
    pub details: StudentDetails,
    pub result: HashMap<String, SubjectResult>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gpa_details: Option<GpaDetails>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sgpa: Option<String>,
    pub history: Vec<ExamAttempt>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AcademicResponse {
    pub details: StudentDetails,
    pub semesters: BTreeMap<String, SemesterSummary>,
    pub cgpa: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct SemesterSummary {
    pub result: HashMap<String, SubjectResult>,
    pub sgpa: String,
    pub failed_subjects: Vec<String>,
    pub history: Vec<ExamAttempt>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AllResultResponse {
    pub details: StudentDetails,
    pub results: BTreeMap<String, Vec<DetailedExamEntry>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct DetailedExamEntry {
    pub exam_code: String,
    #[serde(rename = "type")]
    pub exam_type: String,
    pub year: i32,
    pub result_url: String,
    pub result: HashMap<String, SubjectResult>,
    pub details: StudentDetails,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ClassResultEntry {
    pub details: StudentDetails,
    pub result: HashMap<String, SubjectResult>,
    pub history: Vec<ExamAttempt>,
    pub sgpa: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct Notification {
    pub title: String,
    pub short_title: String,
    pub slug: String,
    pub url: String,
    pub exam_date: String,
    pub publish_date: String,
    pub degree: String,
    pub semester: String,
    pub regulation: String,
    #[serde(rename = "type")]
    pub exam_type: String,
    pub category: String,
    pub is_rcrv: bool,
}

pub type ExamCodesMap = HashMap<String, HashMap<String, HashMap<String, HashMap<String, HashMap<String, Vec<String>>>>>>;

/// Semester index 1-1→1 … 4-2→8.
pub fn sem_index(sem: &str) -> Option<u8> {
    match sem {
        "1-1" => Some(1),
        "1-2" => Some(2),
        "2-1" => Some(3),
        "2-2" => Some(4),
        "3-1" => Some(5),
        "3-2" => Some(6),
        "4-1" => Some(7),
        "4-2" => Some(8),
        _ => None,
    }
}

/// Checks whether a fetched attempt plausibly belongs to `sem`.
///
/// R18-style BTech subject codes embed the semester (`151xx`=1-1 … `158xx`=4-2).
/// JNTUH occasionally lists one examCode under two semesters on the home page
/// (e.g. a typo'd title), which would otherwise merge foreign subjects into the
/// semester. An attempt is dropped only when it *positively* belongs elsewhere
/// (all subjects match the scheme but a different semester); anything
/// unrecognized is kept so unknown schemes never lose data.
pub fn attempt_matches_sem(
    subjects: &HashMap<String, SubjectResult>,
    sem: &str,
    degree: &str,
    reg: &str,
) -> bool {
    if subjects.is_empty() {
        return false;
    }
    let expected = match (degree, reg, sem_index(sem)) {
        ("btech", "R18" | "R22", Some(i)) => i,
        _ => return true, // unknown scheme — keep
    };
    for code in subjects.keys() {
        let b = code.as_bytes();
        if b.len() >= 3 && b[0] == b'1' && b[1] == b'5' && b[2].is_ascii_digit() {
            if b[2] == b'0' + expected {
                return true; // at least one subject of this sem — keep
            }
        } else {
            return true; // non-conforming code — can't validate, keep
        }
    }
    false // all subjects match the scheme but another semester — drop
}
