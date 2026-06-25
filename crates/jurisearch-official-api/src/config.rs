//! OfficialApiConfig + PisteEnvironment, credential constants, and env resolution helpers.

use crate::*;

pub(crate) const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

pub(crate) const READ_TIMEOUT: Duration = Duration::from_secs(30);

pub(crate) const LEGIFRANCE_TOKEN_SKEW: Duration = Duration::from_secs(30);

pub(crate) const UPSTREAM_BODY_LIMIT: usize = 500;

pub(crate) const PROD_JUDILIBRE_CREDENTIALS: &[&str] = &["JURISEARCH_PISTE_JUDILIBRE_KEY_ID", "PISTE_API_KEY"];

pub(crate) const SANDBOX_JUDILIBRE_CREDENTIALS: &[&str] =
    &["JURISEARCH_PISTE_JUDILIBRE_KEY_ID", "PISTE_SANDBOX_API_KEY"];

pub(crate) const PROD_LEGIFRANCE_CLIENT_ID_CREDENTIALS: &[&str] = &[
    "JURISEARCH_PISTE_LEGIFRANCE_CLIENT_ID",
    "PISTE_OAUTH_CLIENT_ID",
];

pub(crate) const SANDBOX_LEGIFRANCE_CLIENT_ID_CREDENTIALS: &[&str] = &[
    "JURISEARCH_PISTE_LEGIFRANCE_CLIENT_ID",
    "PISTE_SANDBOX_OAUTH_CLIENT_ID",
];

pub(crate) const PROD_LEGIFRANCE_CLIENT_SECRET_CREDENTIALS: &[&str] = &[
    "JURISEARCH_PISTE_LEGIFRANCE_CLIENT_SECRET",
    "PISTE_OAUTH_CLIENT_SECRET",
];

pub(crate) const SANDBOX_LEGIFRANCE_CLIENT_SECRET_CREDENTIALS: &[&str] = &[
    "JURISEARCH_PISTE_LEGIFRANCE_CLIENT_SECRET",
    "PISTE_SANDBOX_OAUTH_CLIENT_SECRET",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PisteEnvironment {
    Production,
    Sandbox,
}

impl PisteEnvironment {
    #[must_use]
    pub fn api_base_url(self) -> &'static str {
        match self {
            Self::Production => "https://api.piste.gouv.fr",
            Self::Sandbox => "https://sandbox-api.piste.gouv.fr",
        }
    }

    #[must_use]
    pub fn oauth_base_url(self) -> &'static str {
        match self {
            Self::Production => "https://oauth.piste.gouv.fr",
            Self::Sandbox => "https://sandbox-oauth.piste.gouv.fr",
        }
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct OfficialApiConfig {
    pub environment: PisteEnvironment,
    pub api_base_url: String,
    pub oauth_base_url: String,
    pub judilibre_key_id: Option<String>,
    pub legifrance_client_id: Option<String>,
    pub legifrance_client_secret: Option<String>,
}

impl fmt::Debug for OfficialApiConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OfficialApiConfig")
            .field("environment", &self.environment)
            .field("api_base_url", &self.api_base_url)
            .field("oauth_base_url", &self.oauth_base_url)
            .field(
                "judilibre_key_id",
                &self.judilibre_key_id.as_ref().map(|_| "<redacted>"),
            )
            .field(
                "legifrance_client_id",
                &self.legifrance_client_id.as_ref().map(|_| "<redacted>"),
            )
            .field(
                "legifrance_client_secret",
                &self.legifrance_client_secret.as_ref().map(|_| "<redacted>"),
            )
            .finish()
    }
}

impl OfficialApiConfig {
    #[must_use]
    pub fn production() -> Self {
        Self::for_environment(PisteEnvironment::Production)
    }

    #[must_use]
    pub fn sandbox() -> Self {
        Self::for_environment(PisteEnvironment::Sandbox)
    }

    #[must_use]
    pub fn for_environment(environment: PisteEnvironment) -> Self {
        Self {
            environment,
            api_base_url: environment.api_base_url().to_owned(),
            oauth_base_url: environment.oauth_base_url().to_owned(),
            judilibre_key_id: None,
            legifrance_client_id: None,
            legifrance_client_secret: None,
        }
    }

    #[must_use]
    pub fn from_env() -> Self {
        let environment = match env::var("JURISEARCH_PISTE_ENV")
            .or_else(|_| env::var("PISTE_ENV"))
            .unwrap_or_else(|_| "production".to_owned())
            .to_ascii_lowercase()
            .as_str()
        {
            "sandbox" => PisteEnvironment::Sandbox,
            _ => PisteEnvironment::Production,
        };
        let mut config = Self::for_environment(environment);
        config.api_base_url =
            nonempty_env_or_default("JURISEARCH_PISTE_API_BASE_URL", config.api_base_url);
        config.oauth_base_url =
            nonempty_env_or_default("JURISEARCH_PISTE_OAUTH_BASE_URL", config.oauth_base_url);
        config.judilibre_key_id = first_nonempty_env(if environment == PisteEnvironment::Sandbox {
            SANDBOX_JUDILIBRE_CREDENTIALS
        } else {
            PROD_JUDILIBRE_CREDENTIALS
        });
        config.legifrance_client_id =
            first_nonempty_env(if environment == PisteEnvironment::Sandbox {
                SANDBOX_LEGIFRANCE_CLIENT_ID_CREDENTIALS
            } else {
                PROD_LEGIFRANCE_CLIENT_ID_CREDENTIALS
            });
        config.legifrance_client_secret =
            first_nonempty_env(if environment == PisteEnvironment::Sandbox {
                SANDBOX_LEGIFRANCE_CLIENT_SECRET_CREDENTIALS
            } else {
                PROD_LEGIFRANCE_CLIENT_SECRET_CREDENTIALS
            });
        config
    }
}

pub(crate) fn first_nonempty_env(names: &[&str]) -> Option<String> {
    names
        .iter()
        .find_map(|name| env::var(name).ok().filter(|value| !value.trim().is_empty()))
}

pub(crate) fn nonempty_env_or_default(name: &str, default: String) -> String {
    env::var(name)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or(default)
}

pub(crate) fn judilibre_credential_names(environment: PisteEnvironment) -> &'static [&'static str] {
    match environment {
        PisteEnvironment::Production => PROD_JUDILIBRE_CREDENTIALS,
        PisteEnvironment::Sandbox => SANDBOX_JUDILIBRE_CREDENTIALS,
    }
}

pub(crate) fn legifrance_client_id_credential_names(environment: PisteEnvironment) -> &'static [&'static str] {
    match environment {
        PisteEnvironment::Production => PROD_LEGIFRANCE_CLIENT_ID_CREDENTIALS,
        PisteEnvironment::Sandbox => SANDBOX_LEGIFRANCE_CLIENT_ID_CREDENTIALS,
    }
}

pub(crate) fn legifrance_client_secret_credential_names(
    environment: PisteEnvironment,
) -> &'static [&'static str] {
    match environment {
        PisteEnvironment::Production => PROD_LEGIFRANCE_CLIENT_SECRET_CREDENTIALS,
        PisteEnvironment::Sandbox => SANDBOX_LEGIFRANCE_CLIENT_SECRET_CREDENTIALS,
    }
}
