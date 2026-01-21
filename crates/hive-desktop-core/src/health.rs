use std::time::Duration;

use tokio_postgres::NoTls;

use crate::errors::{DesktopError, Result};

#[derive(Clone, Debug)]
pub struct PostgresPreflight {
    pub host: String,
    pub port: u16,
    pub user: String,
    pub database: String,
    pub password: String,
}

#[derive(Clone, Debug)]
pub struct S3Preflight {
    pub endpoint_url: String,
}

pub async fn run_preflight(
    postgres: &PostgresPreflight,
    s3: &S3Preflight,
    timeout: Duration,
) -> Result<()> {
    check_postgres(postgres, timeout).await?;
    check_s3_endpoint(&s3.endpoint_url, timeout).await?;
    Ok(())
}

async fn check_postgres(config: &PostgresPreflight, timeout: Duration) -> Result<()> {
    let mut cfg = tokio_postgres::Config::new();
    cfg.host(&config.host)
        .port(config.port)
        .user(&config.user)
        .dbname(&config.database)
        .password(&config.password);

    let (client, connection) = tokio::time::timeout(timeout, cfg.connect(NoTls))
        .await
        .map_err(|_| DesktopError::Timeout("connecting to postgres"))??;

    tokio::spawn(async move {
        let _ = connection.await;
    });

    tokio::time::timeout(timeout, client.simple_query("SELECT 1"))
        .await
        .map_err(|_| DesktopError::Timeout("querying postgres"))??;
    Ok(())
}

async fn check_s3_endpoint(url: &str, timeout: Duration) -> Result<()> {
    let client = reqwest::Client::builder()
        .timeout(timeout)
        .build()
        .map_err(|err| DesktopError::Preflight(format!("http client init failed: {err}")))?;
    let response = client
        .get(url)
        .send()
        .await
        .map_err(|err| DesktopError::Preflight(format!("s3 endpoint unreachable: {err}")))?;
    if response.status().is_server_error() {
        return Err(DesktopError::Preflight(format!(
            "s3 endpoint error: {}",
            response.status()
        )));
    }
    Ok(())
}
