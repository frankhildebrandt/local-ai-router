use std::{
    net::{IpAddr, SocketAddr},
    time::Duration,
};

use anyhow::Context;
use url::Url;

use crate::protocol::{CanonicalRequest, ContentBlock};

const MAX_MEDIA_BYTES: u64 = 20 * 1024 * 1024;
const MAX_MEDIA_ITEMS: usize = 8;
const MAX_REDIRECTS: usize = 5;
const FETCH_TIMEOUT: Duration = Duration::from_secs(10);

pub async fn resolve_request_media(
    client: &reqwest::Client,
    request: &mut CanonicalRequest,
) -> anyhow::Result<()> {
    let mut count = 0usize;
    for block in request.system.iter_mut().chain(
        request
            .messages
            .iter_mut()
            .flat_map(|message| message.content.iter_mut()),
    ) {
        if let Some(url) = media_url(block) {
            if url.starts_with("data:") {
                continue;
            }
            count += 1;
            if count > MAX_MEDIA_ITEMS {
                anyhow::bail!("too many remote media attachments");
            }
            let fetched = fetch_public_media(client, url).await?;
            set_media_url(block, fetched);
        }
    }
    Ok(())
}

fn media_url(block: &ContentBlock) -> Option<&str> {
    match block {
        ContentBlock::Image { url, .. }
        | ContentBlock::Audio { url, .. }
        | ContentBlock::Video { url, .. } => Some(url.as_str()),
        _ => None,
    }
}

fn set_media_url(block: &mut ContentBlock, url: String) {
    match block {
        ContentBlock::Image { url: current, .. }
        | ContentBlock::Audio { url: current, .. }
        | ContentBlock::Video { url: current, .. } => *current = url,
        _ => {}
    }
}

pub async fn fetch_public_media(client: &reqwest::Client, url: &str) -> anyhow::Result<String> {
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(FETCH_TIMEOUT)
        .build()
        .unwrap_or_else(|_| client.clone());
    let mut current = Url::parse(url).context("media URL is invalid")?;
    if current.scheme() != "https" {
        anyhow::bail!("remote media must use HTTPS");
    }
    let mut hops = 0usize;
    loop {
        assert_public_url(&current).await?;
        let response = client.get(current.as_str()).send().await?;
        if response.status().is_redirection() {
            hops += 1;
            if hops > MAX_REDIRECTS {
                anyhow::bail!("too many media redirects");
            }
            let location = response
                .headers()
                .get(reqwest::header::LOCATION)
                .and_then(|value| value.to_str().ok())
                .context("redirect is missing a location")?;
            current = current.join(location)?;
            if current.scheme() != "https" {
                anyhow::bail!("media redirects must stay on HTTPS");
            }
            continue;
        }
        let response = response.error_for_status()?;
        let mime = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or("application/octet-stream")
            .split(';')
            .next()
            .unwrap_or("application/octet-stream")
            .to_owned();
        let bytes = response.bytes().await?;
        if bytes.len() as u64 > MAX_MEDIA_BYTES {
            anyhow::bail!("remote media exceeds the 20 MB limit");
        }
        return Ok(format!(
            "data:{mime};base64,{}",
            base64::Engine::encode(&base64::engine::general_purpose::STANDARD, bytes)
        ));
    }
}

pub async fn assert_public_url(url: &Url) -> anyhow::Result<()> {
    if url.scheme() != "https" {
        anyhow::bail!("remote media must use HTTPS");
    }
    let host = url.host_str().context("media URL has no host")?;
    if host.eq_ignore_ascii_case("localhost") || host.ends_with(".localhost") {
        anyhow::bail!("private media hosts are blocked");
    }
    let lookup = format!("{host}:443");
    let addresses: Vec<SocketAddr> = tokio::net::lookup_host(&lookup).await?.collect();
    if addresses.is_empty() {
        anyhow::bail!("media host could not be resolved");
    }
    for address in addresses {
        if !is_public_ip(address.ip()) {
            anyhow::bail!("private, loopback and link-local media destinations are blocked");
        }
    }
    Ok(())
}

pub fn is_public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => {
            !(ip.is_loopback()
                || ip.is_private()
                || ip.is_link_local()
                || ip.is_unspecified()
                || ip.is_broadcast()
                || ip.is_documentation()
                || matches!(ip.octets(), [100, 64..=127, _, _])
                || matches!(ip.octets(), [169, 254, _, _])
                || matches!(ip.octets(), [0, _, _, _]))
        }
        IpAddr::V6(ip) => {
            !(ip.is_loopback()
                || ip.is_unspecified()
                || ip.is_multicast()
                || (ip.segments()[0] & 0xffc0) == 0xfe80
                || (ip.segments()[0] & 0xfe00) == 0xfc00)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{CanonicalMessage, CanonicalRequest};
    use axum::{
        http::{header, StatusCode},
        response::IntoResponse,
        routing::get,
        Router,
    };
    use serde_json::json;

    #[test]
    fn private_and_link_local_addresses_are_blocked() {
        assert!(!is_public_ip("127.0.0.1".parse().unwrap()));
        assert!(!is_public_ip("10.0.0.8".parse().unwrap()));
        assert!(!is_public_ip("192.168.1.1".parse().unwrap()));
        assert!(!is_public_ip("169.254.1.1".parse().unwrap()));
        assert!(!is_public_ip("::1".parse().unwrap()));
        assert!(is_public_ip("1.1.1.1".parse().unwrap()));
    }

    #[tokio::test]
    async fn https_loopback_and_redirects_to_private_hosts_are_rejected() {
        let app = Router::new()
            .route("/ok.bin", get(|| async { [1u8, 2, 3] }))
            .route(
                "/private",
                get(|| async {
                    (
                        StatusCode::FOUND,
                        [(header::LOCATION, "http://127.0.0.1/secret")],
                    )
                        .into_response()
                }),
            )
            .route(
                "/local",
                get(|| async {
                    (
                        StatusCode::FOUND,
                        [(header::LOCATION, "https://127.0.0.1/secret")],
                    )
                        .into_response()
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let client = reqwest::Client::new();
        let http = format!("http://{address}/ok.bin");
        assert!(fetch_public_media(&client, &http)
            .await
            .unwrap_err()
            .to_string()
            .contains("HTTPS"));
        let loopback = format!("https://127.0.0.1:{}/ok.bin", address.port());
        assert!(assert_public_url(&Url::parse(&loopback).unwrap())
            .await
            .is_err());
    }

    #[tokio::test]
    async fn data_urls_are_left_in_place() {
        let mut request = CanonicalRequest {
            system: Vec::new(),
            messages: vec![CanonicalMessage {
                role: "user".into(),
                content: vec![ContentBlock::Image {
                    url: "data:image/png;base64,AAAA".into(),
                    media_type: Some("image/png".into()),
                }],
            }],
            tools: Vec::new(),
            tool_choice: None,
            parallel_tool_calls: None,
            max_tokens: None,
            temperature: None,
            top_p: None,
            stop: None,
            reasoning: None,
            response_format: None,
            stream: false,
        };
        resolve_request_media(&reqwest::Client::new(), &mut request)
            .await
            .unwrap();
        assert_eq!(
            request.messages[0].content[0],
            ContentBlock::Image {
                url: "data:image/png;base64,AAAA".into(),
                media_type: Some("image/png".into()),
            }
        );
        let _ = json!({"ok": true});
    }
}
