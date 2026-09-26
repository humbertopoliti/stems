//! `stems outputs [stem] [--reveal]` (deliverable 26, FR-ST-6,
//! `docs/config.md` → Outputs): the values stems published via `outputs:`.
//!
//! JSON: `data = OutputsResult { stems: [{name, outputs: [{name, value,
//! secret}]}] }`; `value` is `null` until the stem is healthy and
//! `"<redacted>"` for a secret. `--reveal` shows secrets only in human mode
//! with stdout on a terminal; JSON output never reveals them. Human: one row
//! per output, `STEM OUTPUT VALUE`.

use std::io::IsTerminal;

use stems_api::{Method, OutputsParams, OutputsResult};
use stems_core::Errors;

use crate::cli::OutputsArgs;
use crate::client::{self, block_on};
use crate::commands::Ctx;
use crate::output::{CommandOutput, Mode};

/// The human table.
pub fn human(r: &OutputsResult) -> String {
    let mut rows: Vec<[String; 3]> = vec![["STEM", "OUTPUT", "VALUE"].map(str::to_string)];
    for s in &r.stems {
        for o in &s.outputs {
            let value = o
                .value
                .clone()
                .unwrap_or_else(|| "- (not evaluated: the stem is not healthy)".into());
            rows.push([s.name.clone(), o.name.clone(), value]);
        }
    }
    if rows.len() == 1 {
        return "no stem declares outputs\n".into();
    }
    let widths: Vec<usize> = (0..3)
        .map(|i| rows.iter().map(|r| r[i].chars().count()).max().unwrap_or(0))
        .collect();
    let mut out = String::new();
    for row in &rows {
        let line: Vec<String> = row
            .iter()
            .enumerate()
            .map(|(i, c)| format!("{c:<w$}", w = widths[i]))
            .collect();
        out.push_str(line.join("  ").trim_end());
        out.push('\n');
    }
    out
}

/// `stems outputs`.
pub fn run(ctx: &Ctx, args: &OutputsArgs, mode: Mode) -> CommandOutput {
    let reveal = args.reveal && mode == Mode::Human && std::io::stdout().is_terminal();
    let redacted = OutputsParams {
        stems: args.stem.iter().cloned().collect(),
        reveal: false,
    };
    block_on(async {
        let c = client::connect(ctx).await?;
        // The envelope's `data` is always the redacted result.
        let r: OutputsResult = c.call(Method::OUTPUTS, &redacted).await?;
        let shown = if reveal {
            let p = OutputsParams {
                reveal: true,
                ..redacted.clone()
            };
            c.call(Method::OUTPUTS, &p).await?
        } else {
            r.clone()
        };
        let data = serde_json::to_value(&r).unwrap_or_default();
        Ok::<_, Errors>(CommandOutput::data(data).with_human(human(&shown)))
    })
    .unwrap_or_else(CommandOutput::failed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use stems_api::{OutputValue, StemOutputs};

    #[test]
    fn human_table() {
        let r = OutputsResult {
            stems: vec![
                StemOutputs {
                    name: "api".into(),
                    outputs: vec![
                        OutputValue {
                            name: "API_URL".into(),
                            value: Some("http://localhost:18080".into()),
                            secret: false,
                        },
                        OutputValue {
                            name: "TOKEN".into(),
                            value: Some("<redacted>".into()),
                            secret: true,
                        },
                    ],
                },
                StemOutputs {
                    name: "db".into(),
                    outputs: vec![OutputValue {
                        name: "URL".into(),
                        value: None,
                        secret: false,
                    }],
                },
            ],
        };
        insta::assert_snapshot!(human(&r), @r"
        STEM  OUTPUT   VALUE
        api   API_URL  http://localhost:18080
        api   TOKEN    <redacted>
        db    URL      - (not evaluated: the stem is not healthy)
        ");
        assert_eq!(
            human(&OutputsResult::default()),
            "no stem declares outputs\n"
        );
    }
}
