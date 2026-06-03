use std::collections::HashMap;
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
    pub semesters: HashMap<String, SemesterSummary>,
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
    pub results: HashMap<String, Vec<DetailedExamEntry>>,
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
