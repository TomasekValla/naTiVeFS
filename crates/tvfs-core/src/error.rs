pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("network error: {0}")]
    Network(#[from] reqwest::Error),
    #[error("wrong username or password")]
    BadCredentials,
    #[error("not logged in (session expired or missing)")]
    Unauthorized,
    #[error("too many requests, try again later")]
    RateLimited,
    #[error("server error {status}: {message}")]
    Server { status: u16, message: String },
    #[error("invalid url: {0}")]
    Url(#[from] url::ParseError),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("credential store error: {0}")]
    Keyring(String),
    #[error("config error: {0}")]
    Config(String),
    #[error("update error: {0}")]
    Update(String),
}

impl Error {
    /// True when the caller should send the user back to the login screen.
    pub fn is_auth(&self) -> bool {
        matches!(self, Error::BadCredentials | Error::Unauthorized)
    }
}
