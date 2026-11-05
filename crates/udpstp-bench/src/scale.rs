//! The Server-scale Round: planning and judging independent of host I/O.
//! Independent reference fixtures check the plans, reports and judgments.

pub mod commands;
pub mod hosts;
pub mod judge;
pub mod matrix;
pub mod report;
pub mod round;
pub mod rules;
pub mod schedule;

use std::fmt;

use bench_rounds::py::{self, Json};

/// Match the reference's optional JSON-file reads. An input Python can
/// read but this reader cannot remains an error, never missing evidence.
fn evidence_json(bytes: Option<&[u8]>, path: &str) -> Result<Json, Error> {
    let Some(bytes) = bytes else {
        return Ok(Json::Null);
    };
    let Ok(source) = std::str::from_utf8(bytes) else {
        return Ok(Json::Null);
    };
    match Json::parse(source) {
        Ok(value) => Ok(value),
        Err(_) if py::python_also_refuses(source) => Ok(Json::Null),
        Err(error) => Err(Error(format!("{path}: {error}"))),
    }
}

/// A matrix or recorded Round that cannot be read as the reference reads it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Error(pub(crate) String);

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for Error {}

impl From<bench_rounds::py::PyError> for Error {
    fn from(error: bench_rounds::py::PyError) -> Self {
        Self(error.to_string())
    }
}
