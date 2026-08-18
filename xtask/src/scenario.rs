//! The scenario format the screenshot harness drives.
//!
//! Interaction tests are data, not code: a scenario is a text file of one step
//! per line, committed alongside the app, so "what was clicked to produce this
//! screenshot" is reviewable and repeatable rather than a shell incantation that
//! lived in one session's scrollback.
//!
//! ```text
//! # Open a request and look at the response card.
//! shot 01-welcome
//! click 640 520          # window-relative; the driver adds the window origin
//! wait 400
//! type Arm home
//! key Return
//! shot 02-request
//! ```

use std::str::FromStr;
use std::time::Duration;

use anyhow::{Context as _, Result, anyhow, bail};

/// One interaction, or one capture.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    /// Move the pointer to a window-relative position.
    Move { x: i32, y: i32 },
    /// Move and click the primary button.
    Click { x: i32, y: i32 },
    /// Type literal text.
    Type(String),
    /// Press a named key, in `xdotool` spelling: `Return`, `ctrl+s`, `Escape`.
    Key(String),
    /// Scroll by `amount` wheel clicks; negative scrolls up.
    Scroll(i32),
    /// Wait, to let animations and async work settle.
    Wait(Duration),
    /// Capture the window to `<name>.png`.
    Shot(String),
}

/// A parsed scenario, with the name it was loaded under.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Scenario {
    pub name: String,
    pub steps: Vec<Step>,
}

impl Scenario {
    pub fn parse(name: impl Into<String>, source: &str) -> Result<Self> {
        let mut steps = Vec::new();
        for (number, line) in source.lines().enumerate() {
            let line = strip_comment(line).trim();
            if line.is_empty() {
                continue;
            }
            steps.push(
                Step::from_str(line).with_context(|| format!("line {}: {line}", number + 1))?,
            );
        }
        if steps.is_empty() {
            bail!("a scenario needs at least one step");
        }
        Ok(Self {
            name: name.into(),
            steps,
        })
    }

    /// Names of the screenshots this scenario will produce, in order.
    pub fn shots(&self) -> Vec<&str> {
        self.steps
            .iter()
            .filter_map(|step| match step {
                Step::Shot(name) => Some(name.as_str()),
                _ => None,
            })
            .collect()
    }
}

/// A `#` starts a comment, except inside a `type` step's text — typing a literal
/// `#` is legitimate, and dropping it silently would be a confusing bug.
fn strip_comment(line: &str) -> &str {
    if line.trim_start().starts_with("type ") {
        return line;
    }
    match line.find('#') {
        Some(at) => &line[..at],
        None => line,
    }
}

impl FromStr for Step {
    type Err = anyhow::Error;

    fn from_str(line: &str) -> Result<Self> {
        let (verb, rest) = match line.split_once(char::is_whitespace) {
            Some((verb, rest)) => (verb, rest.trim()),
            None => (line, ""),
        };

        match verb {
            "move" | "click" => {
                let (x, y) = coordinates(rest)?;
                Ok(if verb == "click" {
                    Step::Click { x, y }
                } else {
                    Step::Move { x, y }
                })
            }
            "type" => {
                if rest.is_empty() {
                    bail!("`type` needs some text");
                }
                Ok(Step::Type(rest.to_string()))
            }
            "key" => {
                if rest.is_empty() {
                    bail!("`key` needs a key name, such as `Return` or `ctrl+s`");
                }
                Ok(Step::Key(rest.to_string()))
            }
            "scroll" => Ok(Step::Scroll(
                rest.parse().context("`scroll` needs a whole number")?,
            )),
            "wait" => Ok(Step::Wait(Duration::from_millis(
                rest.parse().context("`wait` needs milliseconds")?,
            ))),
            "shot" => {
                if rest.is_empty() || !rest.chars().all(is_shot_char) {
                    bail!("`shot` needs a filename of letters, digits, `-` or `_`");
                }
                Ok(Step::Shot(rest.to_string()))
            }
            other => Err(anyhow!("unknown step `{other}`")),
        }
    }
}

fn is_shot_char(character: char) -> bool {
    character.is_ascii_alphanumeric() || character == '-' || character == '_'
}

fn coordinates(rest: &str) -> Result<(i32, i32)> {
    let mut parts = rest.split_whitespace();
    let x = parts
        .next()
        .context("expected an x coordinate")?
        .parse()
        .context("x is not a whole number")?;
    let y = parts
        .next()
        .context("expected a y coordinate")?
        .parse()
        .context("y is not a whole number")?;
    if parts.next().is_some() {
        bail!("expected exactly two coordinates");
    }
    Ok((x, y))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_every_verb() {
        let scenario = Scenario::parse(
            "example",
            "move 1 2\nclick 3 4\ntype hello\nkey ctrl+s\nscroll -3\nwait 250\nshot 01-start\n",
        )
        .expect("parses");

        assert_eq!(
            scenario.steps,
            vec![
                Step::Move { x: 1, y: 2 },
                Step::Click { x: 3, y: 4 },
                Step::Type("hello".into()),
                Step::Key("ctrl+s".into()),
                Step::Scroll(-3),
                Step::Wait(Duration::from_millis(250)),
                Step::Shot("01-start".into()),
            ]
        );
    }

    #[test]
    fn blank_lines_and_comments_are_ignored() {
        let scenario = Scenario::parse("example", "# a comment\n\n  \nshot only # trailing\n")
            .expect("parses");
        assert_eq!(scenario.steps, vec![Step::Shot("only".into())]);
    }

    #[test]
    fn a_hash_inside_typed_text_is_kept() {
        let scenario = Scenario::parse("example", "type /topic#1\n").expect("parses");
        assert_eq!(scenario.steps, vec![Step::Type("/topic#1".into())]);
    }

    #[test]
    fn typed_text_keeps_its_spaces() {
        let scenario = Scenario::parse("example", "type Arm home pose\n").expect("parses");
        assert_eq!(scenario.steps, vec![Step::Type("Arm home pose".into())]);
    }

    #[test]
    fn shots_are_listed_in_order() {
        let scenario =
            Scenario::parse("example", "shot one\nclick 1 1\nshot two\n").expect("parses");
        assert_eq!(scenario.shots(), vec!["one", "two"]);
    }

    #[test]
    fn an_empty_scenario_is_rejected() {
        let error = Scenario::parse("example", "# nothing but comments\n")
            .expect_err("an empty scenario is useless");
        assert!(error.to_string().contains("at least one step"));
    }

    #[test]
    fn malformed_steps_are_errors() {
        for source in [
            "click 1\n",
            "click 1 2 3\n",
            "click a b\n",
            "wait soon\n",
            "shot ../escape\n",
            "frobnicate\n",
            "type\n",
            "key\n",
        ] {
            assert!(
                Scenario::parse("example", source).is_err(),
                "{source:?} should be rejected"
            );
        }
    }

    #[test]
    fn errors_carry_the_line_number() {
        let error = Scenario::parse("example", "shot fine\nclick nowhere\n")
            .expect_err("the second line is bad");
        assert!(
            format!("{error:#}").contains("line 2"),
            "error should name the line: {error:#}"
        );
    }
}
