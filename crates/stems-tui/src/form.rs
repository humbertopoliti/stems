//! The script argument form (30): one field per declared `args` entry,
//! defaults prefilled, enums as a `←/→` picker, bools as a toggle, and
//! inline validation with [`stems_core::scriptargs::parse_args_json`]
//! before anything is sent.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use serde_json::{Map, Value};
use stems_config::{ArgType, Scalar, ScriptArg};

/// A field's value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FieldValue {
    /// Free text (`string`, `int`, `float`, `path`); empty = not given.
    Text(String),
    /// An enum choice: an index into `values`, `None` = not given.
    Choice(Option<usize>),
    /// A bool; `None` = not given (no default).
    Toggle(Option<bool>),
}

/// One field of the form.
#[derive(Clone, Debug, PartialEq)]
pub struct FormField {
    /// The declared argument.
    pub arg: ScriptArg,
    /// Its current value.
    pub value: FieldValue,
    /// The inline error of the last submit.
    pub error: Option<String>,
}

impl FormField {
    /// A field with the argument's default prefilled.
    pub fn new(arg: ScriptArg) -> Self {
        let value = match arg.kind {
            ArgType::Enum => FieldValue::Choice(match &arg.default {
                Some(d) => arg.values.iter().position(|v| *v == d.to_string()),
                None if arg.required => Some(0).filter(|_| !arg.values.is_empty()),
                None => None,
            }),
            ArgType::Bool => FieldValue::Toggle(match &arg.default {
                Some(Scalar::Bool(b)) => Some(*b),
                Some(other) => Some(matches!(
                    other.to_string().to_ascii_lowercase().as_str(),
                    "true" | "yes" | "y" | "on" | "1"
                )),
                None => None,
            }),
            _ => FieldValue::Text(
                arg.default
                    .as_ref()
                    .map(ToString::to_string)
                    .unwrap_or_default(),
            ),
        };
        Self {
            arg,
            value,
            error: None,
        }
    }

    /// The value as shown (`admin`, `[x]`, `-`).
    pub fn display(&self) -> String {
        match &self.value {
            FieldValue::Text(s) => s.clone(),
            FieldValue::Choice(Some(i)) => self.arg.values.get(*i).cloned().unwrap_or_default(),
            FieldValue::Choice(None) => "-".into(),
            FieldValue::Toggle(Some(true)) => "true".into(),
            FieldValue::Toggle(Some(false)) => "false".into(),
            FieldValue::Toggle(None) => "-".into(),
        }
    }

    /// The JSON value sent (`None` = not given).
    pub fn json(&self) -> Option<Value> {
        match &self.value {
            FieldValue::Text(s) if s.is_empty() => None,
            FieldValue::Text(s) => Some(Value::String(s.clone())),
            FieldValue::Choice(i) => i
                .and_then(|i| self.arg.values.get(i))
                .map(|v| Value::String(v.clone())),
            FieldValue::Toggle(b) => b.map(Value::Bool),
        }
    }

    /// Cycle an enum (`+1` right, `-1` left; an optional enum without a
    /// default includes "not given") or flip a bool.
    pub fn cycle(&mut self, delta: isize) {
        let optional = !self.arg.required && self.arg.default.is_none();
        match &mut self.value {
            FieldValue::Choice(cur) => {
                let n = self.arg.values.len();
                if n == 0 {
                    return;
                }
                // Positions: 0 = none (when optional), then the values.
                let off = usize::from(optional);
                let total = (n + off) as isize;
                let pos = match cur {
                    Some(i) => *i + off,
                    None => 0,
                } as isize;
                let next = (pos + delta).rem_euclid(total) as usize;
                *cur = if optional && next == 0 {
                    None
                } else {
                    Some(next - off)
                };
            }
            FieldValue::Toggle(b) => {
                *b = Some(!b.unwrap_or(false));
            }
            FieldValue::Text(_) => {}
        }
    }
}

/// The form of one script run.
#[derive(Clone, Debug, PartialEq)]
pub struct ScriptForm {
    /// Owning stem (`None`: a workspace script).
    pub stem: Option<String>,
    /// Script name.
    pub script: String,
    /// The fields, in declaration order.
    pub fields: Vec<FormField>,
    /// The focused field.
    pub focus: usize,
    /// A form-level error (e.g. an invalid schema).
    pub error: Option<String>,
}

