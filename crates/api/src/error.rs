/// Error of one API request.
#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    /// Non-2xx answer; `code` / `message` come from `{"error":{...}}`.
    #[error("{message}")]
    Http {
        status: u16,
        code: Option<String>,
        message: String,
    },
    #[error("network error: {0}")]
    Network(reqwest::Error),
    #[error("invalid response: {0}")]
    Decode(String),
    #[error("invalid URL: {0}")]
    Url(String),
}

impl From<reqwest::Error> for ApiError {
    fn from(err: reqwest::Error) -> Self {
        // Strip the request URL so query-carried credentials (mediaToken) can
        // never leak into UI notices or logs via Display/Debug.
        Self::Network(err.without_url())
    }
}

impl ApiError {
    pub fn status(&self) -> Option<u16> {
        match self {
            Self::Http { status, .. } => Some(*status),
            _ => None,
        }
    }

    pub fn code(&self) -> Option<&str> {
        match self {
            Self::Http { code, .. } => code.as_deref(),
            _ => None,
        }
    }

    /// The API token was rejected (rotated on the server): sign in again.
    pub fn is_unauthorized(&self) -> bool {
        self.status() == Some(401)
    }
}

/// Why signing in failed. The UI maps each variant to a localized message.
#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    #[error("API token and media token are both required")]
    CredentialsRequired,
    #[error("API token and media token must differ")]
    TokensMustDiffer,
    #[error("API token is invalid")]
    ApiTokenInvalid,
    #[error("unsupported server API revision {0}")]
    UnsupportedRevision(i64),
    #[error("media token is invalid")]
    MediaTokenInvalid,
    #[error(transparent)]
    Api(#[from] ApiError),
}
