//! Owner-only profile editor; existing assignments keep their reserved settings.
use crate::{
    control::model::{Mutation, Profile, Run, identifier},
    managed_form::{Field, Submission},
};
use anyhow::{Context, Result, ensure};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use serde_json::Value;

pub struct Form {
    pub slug: String,
    pub fields: Vec<Field>,
    pub selected: usize,
    run: Run,
    existing: Option<String>,
    submitted: Option<Submission>,
}

pub enum Outcome {
    Stay,
    Cancel,
    Submit(Submission),
}

impl Form {
    pub fn new(slug: &str, context: &Value, name: Option<&str>) -> Result<Self> {
        crate::project::validate_slug(slug)?;
        ensure!(
            context["actor_role"] == "owner",
            "Only the owner can edit run profiles"
        );
        let run: Run = serde_json::from_value(context["run"].clone())?;
        ensure!(run.project == slug, "profile belongs to another project");
        ensure!(
            ["active", "paused"].contains(&run.state.as_str()),
            "run is closed"
        );
        let profile = if let Some(name) = name {
            run.policy
                .profiles
                .get(name)
                .context("profile no longer exists")?
                .clone()
        } else {
            Profile {
                runtime: "codex".into(),
                argv: vec![],
                limit: 1,
            }
        };
        let fields = vec![
            Field {label:"Name",value:name.unwrap_or_default().into(),hint:if name.is_some() {"Existing names are fixed; create a new profile to use a different name."} else {"Unique profile ID used when assigning workers or reviewers."}.into()},
            Field {label:"Runtime",value:profile.runtime,hint:"Left/right: Codex, Claude or Pi. Existing attempts retain their assigned runtime.".into()},
            Field {label:"Arguments",value:serde_json::to_string(&profile.argv)?,hint:"JSON argv, e.g. [\"--model\",\"MODEL\"]. Only supported model/tuning flags are allowed.".into()},
            Field {label:"Active limit",value:profile.limit.to_string(),hint:"New reservations count every active attempt with this name, including earlier settings.".into()},
        ];
        Ok(Self {
            slug: slug.into(),
            fields,
            selected: 0,
            run,
            existing: name.map(str::to_owned),
            submitted: None,
        })
    }
    pub fn editing(&self) -> bool {
        self.existing.is_some()
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
                "Submission is frozen; Enter retries exactly. Escape returns to inspect state."
            );
        }
        match key.code {
            KeyCode::Up | KeyCode::BackTab => self.selected = self.selected.saturating_sub(1),
            KeyCode::Down | KeyCode::Tab => {
                self.selected = (self.selected + 1).min(self.fields.len() - 1)
            }
            KeyCode::Left | KeyCode::Right if self.selected == 1 => {
                let runtimes = ["codex", "claude", "pi"];
                let at = runtimes
                    .iter()
                    .position(|r| *r == self.fields[1].value)
                    .unwrap_or(0);
                self.fields[1].value =
                    runtimes[(at + if key.code == KeyCode::Right { 1 } else { 2 }) % 3].into();
            }
            KeyCode::Backspace if self.selected != 1 && !(self.selected == 0 && self.editing()) => {
                self.fields[self.selected].value.pop();
            }
            KeyCode::Char(c)
                if self.selected != 1
                    && !(self.selected == 0 && self.editing())
                    && !key.modifiers.contains(KeyModifiers::CONTROL)
                    && !c.is_control() =>
            {
                let field = &mut self.fields[self.selected].value;
                ensure!(
                    field.len() + c.len_utf8() <= 32768,
                    "Profile field too large"
                );
                field.push(c);
            }
            _ => (),
        }
        Ok(Outcome::Stay)
    }
    pub fn prepare(&mut self) -> Result<Submission> {
        if let Some(submitted) = &self.submitted {
            return Ok(submitted.clone());
        }
        let name = self.fields[0].value.trim().to_string();
        identifier(&name)?;
        if let Some(existing) = &self.existing {
            ensure!(existing == &name, "Existing profile names cannot change");
        } else {
            ensure!(
                !self.run.policy.profiles.contains_key(&name),
                "Profile already exists; open its editor instead"
            );
        }
        let profile = Profile {
            runtime: self.fields[1].value.clone(),
            argv: serde_json::from_str(&self.fields[2].value)
                .context("Arguments must be a JSON array of strings")?,
            limit: self.fields[3]
                .value
                .trim()
                .parse()
                .context("Active limit must be a positive number")?,
        };
        let mut policy = self.run.policy.clone();
        policy.profiles.insert(name.clone(), profile.clone());
        policy.validate()?;
        let submission = Submission::new(
            &self.slug,
            &Mutation::SetProfile {
                run: self.run.id.clone(),
                expected_revision: self.run.revision,
                name,
                profile: Some(profile),
            },
        )?;
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
        let mut doc = crate::managed_task_form::tests::context();
        doc["actor_role"] = json!("owner");
        doc["run"]["policy"]["profiles"]["second"] =
            json!({"runtime":"claude","argv":["--model","original"],"limit":1});
        doc
    }
    #[test]
    fn run_profiles_form_is_owner_only_and_preserves_name_argv_and_run_revision() {
        let mut denied = context();
        denied["actor_role"] = json!("supervisor");
        assert!(Form::new("demo", &denied, Some("auto")).is_err());
        assert!(Form::new("wrong", &context(), None).is_err());
        let mut form = Form::new("demo", &context(), Some("second")).unwrap();
        assert!(form.editing());
        form.key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE))
            .unwrap();
        assert_eq!(form.fields[0].value, "second");
        assert_eq!(form.fields[2].value, r#"["--model","original"]"#);
        form.fields[2].value = r#"["--model","literal $HOME; `text`"]"#.into();
        let submission = form.prepare().unwrap();
        let request: Value = serde_json::from_str(&submission.request).unwrap();
        assert_eq!(request["method"], "set_profile");
        assert_eq!(request["params"]["expected_revision"], 7);
        assert_eq!(
            request["params"]["profile"]["argv"],
            json!(["--model", "literal $HOME; `text`"])
        );
        assert!(
            form.key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE))
                .is_err()
        );
        form.run.revision = 99;
        assert_eq!(form.prepare().unwrap(), submission);
    }
    #[test]
    fn run_profiles_form_validates_permission_flags_name_and_limits_before_freezing() {
        let mut form = Form::new("demo", &context(), None).unwrap();
        form.fields[0].value = "auto".into();
        assert!(form.prepare().is_err());
        form.fields[0].value = "new".into();
        form.fields[2].value = r#"["--dangerously-bypass-approvals-and-sandbox"]"#.into();
        assert!(form.prepare().is_err());
        assert!(!form.frozen());
        form.fields[2].value = "[]".into();
        form.fields[3].value = "0".into();
        assert!(form.prepare().is_err());
        form.fields[3].value = "1".into();
        form.selected = 1;
        form.key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE))
            .unwrap();
        assert_eq!(form.fields[1].value, "pi");
        form.selected = 3;
        for h in 0..12 {
            assert!(form.visible(h).contains(&3));
        }
        assert!(form.prepare().is_ok());
    }
}
