use std::fmt;

use tower_lsp::lsp_types::Range;

use crate::token::Token;

// Errors that happen during compilation.
// We will want to report these along with a location in the source code.
#[derive(Debug)]
pub struct Error {
    // The range of tokens the error occurred at.
    first_token: Token,
    last_token: Token,

    message: String,

    // When you try to import a module that itself had a compilation error, that is a "secondary error".
    // We may or may not want to report these.
    // If the primary location is visible, there's no point in also reporting the secondary.
    // But if the primary location is inaccessible, we should report it at the secondary location.
    pub secondary: bool,
}

fn fmt_line_part(f: &mut fmt::Formatter, text: &str, line: &str, index: usize) -> fmt::Result {
    write!(f, "{}\n", line)?;
    for (i, _) in line.char_indices() {
        if i < index {
            write!(f, " ")?;
        } else if i < index + text.len() {
            write!(f, "^")?;
        }
    }
    if index >= line.len() {
        // The token is the final newline.
        write!(f, "^")?;
    }
    write!(f, "\n")
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}:\n", self.message)?;
        fmt_line_part(
            f,
            &self.first_token.text(),
            &self.first_token.line,
            self.first_token.start as usize,
        )
    }
}

impl Error {
    pub fn old(token: &Token, message: &str) -> Self {
        Error {
            message: message.to_string(),
            first_token: token.clone(),
            last_token: token.clone(),
            secondary: false,
        }
    }

    pub fn new(first_token: &Token, last_token: &Token, message: &str) -> Self {
        Error {
            first_token: first_token.clone(),
            last_token: last_token.clone(),
            message: message.to_string(),
            secondary: false,
        }
    }

    pub fn secondary(first_token: &Token, last_token: &Token, message: &str) -> Self {
        Error {
            first_token: first_token.clone(),
            last_token: last_token.clone(),
            message: message.to_string(),
            secondary: true,
        }
    }

    pub fn range(&self) -> Range {
        Range::new(self.first_token.range().start, self.last_token.range().end)
    }
}

pub type Result<T> = std::result::Result<T, Error>;

pub trait ErrorSource {
    fn error(&self, message: &str) -> Error;
}
