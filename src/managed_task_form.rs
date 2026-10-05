//! Task creation captures one controller revision and freezes an uncertain submission.
use anyhow::{Context, Result, ensure};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use serde_json::Value;
use std::collections::BTreeSet;

use crate::control::model::{Contract, Mutation, Run, Task, identifier};

pub struct Field {
    pub label: &'static str,
    pub value: String,
    pub hint: String,
}

pub struct Form {
    pub slug: String,
    pub fields: Vec<Field>,
    pub selected: usize,
    run: Run,
    edit: Option<(String, u64)>,
    dependencies: BTreeSet<String>,
    submitted: Option<Submission>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Submission {
    pub args: Vec<String>,
    pub request: String,
}

pub enum Outcome {
    Stay,
    Cancel,
    Submit(Submission),
}

impl Form {
    pub fn new(slug: &str, context: &Value) -> Result<Self> {
        crate::project::validate_slug(slug)?;
        let run: Run = serde_json::from_value(context["run"].clone())?;
        ensure!(run.project == slug, "task form belongs to another project");
        ensure!(
            run.state == "active",
            "task creation requires an active run"
        );
        run.policy.validate()?;
        let mut dependencies = BTreeSet::new();
        let mut choices = Vec::new();
        for task in context["tasks"].as_array().context("task index missing")? {
            let id = task["id"].as_str().context("task ID missing")?;
            identifier(id)?;
            ensure!(task["run"] == run.id, "foreign task in form context");
            dependencies.insert(id.to_string());
            choices.push(format!(
                "{id}: {}",
                task["contract"]["title"].as_str().unwrap_or_default()
            ));
        }
        let checks = run.policy.checks.keys().cloned().collect::<Vec<_>>();
        let fields = vec![
            Field { label: "Title", value: String::new(), hint: "A short name for this task (up to 512 UTF-8 bytes).".into() },
            Field { label: "Instructions", value: String::new(), hint: "The work this agent should do; its contract is fixed when dispatched.".into() },
            Field { label: "Criteria", value: "[]".into(), hint: "Acceptance criteria as JSON strings, e.g. [\"Parser rejects invalid input\",\"Tests pass\"]. At least one is required.".into() },
            Field { label: "Result", value: "code".into(), hint: "Left/right: code (Git commit) or artifact (verified files).".into() },
            Field { label: "Checks", value: serde_json::to_string(&checks)?, hint: format!("JSON list of policy check names. Available: {}", checks.join(", ")) },
            Field { label: "Memory topics", value: "[]".into(), hint: "JSON list of memory topic IDs; approved preferences are included automatically.".into() },
            Field { label: "Dependencies", value: "[]".into(), hint: format!("JSON list of predecessor task IDs; [] for none. Available: {}", choices.join("; ")) },
        ];
        Ok(Self {
            slug: slug.into(),
            fields,
            selected: 0,
            run,
            edit: None,
            dependencies,
            submitted: None,
        })
    }

    pub fn edit(slug: &str, context: &Value, task: &str) -> Result<Self> {
        let mut form = Self::new(slug, context)?;
        let value = context["tasks"]
            .as_array()
            .context("task index missing")?
            .iter()
            .find(|item| item["id"] == task)
            .context("task not present in captured context")?;
        let task: Task = serde_json::from_value(value.clone())?;
        ensure!(task.state == "planned", "only planned tasks can be edited");
        let mut predecessors = Vec::new();
        for edge in context["dependencies"]
            .as_array()
            .context("dependency index missing")?
        {
            if edge["task"] == task.id {
                predecessors.push(
                    edge["predecessor"]
                        .as_str()
                        .context("invalid predecessor")?
                        .to_string(),
                );
            }
        }
        form.fields[0].value = task.contract.title;
        form.fields[1].value = task.contract.instructions;
        form.fields[2].value = serde_json::to_string(&task.contract.criteria)?;
        form.fields[3].value = task.contract.kind;
        form.fields[4].value = serde_json::to_string(&task.contract.checks)?;
        form.fields[5].value = serde_json::to_string(&task.contract.memory_topics)?;
        form.fields[6].value = serde_json::to_string(&predecessors)?;
        form.dependencies.remove(&task.id);
        form.fields[6].hint = format!(
            "JSON list of predecessor task IDs; [] removes all. Available: {}",
            form.dependencies
                .iter()
                .cloned()
                .collect::<Vec<_>>()
                .join(", ")
        );
        form.edit = Some((task.id, task.revision));
        Ok(form)
    }

    pub fn editing(&self) -> bool {
        self.edit.is_some()
    }

    pub fn frozen(&self) -> bool {
        self.submitted.is_some()
    }

