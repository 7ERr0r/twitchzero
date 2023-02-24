use reqwest::header::{HeaderMap, ACCEPT, ORIGIN, REFERER};

use crate::{
    model::{GQLTokenResponse, TokenSig},
    stderr,
};

/// Streamlink:
/// https://github.com/streamlink/streamlink/blob/e2e41987ae4a9026c3866943d24cd87ef4652e7b/src/streamlink/plugins/twitch.py#L664
pub async fn fetch_access_token_gql(
    client: &reqwest::Client,
    channel: &String,
) -> Result<TokenSig, anyhow::Error> {
    let access_token_addr = format!("https://gql.twitch.tv/gql");
    let token_sig = {
        let params = [("platform", "_")];
        let url = reqwest::Url::parse_with_params(&access_token_addr, &params)?;

        let is_live = true;
        let login = if is_live { channel } else { "" };
        //let vod_id = if is_live { "" } else { "" };
        let vod_id = "";
        let json_map = serde_json::json!({
            "operationName": "PlaybackAccessToken",
            "extensions": {
                "persistedQuery": {
                    "version": 1,
                    "sha256Hash": "0828119ded1c13477966434e15800ff57ddacf13ba1911c129dc2200705b0712"
                }
            },
            "variables": {
                "isLive": is_live,
                "login": login,
                "isVod": !is_live,
                "vodID": vod_id,
                "playerType": "embed"
            }
        });

        let mut h = HeaderMap::new();
        h.insert("Client-ID", "kimne78kx3ncx6brgo4mv6wki5h1ko".parse()?);
        h.insert(ACCEPT, "application/vnd.twitchtv.v3+json".parse()?);
        h.insert(REFERER, "https://player.twitch.tv".parse()?);
        h.insert(ORIGIN, "https://player.twitch.tv".parse()?);

        let res = client.post(url).json(&json_map).headers(h).send().await?;

        stderr!("fetch_access_token_gql: Status: {}\n", res.status())?;

        let bytes_vec = res.bytes().await?;
        stderr!(
            "fetch_access_token_gql: {}\n",
            String::from_utf8_lossy(&bytes_vec)
        )?;

        let gql_resp: GQLTokenResponse = serde_json::from_slice(&bytes_vec)?;

        let token_sig = TokenSig {
            token: gql_resp.data.streamPlaybackAccessToken.value,
            sig: gql_resp.data.streamPlaybackAccessToken.signature,
        };
        token_sig
    };
    Ok(token_sig)
}