/// What a key did to the form.
#[derive(Clone, Debug, PartialEq)]
pub enum FormAction {
    /// Nothing more to do.
    Handled,
    /// Close the form.
    Cancel,
    /// Valid: run the script with these arguments.
    Submit(Map<String, Value>),
}

impl ScriptForm {
    /// A form for `script` with its declared `args`.
    pub fn new(stem: Option<String>, script: impl Into<String>, args: &[ScriptArg]) -> Self {
        Self {
            stem,
            script: script.into(),
            fields: args.iter().cloned().map(FormField::new).collect(),
            focus: 0,
            error: None,
        }
    }

    /// The arguments as a JSON object (fields not given are left out).
    pub fn values(&self) -> Map<String, Value> {
        self.fields
            .iter()
            .filter_map(|f| f.json().map(|v| (f.arg.name.clone(), v)))
            .collect()
    }

    /// Validate with `parse_args_json`: on error, the message goes to the
    /// offending field (or the form), focus moves there, and the message
    /// is returned.
    pub fn validate(&mut self) -> Result<Map<String, Value>, String> {
        for f in &mut self.fields {
            f.error = None;
        }
        self.error = None;
        let values = self.values();
        let schema: Vec<ScriptArg> = self.fields.iter().map(|f| f.arg.clone()).collect();
        match stems_core::scriptargs::parse_args_json(&schema, &values) {
            Ok(_) => Ok(values),
            Err(e) => {
                let arg = e.details.get("arg").and_then(Value::as_str);
                let reason = e
                    .details
                    .get("reason")
                    .and_then(Value::as_str)
                    .map_or_else(|| e.message.clone(), str::to_string);
                match arg.and_then(|a| self.fields.iter().position(|f| f.arg.name == a)) {
                    Some(i) => {
                        self.fields[i].error = Some(reason.clone());
                        self.focus = i;
                        Err(reason)
                    }
                    None => {
                        self.error = Some(e.message.clone());
                        Err(e.message)
                    }
                }
            }
        }
    }