    pub fn key(&mut self, key: KeyEvent) -> Result<Outcome> {
        if key.code == KeyCode::Esc {
            return Ok(Outcome::Cancel);
        }
        if key.code == KeyCode::Enter {
            return Ok(Outcome::Submit(self.prepare()?));
        }
        if self.frozen() {
            ensure!(
                matches!(
                    key.code,
                    KeyCode::Up | KeyCode::Down | KeyCode::Tab | KeyCode::BackTab
                ),
                "Submission is frozen; Enter retries the exact request. Escape returns to inspect controller state."
            );
        }
        match key.code {
            KeyCode::Up | KeyCode::BackTab => self.selected = self.selected.saturating_sub(1),
            KeyCode::Down | KeyCode::Tab => {
                self.selected = (self.selected + 1).min(self.fields.len() - 1)
            }
            KeyCode::Left | KeyCode::Right if self.selected == 3 => {
                self.fields[3].value = if self.fields[3].value == "code" {
                    "artifact"
                } else {
                    "code"
                }
                .into();
            }
            KeyCode::Backspace if self.selected != 3 => {
                self.fields[self.selected].value.pop();
            }
            KeyCode::Char(c)
                if self.selected != 3
                    && !key.modifiers.contains(KeyModifiers::CONTROL)
                    && !c.is_control() =>
            {
                let value = &mut self.fields[self.selected].value;
                ensure!(
                    value.len() + c.len_utf8() <= 65536,
                    "Field exceeds 65536 UTF-8 bytes"
                );
                value.push(c);
            }
            _ => (),
        }
        Ok(Outcome::Stay)
    }

    pub fn prepare(&mut self) -> Result<Submission> {
        if let Some(args) = &self.submitted {
            return Ok(args.clone());
        }
        let list = |index: usize| -> Result<Vec<String>> {
            serde_json::from_str(&self.fields[index].value).with_context(|| {
                format!(
                    "{} must be a JSON array of strings",
                    self.fields[index].label
                )
            })
        };
        let contract = Contract {
            title: self.fields[0].value.clone(),
            instructions: self.fields[1].value.clone(),
            criteria: list(2)?,
            kind: self.fields[3].value.clone(),
            checks: list(4)?,
            memory_topics: list(5)?,
        };
        contract.validate(&self.run.policy)?;
        let dependencies = list(6)?;
        ensure!(
            dependencies.iter().all(|id| self.dependencies.contains(id)),
            "Choose dependency IDs from this run's captured task list"
        );
        let command = if let Some((task, revision)) = &self.edit {
            Mutation::AmendTask {
                task: task.clone(),
                expected_revision: *revision,
                contract,
                dependencies,
            }
        } else {
            Mutation::CreateTask {
                run: self.run.id.clone(),
                expected_revision: self.run.revision,
                contract,
                dependencies,
            }
        };
        ensure!(
            serde_json::to_vec(&command)?.len() <= 300000,
            "Task request exceeds the managed API document limit"
        );
        let mut args = crate::managed::Action {
            key: 'n',
            label: "create task",
            command,
            confirm: false,
        }
        .args(&self.slug)?;
        // A contract can exceed the OS per-argument limit. Keep JSON on stdin.
        let request = args.pop().context("task request missing")?;
        ensure!(
            args.pop().as_deref() == Some("--request-json"),
            "unexpected managed argument shape"
        );
        args.extend(["--request".into(), "-".into()]);
        let submission = Submission { args, request };
        self.submitted = Some(submission.clone());
        Ok(submission)
    }

