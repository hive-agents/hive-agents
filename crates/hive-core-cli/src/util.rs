use std::collections::VecDeque;
use std::error::Error;
use std::fmt;

#[derive(Debug)]
pub struct CliError(pub String);

impl fmt::Display for CliError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl Error for CliError {}

pub type Result<T> = std::result::Result<T, CliError>;

pub fn err(message: impl Into<String>) -> CliError {
    CliError(message.into())
}

pub fn take_value(args: &mut VecDeque<String>, flag: &str) -> Result<String> {
    args.pop_front()
        .ok_or_else(|| err(format!("missing value for {}", flag)))
}
