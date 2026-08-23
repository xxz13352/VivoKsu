use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

use crate::api_client::{CloudflareClient, CloudflareError};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct VersionCheckResponse {
    pub latest: Option<String>,
    pub min: Option<String>,
    pub download_url: Option<String>,
    // The WPF client tolerates an omitted update/force flag (defaulting it to
    // false) while still surfacing `latest`/`min`/`download_url`; a missing
    // boolean must not discard the whole response.
    #[serde(default)]
    pub update_required: bool,
    #[serde(default)]
    pub force_update: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct VersionCheckResult {
    pub latest: Option<String>,
    pub min_version: Option<String>,
    pub download_url: Option<String>,
    pub update_required: bool,
    pub force_update: bool,
}

impl VersionCheckResult {
    pub const ALLOW_ALL: Self = Self {
        latest: None,
        min_version: None,
        download_url: None,
        update_required: false,
        force_update: false,
    };

    pub fn from_response(value: VersionCheckResponse) -> Self {
        Self {
            latest: value.latest,
            min_version: value.min,
            download_url: value.download_url,
            update_required: value.update_required,
            force_update: value.force_update,
        }
    }
}

#[derive(Debug, Clone)]
pub struct VersionClient {
    client: CloudflareClient,
    session_result: Arc<Mutex<Option<VersionCheckResult>>>,
}

impl VersionClient {
    pub fn new(base_url: impl Into<String>, app_version: impl Into<String>) -> Self {
        Self {
            client: CloudflareClient::new_injected(base_url, app_version),
            session_result: Arc::new(Mutex::new(None)),
        }
    }

    pub fn with_client(client: CloudflareClient) -> Self {
        Self {
            client,
            session_result: Arc::new(Mutex::new(None)),
        }
    }

    pub async fn check(&self) -> VersionCheckResult {
        let mut cached = self.session_result.lock().await;
        if let Some(result) = cached.as_ref() {
            return result.clone();
        }

        let result = match self.client.check_version_policy().await {
            Ok(response) => VersionCheckResult::from_response(response),
            Err(CloudflareError::Integrity(_)) => VersionCheckResult {
                latest: None,
                min_version: None,
                download_url: None,
                update_required: true,
                force_update: true,
            },
            Err(CloudflareError::UpdateRequired(update)) => VersionCheckResult {
                latest: update.latest,
                min_version: update.min_version,
                download_url: update.download_url,
                update_required: true,
                force_update: true,
            },
            Err(CloudflareError::InvalidInput(_))
            | Err(CloudflareError::Transport(_))
            | Err(CloudflareError::ApiError { .. })
            | Err(CloudflareError::InvalidResponse(_)) => VersionCheckResult::ALLOW_ALL,
        };
        *cached = Some(result.clone());
        result
    }
}

impl Default for VersionClient {
    fn default() -> Self {
        let client = CloudflareClient::new_default()
            .unwrap_or_else(|_| panic!("pinned API client initialization failed closed"));
        Self::with_client(client)
    }
}