    /// Handle a key.
    pub fn key(&mut self, k: KeyEvent) -> FormAction {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let n = self.fields.len();
        match k.code {
            KeyCode::Esc => return FormAction::Cancel,
            KeyCode::Enter => {
                return match self.validate() {
                    Ok(v) => FormAction::Submit(v),
                    Err(_) => FormAction::Handled,
                };
            }
            KeyCode::Tab | KeyCode::Down if n > 0 => self.focus = (self.focus + 1) % n,
            KeyCode::BackTab | KeyCode::Up if n > 0 => self.focus = (self.focus + n - 1) % n,
            _ => {}
        }
        let Some(f) = self.fields.get_mut(self.focus) else {
            return FormAction::Handled;
        };
        match (&mut f.value, k.code) {
            (FieldValue::Text(s), KeyCode::Backspace) => {
                s.pop();
                f.error = None;
            }
            (FieldValue::Text(s), KeyCode::Char(c)) if !ctrl => {
                s.push(c);
                f.error = None;
            }
            (FieldValue::Text(s), KeyCode::Char('u')) if ctrl => s.clear(),
            (
                FieldValue::Choice(_) | FieldValue::Toggle(_),
                KeyCode::Right | KeyCode::Char('l'),
            ) => {
                f.cycle(1);
                f.error = None;
            }
            (FieldValue::Choice(_) | FieldValue::Toggle(_), KeyCode::Left | KeyCode::Char('h')) => {
                f.cycle(-1);
                f.error = None;
            }
            (FieldValue::Toggle(_), KeyCode::Char(' ')) => {
                f.cycle(1);
                f.error = None;
            }
            _ => {}
        }
        FormAction::Handled
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn arg(v: Value) -> ScriptArg {
        serde_json::from_value(v).expect("script arg")
    }

    fn schema() -> Vec<ScriptArg> {
        vec![
            arg(
                serde_json::json!({"name": "email", "type": "string", "required": true,
                "default": null, "description": "Email", "values": []}),
            ),
            arg(
                serde_json::json!({"name": "role", "type": "enum", "required": false,
                "default": "admin", "description": null, "values": ["admin", "user"]}),
            ),
            arg(
                serde_json::json!({"name": "count", "type": "int", "required": false,
                "default": 2, "description": null, "values": []}),
            ),
            arg(
                serde_json::json!({"name": "verbose", "type": "bool", "required": false,
                "default": null, "description": null, "values": []}),
            ),
            arg(
                serde_json::json!({"name": "tier", "type": "enum", "required": false,
                "default": null, "description": null, "values": ["gold", "silver"]}),
            ),
        ]
    }

    fn key(c: KeyCode) -> KeyEvent {
        KeyEvent::new(c, KeyModifiers::NONE)
    }

    fn typed(f: &mut ScriptForm, s: &str) {
        for c in s.chars() {
            f.key(key(KeyCode::Char(c)));
        }
    }

    #[test]
    fn defaults_are_prefilled() {
        let f = ScriptForm::new(Some("api".into()), "create-test-user", &schema());
        let shown: Vec<String> = f.fields.iter().map(FormField::display).collect();
        assert_eq!(shown, ["", "admin", "2", "-", "-"]);
        assert_eq!(
            Value::Object(f.values()),
            serde_json::json!({"role": "admin", "count": "2"})
        );
    }

    #[test]
    fn enum_cycling_and_toggles() {
        let mut f = ScriptForm::new(None, "s", &schema());
        f.key(key(KeyCode::Tab));
        assert_eq!(f.focus, 1);
        f.key(key(KeyCode::Right));
        assert_eq!(f.fields[1].display(), "user");
        f.key(key(KeyCode::Right));
        assert_eq!(
            f.fields[1].display(),
            "admin",
            "wraps; a defaulted enum has no 'unset'"
        );
        f.key(key(KeyCode::Left));
        assert_eq!(f.fields[1].display(), "user");
        // The optional enum without default cycles through "not given".
        f.focus = 4;
        f.key(key(KeyCode::Right));
        assert_eq!(f.fields[4].display(), "gold");
        f.key(key(KeyCode::Right));
        f.key(key(KeyCode::Right));
        assert_eq!(f.fields[4].display(), "-");
        f.key(key(KeyCode::Left));
        assert_eq!(f.fields[4].display(), "silver");
        f.focus = 3;
        f.key(key(KeyCode::Char(' ')));
        assert_eq!(f.fields[3].display(), "true");
        f.key(key(KeyCode::Char(' ')));
        assert_eq!(f.fields[3].display(), "false");
        f.key(key(KeyCode::BackTab));
        assert_eq!(f.focus, 2);
        f.key(key(KeyCode::Up));
        f.key(key(KeyCode::Up));
        f.key(key(KeyCode::Up));
        assert_eq!(f.focus, 4, "wraps backwards");
    }

    #[test]
    fn required_empty_is_an_inline_error_and_nothing_is_submitted() {
        let mut f = ScriptForm::new(None, "s", &schema());
        f.focus = 2;
        assert_eq!(f.key(key(KeyCode::Enter)), FormAction::Handled);
        assert_eq!(f.focus, 0, "focus moves to the bad field");
        let err = f.fields[0].error.clone().unwrap();
        assert!(err.contains("required"), "{err}");
        typed(&mut f, "a@b.c");
        assert!(f.fields[0].error.is_none(), "typing clears the error");
        match f.key(key(KeyCode::Enter)) {
            FormAction::Submit(v) => assert_eq!(
                Value::Object(v),
                serde_json::json!({"email": "a@b.c", "role": "admin", "count": "2"})
            ),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn type_errors_are_inline() {
        let mut f = ScriptForm::new(None, "s", &schema());
        typed(&mut f, "x");
        f.focus = 2;
        f.key(key(KeyCode::Backspace));
        typed(&mut f, "many");
        assert_eq!(f.key(key(KeyCode::Enter)), FormAction::Handled);
        assert_eq!(f.focus, 2);
        assert!(f.fields[2].error.as_deref().unwrap().contains("many"));
        assert_eq!(f.key(key(KeyCode::Esc)), FormAction::Cancel);
    }
}
