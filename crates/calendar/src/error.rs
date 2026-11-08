use thiserror::Error;

pub type Result<T> = std::result::Result<T, Error>;

/// Errors never contain ICS bodies, descriptions or other user content beyond short field names.
#[derive(Debug, Error)]
pub enum Error {
    #[error("input too large (max {max} bytes)")]
    TooLarge { max: usize },
    #[error("invalid input: {0}")]
    Invalid(String),
    #[error("not found: {0}")]
    NotFound(String),
    #[error("database error: {0}")]
    Db(#[from] rusqlite::Error),
}
