//! The pedal catalog `brownnote --list` prints, parsed into typed data.
//!
//! Pure: this module never starts a process. The format is the bench's own
//! (`list_effects()` in GuitarPedal's `Validation/bench/bench.c`), which
//! `brownnote --list` passes through unchanged.
use std::fmt;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MixLaw {
    None,
    Linear,
    EqualPower,
}

impl MixLaw {
    pub fn text(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Linear => "linear",
            Self::EqualPower => "equal power",
        }
    }

    fn parse(text: &str) -> Option<Self> {
        match text {
            "none" => Some(Self::None),
            "linear" => Some(Self::Linear),
            "equal power" => Some(Self::EqualPower),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pot {
    pub label: String,
    pub max: u8,
    pub default: u8,
    pub unit: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Effect {
    pub name: String,
    pub always_runs: bool,
    pub mix: MixLaw,
    pub pots: Vec<Pot>,
}

impl Effect {
    /// The bench refuses `--route` for exactly the always-run system blocks.
    pub fn routable(&self) -> bool {
        !self.always_runs
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Catalog {
    pub effects: Vec<Effect>,
    /// The raw text, which the renderer hashes into its cache key as the
    /// pedal version.
    pub source: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParseError {
    pub line: usize,
    pub text: String,
    pub reason: String,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "pedal list line {}: {} (in {:?})",
            self.line, self.reason, self.text
        )
    }
}
impl std::error::Error for ParseError {}

impl Catalog {
    pub fn parse(text: &str) -> Result<Self, ParseError> {
        let mut effects: Vec<Effect> = Vec::new();
        for (index, line) in text.lines().enumerate() {
            let line_number = index + 1;
            if line.starts_with(' ') {
                let effect = effects.last_mut().ok_or_else(|| ParseError {
                    line: line_number,
                    text: line.to_string(),
                    reason: "pot before the first effect header".into(),
                })?;
                effect.pots.push(parse_pot(line, line_number)?);
            } else {
                effects.push(parse_header(line, line_number)?);
            }
        }
        if effects.is_empty() {
            return Err(ParseError {
                line: 1,
                text: String::new(),
                reason: "empty effect list".into(),
            });
        }
        Ok(Self {
            effects,
            source: text.to_string(),
        })
    }

    pub fn routable(&self) -> impl Iterator<Item = &Effect> {
        self.effects.iter().filter(|effect| effect.routable())
    }

    pub fn effect(&self, name: &str) -> Option<&Effect> {
        self.effects.iter().find(|effect| effect.name == name)
    }
}

fn parse_header(line: &str, line_number: usize) -> Result<Effect, ParseError> {
    let error = |reason: &str| ParseError {
        line: line_number,
        text: line.to_string(),
        reason: reason.into(),
    };
    let (head, law) = line
        .rsplit_once(" [mix: ")
        .ok_or_else(|| error("neither an effect header nor a pot"))?;
    let law = law
        .strip_suffix(']')
        .ok_or_else(|| error("effect header does not close its mix law"))?;
    let mix = MixLaw::parse(law).ok_or_else(|| error("unknown mix law"))?;
    let (name, always_runs) = match head.strip_suffix("  (always runs)") {
        Some(name) => (name, true),
        None => (head, false),
    };
    if name.is_empty() || name.trim() != name {
        return Err(error("effect name is empty or padded"));
    }
    Ok(Effect {
        name: name.to_string(),
        always_runs,
        mix,
        pots: Vec::new(),
    })
}

fn parse_pot(line: &str, line_number: usize) -> Result<Pot, ParseError> {
    let error = |reason: &str| ParseError {
        line: line_number,
        text: line.to_string(),
        reason: reason.into(),
    };
    if !line.starts_with("    ") {
        return Err(error("pot line does not start with four spaces"));
    }
    let body = &line[4..];
    let (label, rest) = body
        .split_once(" 0..")
        .ok_or_else(|| error("pot line has no 0..MAX field"))?;
    let label = label.trim_end();
    if label.is_empty() {
        return Err(error("pot has an empty label"));
    }
    let (max, rest) = parse_number(rest, line, line_number, "max")?;
    let rest = rest
        .strip_prefix(' ')
        .map(str::trim_start)
        .and_then(|rest| rest.strip_prefix("default "))
        .ok_or_else(|| error("pot line has no default"))?;
    let (default, rest) = parse_number(rest, line, line_number, "default")?;
    let max = u8::try_from(max).map_err(|_| error("max does not fit in u8"))?;
    let default = u8::try_from(default).map_err(|_| error("default does not fit in u8"))?;
    if default > max {
        return Err(error("default is above max"));
    }
    Ok(Pot {
        label: label.to_string(),
        max,
        default,
        unit: rest.trim().to_string(),
    })
}

/// Split leading ASCII digits off `text` as a number, leaving the rest.
fn parse_number<'a>(
    text: &'a str,
    line: &str,
    line_number: usize,
    field: &str,
) -> Result<(u32, &'a str), ParseError> {
    let digits = text.chars().take_while(char::is_ascii_digit).count();
    let number = text[..digits].parse::<u32>().map_err(|_| ParseError {
        line: line_number,
        text: line.to_string(),
        reason: format!("{field} is missing or is not a number"),
    })?;
    Ok((number, &text[digits..]))
}
