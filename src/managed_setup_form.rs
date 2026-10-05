//! Operator-owned run policy entry. The popup submits the same CLI specification.
use crate::{
    control::model::{Policy, Profile},
    managed_setup::Spec,
};
use anyhow::{Context, Result, ensure};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::{collections::BTreeMap, path::PathBuf};

pub struct Field {
    pub label: &'static str,
    pub value: String,
    pub hint: &'static str,
    options: &'static [&'static str],
}

pub struct Form {
    pub fields: Vec<Field>,
    pub selected: usize,
}

pub enum Outcome {
    Stay,
    Cancel,
    Resume(String),
    Create(Box<Spec>),
}

impl Form {
    pub fn new() -> Self {
        let fields = [
            ("Project name", "", "New project name. To resume a saved setup, enter its name and press Ctrl-R."),
            ("Objective", "", "What should the supervisor accomplish?"),
            ("Controller", "", "State directory of your running controller service."),
            ("Owner credential", "", "Credential file path; blank uses admin.token in the controller directory."),
            ("Repository", "", "Absolute repository ID from the controller and node repository maps."),
            ("Base commit", "", "Full Git commit ID to start from (40 or 64 lowercase hexadecimal characters)."),
            ("Runtime", "codex", "Left/right: Codex, Claude or Pi. Normal configured permission checks apply."),
            ("Model", "", "Optional model ID; blank keeps the runtime's configured default."),
            ("Active agents", "2", "Includes authors and reviewers; one slot is reserved for independent review."),
            ("Task attempts", "3", "Maximum attempts per task, from 1 to 100."),
            ("Run minutes", "60", "Maximum run duration in minutes."),
            ("Check argv", "[]", "Optional check command as argv, e.g. [\"cargo\",\"test\"]. It is named test; [] defines no check."),
            ("Launcher config", "", "Optional controller-host launcher configuration file for opening a native coordinator."),
        ].into_iter().map(|(label, value, hint)| Field {
            label, value: value.into(), hint,
            options: if label == "Runtime" { &["codex", "claude", "pi"] } else { &[] },
        }).collect();
        Self {
            fields,
            selected: 0,
        }
    }

