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
            client: CloudflareClient::new_injected(base_url, app_version),
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
        let client = CloudflareClient::new_default()
            .unwrap_or_else(|_| panic!("pinned API client initialization failed closed"));
        Self::with_client(client)
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
