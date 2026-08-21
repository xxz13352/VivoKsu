use crate::api_client::{CloudflareClient, CloudflareError, CloudflareResult, LoginResult};

#[derive(Debug, Clone)]
pub struct AuthService {
    client: CloudflareClient,
}

#[derive(Debug, Clone)]
pub struct AuthSession {
    pub token: String,
    pub username: String,
    pub name: String,
}

impl AuthService {
    pub fn new(base_url: impl Into<String>, app_version: impl Into<String>) -> Self {
        Self {
            client: CloudflareClient::new(base_url, app_version),
        }
    }

    pub fn with_client(client: CloudflareClient) -> Self {
        Self { client }
    }

    pub async fn login(&self, username: &str, password: &str) -> CloudflareResult<AuthSession> {
        let result = self.client.login(username, password).await?;

        Ok(AuthSession {
            token: result.token,
            username: result.username,
            name: result.name,
        })
    }

    pub async fn validate_token(&self, token: &str) -> CloudflareResult<Option<String>> {
        match self.client.validate_token(token).await {
            Ok(name) => Ok(name),
            Err(CloudflareError::UpdateRequired(info)) => {
                Err(CloudflareError::UpdateRequired(info))
            }
            Err(_) => Ok(None),
        }
    }

    pub fn default_client() -> Self {
        Self::new(
            crate::api_client::DEFAULT_BASE_URL,
            crate::api_client::DEFAULT_APP_VERSION,
        )
    }

    pub fn client(&self) -> &CloudflareClient {
        &self.client
    }
}

impl AuthSession {
    pub fn from_login(result: LoginResult) -> Self {
        Self {
            token: result.token,
            username: result.username,
            name: result.name,
        }
    }
}
