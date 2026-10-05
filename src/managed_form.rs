//! Shared values for managed editors; submissions use bounded stdin and stable IDs.
use crate::control::model::{Mutation, digest};
use anyhow::{Result, ensure};

pub struct Field {
    pub label: &'static str,
    pub value: String,
    pub hint: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Submission {
    pub args: Vec<String>,
    pub request: String,
}

impl Submission {
    pub fn new(slug: &str, command: &Mutation) -> Result<Self> {
        crate::project::validate_slug(slug)?;
        let request = serde_json::to_string(command)?;
        ensure!(
            request.len() <= 300000,
            "Request exceeds the managed API document limit"
        );
        let args = vec![
            "managed".into(),
            "apply".into(),
            slug.into(),
            "--operation".into(),
            format!("ui-{}", digest(request.as_bytes())),
            "--request".into(),
            "-".into(),
        ];
        Ok(Self { args, request })
    }
}
