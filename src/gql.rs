use reqwest::header::{HeaderMap, ACCEPT, ORIGIN, REFERER};
use serde::{de, Serialize};

use crate::{
    model::{GQLAnyResponse, GQLTokenResponseData, TokenSig},
    stderr,
};

#[derive(Default)]
pub enum DeviceIdAction<'a> {
    /// set `Device-ID`
    InitDeviceId(&'a str),

    // TODO maybe useful
    #[allow(dead_code)]
    /// set `X-Device-Id`
    XDeviceId(&'a str),

    #[default]
    DoNotSet,
}

pub fn gql_headers(daction: DeviceIdAction) -> Result<HeaderMap, anyhow::Error> {
    let mut h = HeaderMap::new();

    match daction {
        DeviceIdAction::InitDeviceId(device_id) => h.insert("Device-ID", device_id.parse()?),
        DeviceIdAction::XDeviceId(device_id) => h.insert("X-Device-Id", device_id.parse()?),
        DeviceIdAction::DoNotSet => None,
    };
    h.insert("Client-ID", "kimne78kx3ncx6brgo4mv6wki5h1ko".parse()?);

    h.insert(ACCEPT, "application/vnd.twitchtv.v3+json".parse()?);
    h.insert(REFERER, "https://player.twitch.tv".parse()?);
    h.insert(ORIGIN, "https://player.twitch.tv".parse()?);
    h.insert(ORIGIN, "https://player.twitch.tv".parse()?);

    Ok(h)
}

// https://github.com/streamlink/streamlink/blob/57d88a5fa02d240afdfcce3bddf094026833e2f0/src/streamlink/plugins/twitch.py#L249
pub async fn gql_persisted_query<'a, I, O>(
    client: &reqwest::Client,
    json_input: &I,
    daction: DeviceIdAction<'_>,
) -> Result<Box<O>, anyhow::Error>
where
    I: Serialize + ?Sized,
    O: de::DeserializeOwned,
{
    let gql_addr = "https://gql.twitch.tv/gql".to_string();

    let params = [("platform", "_")];
    let req_url = reqwest::Url::parse_with_params(&gql_addr, &params)?;

    let res = client
        .post(req_url)
        .json(json_input)
        .headers(gql_headers(daction)?)
        .send()
        .await?;

    stderr!("gql: Status: {}\n", res.status())?;

    let resp_bytes = res.bytes().await?;
    stderr!("gql response: {}\n", String::from_utf8_lossy(&resp_bytes))?;

    let data = {
        let gql_resp: GQLAnyResponse<O> = serde_json::from_slice(&resp_bytes)?;
        gql_resp.data
    };
    Ok(data)
}

pub async fn fetch_access_token_gql(
    client: &reqwest::Client,
    channel: &str,
    device_id: Option<&str>,
) -> Result<TokenSig, anyhow::Error> {
    let use_template = true;
    let daction = device_id
        .map(DeviceIdAction::InitDeviceId)
        .unwrap_or_default();
    if use_template {
        fetch_access_token_gql_template(client, channel, daction).await
    } else {
        fetch_access_token_gql_persisted(client, channel, daction).await
    }
}

pub fn make_access_token_variables(channel: &str) -> serde_json::Value {
    let is_live = true;
    let login = if is_live { channel } else { "" };
    let vod_id = if is_live { "" } else { "x" };
    serde_json::json!({
        "isLive": is_live,
        "login": login,
        "isVod": !is_live,
        "vodID": vod_id,
        "playerType": "site"
    })
}

/// `PlaybackAccessToken`
/// Streamlink:
/// https://github.com/streamlink/streamlink/blob/e2e41987ae4a9026c3866943d24cd87ef4652e7b/src/streamlink/plugins/twitch.py#L664
pub async fn fetch_access_token_gql_persisted(
    client: &reqwest::Client,
    channel: &str,
    daction: DeviceIdAction<'_>,
) -> Result<TokenSig, anyhow::Error> {
    let json_map = serde_json::json!({
        "operationName": "PlaybackAccessToken",
        "extensions": {
            "persistedQuery": {
                "version": 1,
                "sha256Hash": "0828119ded1c13477966434e15800ff57ddacf13ba1911c129dc2200705b0712"
            }
        },
        "variables": make_access_token_variables(channel)
    });

    let gql_resp: Box<GQLTokenResponseData> =
        gql_persisted_query(client, &json_map, daction).await?;

    let token_sig = TokenSig {
        token: gql_resp.streamPlaybackAccessToken.value,
        sig: gql_resp.streamPlaybackAccessToken.signature,
    };

    Ok(token_sig)
}

pub async fn fetch_access_token_gql_template(
    client: &reqwest::Client,
    channel: &str,
    daction: DeviceIdAction<'_>,
) -> Result<TokenSig, anyhow::Error> {
    let query = include_str!("access_token_template.txt");
    let json_map = serde_json::json!({
        "operationName": "PlaybackAccessToken_Template",
        "query": query,

        "variables": make_access_token_variables(channel)
    });

    let gql_resp: Box<GQLTokenResponseData> =
        gql_persisted_query(client, &json_map, daction).await?;

    let token_sig = TokenSig {
        token: gql_resp.streamPlaybackAccessToken.value,
        sig: gql_resp.streamPlaybackAccessToken.signature,
    };

    Ok(token_sig)
}
