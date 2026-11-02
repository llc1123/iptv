use std::time::Duration;

use reqwest::{Client, RequestBuilder, Response};
use tokio::sync::Semaphore;

use super::client::{EpgError, EpgErrorStage};
use super::session::Sleeper;

pub(crate) const MAX_ATTEMPTS: u8 = 2;
pub(crate) const RETRY_BACKOFF: Duration = Duration::from_millis(100);

pub(crate) async fn send_with_client_retry<F>(
    client: &Client,
    build: F,
    sleeper: &dyn Sleeper,
    stage: EpgErrorStage,
    permits: std::sync::Arc<Semaphore>,
    pool_timeout: Duration,
    write_timeout: Duration,
) -> Result<Response, EpgError>
where
    F: Fn(&Client) -> RequestBuilder,
{
    let mut attempt = 1;
    loop {
        let permit = tokio::time::timeout(pool_timeout, permits.clone().acquire_owned())
            .await
            .map_err(|_| EpgError::pool_timeout(stage, attempt))?
            .map_err(|_| EpgError::pool_timeout(stage, attempt))?;
        let response = tokio::time::timeout(write_timeout, build(client).send()).await;
        drop(permit);
        match response {
            Ok(Ok(response)) => return Ok(response),
            Ok(Err(error)) if error.is_connect() && attempt < MAX_ATTEMPTS => {
                sleeper.sleep(RETRY_BACKOFF).await;
                attempt += 1;
            }
            Ok(Err(error)) => {
                return Err(EpgError::transport(stage, attempt, error.is_timeout()));
            }
            Err(_) => return Err(EpgError::timeout(stage, attempt)),
        }
    }
}