    pub fn visible(&self, height: usize) -> std::ops::Range<usize> {
        let count = height.max(1).min(self.fields.len());
        let start = self.selected.saturating_sub(count - 1);
        start..(start + count).min(self.fields.len())
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use serde_json::json;

    pub(crate) fn context() -> Value {
        json!({"run":{"id":"run-1","project":"demo","repo":"/repo","base_oid":"1111111111111111111111111111111111111111",
            "integration_oid":"1111111111111111111111111111111111111111", "objective":"Improve parser", "state":"active","revision":7,"deadline":4102444800_i64,
            "policy":{"max_active":2,"reserved_reviewers":1,"max_attempts":3,"duration_secs":3600,"require_review":true,
                "profiles":{"auto":{"runtime":"codex","argv":[],"limit":2}},"checks":{"test":["cargo","test"]}}},
            "tasks":[{"id":"task-1","run":"run-1","contract":{"title":"Predecessor"}}]})
    }
    pub(crate) fn edit_context() -> Value {
        let mut doc = context();
        doc["tasks"][0] = json!({"id":"task-1","run":"run-1","state":"planned","revision":4,
            "contract_hash":"old-hash","contract":{"title":"Original title ","instructions":" Keep whitespace ",
                "criteria":["Original criterion"],"kind":"artifact","checks":["test"],"memory_topics":["parser"]}});
        let mut other = doc["tasks"][0].clone();
        other["id"] = json!("task-2");
        doc["tasks"].as_array_mut().unwrap().push(other);
        doc["dependencies"] = json!([{"task":"task-1","predecessor":"task-2"}]);
        doc
    }

    #[test]
    fn amend_task_form_preserves_fields_and_uses_task_revision_and_exact_dependencies() {
        let mut form = Form::edit("demo", &edit_context(), "task-1").unwrap();
        assert!(form.editing());
        assert_eq!(form.fields[0].value, "Original title ");
        assert_eq!(form.fields[1].value, " Keep whitespace ");
        assert_eq!(form.fields[3].value, "artifact");
        let submission = form.prepare().unwrap();
        let command: Value = serde_json::from_str(&submission.request).unwrap();
        assert_eq!(command["method"], "amend_task");
        assert_eq!(command["params"]["task"], "task-1");
        assert_eq!(command["params"]["expected_revision"], 4);
        assert_eq!(
            command["params"]["contract"],
            edit_context()["tasks"][0]["contract"]
        );
        assert_eq!(command["params"]["dependencies"], json!(["task-2"]));
        assert_eq!(form.prepare().unwrap(), submission);
    }

    #[test]
    fn amend_task_form_refuses_live_unknown_self_dependent_or_incomplete_context() {
        let mut doc = edit_context();
        doc["tasks"][0]["state"] = json!("running");
        assert!(Form::edit("demo", &doc, "task-1").is_err());
        assert!(Form::edit("demo", &edit_context(), "missing").is_err());
        let mut form = Form::edit("demo", &edit_context(), "task-1").unwrap();
        form.fields[6].value = r#"["task-1"]"#.into();
        assert!(form.prepare().is_err());
        doc = edit_context();
        doc.as_object_mut().unwrap().remove("dependencies");
        assert!(Form::edit("demo", &doc, "task-1").is_err());
    }

    pub(crate) fn fill(form: &mut Form) {
        form.fields[0].value = "Parser change".into();
        form.fields[1].value = "Preserve literal $HOME and `text`".into();
        form.fields[2].value = r#"["Reject invalid syntax","Keep error positions"]"#.into();
        form.fields[5].value = r#"["parser"]"#.into();
        form.fields[6].value = r#"["task-1"]"#.into();
    }
    #[test]
    fn managed_task_form_validates_policy_dependencies_and_preserves_literal_contract() {
        let mut form = Form::new("demo", &context()).unwrap();
        fill(&mut form);
        form.fields[4].value = r#"["shell"]"#.into();
        assert!(form.prepare().is_err());
        assert!(!form.frozen());
        form.fields[4].value = r#"["test"]"#.into();
        form.fields[6].value = r#"["foreign-task"]"#.into();
        assert!(form.prepare().is_err());
        form.fields[6].value = r#"["task-1"]"#.into();
        let args = form.prepare().unwrap();
        let request: Value = serde_json::from_str(&args.request).unwrap();
        assert_eq!(request["params"]["expected_revision"], 7);
        assert_eq!(
            request["params"]["contract"]["instructions"],
            "Preserve literal $HOME and `text`"
        );
        assert_eq!(request["params"]["dependencies"], json!(["task-1"]));
        assert_eq!(request["params"]["contract"]["checks"], json!(["test"]));
        assert_eq!(
            request["params"]["contract"]["criteria"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
    }
    #[test]
    fn managed_task_form_large_contract_travels_on_stdin_with_bounded_argv() {
        let mut form = Form::new("demo", &context()).unwrap();
        fill(&mut form);
        form.fields[1].value = "\\".repeat(65536);
        let submission = form.prepare().unwrap();
        assert!(submission.request.len() > 131072);
        assert!(submission.args.iter().all(|arg| arg.len() < 512));
        assert_eq!(&submission.args[5..], &["--request", "-"]);
        let request: Value = serde_json::from_str(&submission.request).unwrap();
        assert_eq!(
            request["params"]["contract"]["instructions"]
                .as_str()
                .unwrap()
                .len(),
            65536
        );
    }

    #[test]
    fn managed_task_form_freezes_uncertain_request_and_never_rebases_a_retry() {
        let mut form = Form::new("demo", &context()).unwrap();
        fill(&mut form);
        let args = form.prepare().unwrap();
        assert!(
            form.key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE))
                .is_err()
        );
        assert!(
            form.key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE))
                .is_err()
        );
        // Even an accidental internal mutation cannot change an uncertain operation.
        form.run.revision = 99;
        form.fields[0].value = "Different task".into();
        assert_eq!(form.prepare().unwrap(), args);
        assert!(matches!(
            form.key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))
                .unwrap(),
            Outcome::Cancel
        ));
    }
    #[test]
    fn managed_task_form_rejects_foreign_scope_and_paused_runs_and_supports_artifacts() {
        assert!(Form::new("other", &context()).is_err());
        let mut doc = context();
        doc["run"]["state"] = json!("paused");
        assert!(Form::new("demo", &doc).is_err());
        doc = context();
        doc["tasks"][0]["run"] = json!("other");
        assert!(Form::new("demo", &doc).is_err());
        let mut form = Form::new("demo", &context()).unwrap();
        fill(&mut form);
        form.selected = 3;
        form.key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE))
            .unwrap();
        assert_eq!(form.fields[3].value, "artifact");
        form.fields[2].value = "[]".into();
        assert!(form.prepare().is_err());
        form.selected = 6;
        for height in 0..20 {
            assert!(form.visible(height).contains(&form.selected));
        }
    }
}