    pub fn key(&mut self, key: KeyEvent) -> Result<Outcome> {
        match key.code {
            KeyCode::Char('r') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                return Ok(Outcome::Resume(crate::project::slug_from_name(
                    self.fields[0].value.trim(),
                )?));
            }
            KeyCode::Esc => return Ok(Outcome::Cancel),
            KeyCode::Up | KeyCode::BackTab => self.selected = self.selected.saturating_sub(1),
            KeyCode::Down | KeyCode::Tab => {
                self.selected = (self.selected + 1).min(self.fields.len() - 1)
            }
            KeyCode::Enter => return Ok(Outcome::Create(Box::new(self.spec()?))),
            KeyCode::Left | KeyCode::Right => {
                let field = &mut self.fields[self.selected];
                if !field.options.is_empty() {
                    let at = field
                        .options
                        .iter()
                        .position(|s| *s == field.value)
                        .unwrap_or(0);
                    let n = field.options.len();
                    let next = if key.code == KeyCode::Right {
                        (at + 1) % n
                    } else {
                        (at + n - 1) % n
                    };
                    field.value = field.options[next].into();
                }
            }
            KeyCode::Backspace if self.fields[self.selected].options.is_empty() => {
                self.fields[self.selected].value.pop();
            }
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                let field = &mut self.fields[self.selected];
                if field.options.is_empty()
                    && !c.is_control()
                    && field.value.len() + c.len_utf8() <= 16384
                {
                    field.value.push(c);
                }
            }
            _ => (),
        }
        Ok(Outcome::Stay)
    }

    fn spec(&self) -> Result<Spec> {
        let value = |index: usize| self.fields[index].value.trim().to_string();
        let controller = PathBuf::from(value(2));
        ensure!(
            !value(2).is_empty(),
            "Choose the controller state directory"
        );
        let credential = if value(3).is_empty() {
            controller.join("admin.token")
        } else {
            value(3).into()
        };
        let slots = value(8)
            .parse::<u32>()
            .context("Active agents must be a number")?;
        let attempts = value(9)
            .parse::<u32>()
            .context("Task attempts must be a number")?;
        let minutes = value(10)
            .parse::<u64>()
            .context("Run minutes must be a number")?;
        let duration = minutes.checked_mul(60).context("Run duration too large")?;
        let check: Vec<String> = serde_json::from_str(&value(11))
            .context("Check argv must be a JSON array of strings")?;
        let mut argv = Vec::new();
        if !value(7).is_empty() {
            argv.extend(["--model".into(), value(7)]);
        }
        let spec = Spec {
            name: value(0),
            objective: value(1),
            controller,
            credential,
            repo: value(4),
            base_oid: value(5),
            policy: Policy {
                max_active: slots,
                reserved_reviewers: 1,
                max_attempts: attempts,
                duration_secs: duration,
                require_review: true,
                profiles: BTreeMap::from([(
                    "auto".into(),
                    Profile {
                        runtime: value(6),
                        argv,
                        limit: slots,
                    },
                )]),
                checks: if check.is_empty() {
                    BTreeMap::new()
                } else {
                    BTreeMap::from([("test".into(), check)])
                },
            },
            launch_config: (!value(12).is_empty()).then(|| PathBuf::from(value(12))),
        };
        spec.validate()?;
        Ok(spec)
    }

    pub fn visible(&self, height: usize) -> std::ops::Range<usize> {
        let count = height.max(1).min(self.fields.len());
        let start = self.selected.saturating_sub(count - 1);
        start..(start + count).min(self.fields.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn valid() -> Form {
        let mut form = Form::new();
        for (index, value) in [
            (0, "Demo"),
            (1, "Build a parser"),
            (2, "/state/controller"),
            (4, "/logical/repo"),
            (5, "1111111111111111111111111111111111111111"),
        ] {
            form.fields[index].value = value.into();
        }
        form
    }
    #[test]
    fn managed_setup_form_keeps_argv_literal_and_requires_review_capacity() {
        let mut form = valid();
        form.fields[11].value = r#"["printf","quoted text; $(touch unwanted)"]"#.into();
        let spec = form.spec().unwrap();
        assert_eq!(
            spec.policy.checks["test"],
            ["printf", "quoted text; $(touch unwanted)"]
        );
        assert!(spec.policy.require_review);
        assert_eq!(spec.policy.reserved_reviewers, 1);
        assert_eq!(
            spec.credential,
            PathBuf::from("/state/controller/admin.token")
        );
        assert!(spec.launch_config.is_none());
        form.fields[8].value = "1".into();
        assert!(form.spec().is_err());
        form.fields[8].value = "2".into();
        form.fields[10].value = u64::MAX.to_string();
        assert!(form.spec().is_err());
    }
    #[test]
    fn managed_setup_form_scroll_keeps_selected_field_visible_and_cancel_is_inert() {
        let mut form = valid();
        for _ in 0..30 {
            form.key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE))
                .unwrap();
        }
        assert_eq!(form.selected, form.fields.len() - 1);
        for height in 0..30 {
            let visible = form.visible(height);
            assert!(visible.contains(&form.selected));
            assert!(visible.end <= form.fields.len());
            assert!(visible.len() <= height.max(1));
        }
        assert!(matches!(
            form.key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))
                .unwrap(),
            Outcome::Cancel
        ));
    }
    #[test]
    fn managed_setup_form_supports_three_runtimes_and_rejects_invalid_check_documents() {
        let mut form = valid();
        form.selected = 6;
        for expected in ["codex", "claude", "pi"] {
            assert_eq!(
                form.spec().unwrap().policy.profiles["auto"].runtime,
                expected
            );
            form.key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE))
                .unwrap();
        }
        form.fields[11].value = "cargo test".into();
        assert!(form.spec().is_err());
        form.fields[11].value = "[]".into();
        assert!(form.spec().unwrap().policy.checks.is_empty());
        form.fields[0].value = "../outside".into();
        assert!(form.spec().is_err());
    }
}
