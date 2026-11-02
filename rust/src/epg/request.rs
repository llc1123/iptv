use std::future::Future;
use std::time::Duration;

use reqwest::{Client, RequestBuilder, Response};

use super::client::{EpgError, EpgErrorStage};

pub(crate) const MAX_ATTEMPTS: u8 = 2;
pub(crate) const RETRY_BACKOFF: Duration = Duration::from_millis(100);

pub(crate) async fn send_with_client_retry<F, Fut, S>(
    client: &Client,
    build: F,
    mut sleep: S,
    stage: EpgErrorStage,
) -> Result<Response, EpgError>
where
    F: Fn(&Client) -> RequestBuilder,
    Fut: Future<Output = ()>,
    S: FnMut(Duration) -> Fut,
{
    let mut attempt = 1;
    loop {
        let response = build(client).send().await;
        match response {
            Ok(response) => return Ok(response),
            Err(error) if error.is_connect() && attempt < MAX_ATTEMPTS => {
                sleep(RETRY_BACKOFF).await;
                attempt += 1;
            }
            Err(error) => {
                return Err(EpgError::transport(stage, attempt, error.is_timeout()));
            }
        }
    }
}
