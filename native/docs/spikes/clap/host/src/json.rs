//! Just enough JSON output for the probe's reports.

use std::fmt;

pub enum Json {
    Null,
    Bool(bool),
    Num(String),
    Str(String),
    Arr(Vec<Json>),
    Obj(Vec<(String, Json)>),
}

impl Json {
    pub fn obj<const N: usize>(fields: [(&str, Json); N]) -> Self {
        Self::Obj(fields.into_iter().map(|(k, v)| (k.to_owned(), v)).collect())
    }
    pub fn str(s: impl Into<String>) -> Self {
        Self::Str(s.into())
    }
    pub fn num(n: impl Into<u64>) -> Self {
        Self::Num(n.into().to_string())
    }
    pub fn f(x: f64) -> Self {
        if x.is_finite() {
            Self::Num(format!("{x:?}"))
        } else {
            Self::Null
        }
    }
    pub fn opt_f(x: Option<f64>) -> Self {
        x.map_or(Self::Null, Self::f)
    }
    pub fn opt_num(n: Option<impl Into<u64>>) -> Self {
        n.map_or(Self::Null, Self::num)
    }
    pub fn opt_bool(b: Option<bool>) -> Self {
        b.map_or(Self::Null, Self::Bool)
    }
}

impl fmt::Display for Json {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Null => f.write_str("null"),
            Self::Bool(b) => write!(f, "{b}"),
            Self::Num(n) => f.write_str(n),
            Self::Str(s) => {
                f.write_str("\"")?;
                for c in s.chars() {
                    match c {
                        '"' => f.write_str("\\\"")?,
                        '\\' => f.write_str("\\\\")?,
                        c if (c as u32) < 0x20 => write!(f, "\\u{:04x}", c as u32)?,
                        c => write!(f, "{c}")?,
                    }
                }
                f.write_str("\"")
            }
            Self::Arr(items) => {
                f.write_str("[")?;
                for (i, v) in items.iter().enumerate() {
                    if i > 0 {
                        f.write_str(",")?;
                    }
                    write!(f, "{v}")?;
                }
                f.write_str("]")
            }
            Self::Obj(fields) => {
                f.write_str("{")?;
                for (i, (k, v)) in fields.iter().enumerate() {
                    if i > 0 {
                        f.write_str(",")?;
                    }
                    write!(f, "{}:{v}", Json::Str(k.clone()))?;
                }
                f.write_str("}")
            }
        }
    }
}
