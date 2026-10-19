//! Error types for the yumo library.
//!
//! Mirrors the original C++ `yumo::exception` / `exception_ex` design,
//! expressed as idiomatic Rust `Result` values.

use std::fmt;

/// Categories of errors that can occur inside the library.
///
/// Matches `yumo::exception::type` from the original C++ header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExceptionType {
    // File operations
    FileNotFound,
    FileOpenError,
    FileReadError,
    FileWriteError,
    FileCloseError,
    FileCorrupted,
    FileError,

    // Parameter / state errors
    InvalidInput,
    InvalidFormat,
    InvalidID,
    InvalidData,
    InvalidState,

    // Audio playback & decoding errors
    PlaybackError,
    DecodeError,

    // Memory errors
    OutOfMemory,
    MemoryError,

    // Uncategorised
    CustomizedError,
    UnknownError,
}

impl fmt::Display for ExceptionType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            ExceptionType::FileNotFound => "file not found",
            ExceptionType::FileOpenError => "file open error",
            ExceptionType::FileReadError => "file read error",
            ExceptionType::FileWriteError => "file write error",
            ExceptionType::FileCloseError => "file close error",
            ExceptionType::FileCorrupted => "file corrupted",
            ExceptionType::FileError => "file error",
            ExceptionType::InvalidInput => "invalid input",
            ExceptionType::InvalidFormat => "invalid format",
            ExceptionType::InvalidID => "invalid id",
            ExceptionType::InvalidData => "invalid data",
            ExceptionType::InvalidState => "invalid state",
            ExceptionType::PlaybackError => "playback error",
            ExceptionType::DecodeError => "decode error",
            ExceptionType::OutOfMemory => "out of memory",
            ExceptionType::MemoryError => "memory error",
            ExceptionType::CustomizedError => "customized error",
            ExceptionType::UnknownError => "unknown error",
        };
        f.write_str(s)
    }
}

/// The library-level error type. Carries a category and an optional message.
#[derive(Debug, Clone)]
pub struct Exception {
    pub kind: ExceptionType,
    pub message: String,
}

impl Exception {
    pub fn new(kind: ExceptionType, message: impl Into<String>) -> Self {
        Self { kind, message: message.into() }
    }

    pub fn kind(&self) -> ExceptionType { self.kind }
    pub fn what(&self) -> &str { &self.message }
}

impl fmt::Display for Exception {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[{}] {}", self.kind, self.message)
    }
}

impl std::error::Error for Exception {}

impl From<std::io::Error> for Exception {
    fn from(e: std::io::Error) -> Self {
        let kind = if e.kind() == std::io::ErrorKind::NotFound {
            ExceptionType::FileNotFound
        } else {
            ExceptionType::FileReadError
        };
        Exception::new(kind, e.to_string())
    }
}

/// Convenience type alias used throughout the crate.
pub type Result<T> = std::result::Result<T, Exception>;
