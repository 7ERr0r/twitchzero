use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
pub struct TokenSig {
    pub token: String,
    pub sig: String,
}

#[derive(Serialize, Deserialize)]
pub struct GQLTokenSignature {
    pub value: String,
    pub signature: String,
}

#[derive(Serialize, Deserialize)]
#[allow(non_snake_case)]
pub struct GQLTokenResponseData {
    pub streamPlaybackAccessToken: GQLTokenSignature,
}

#[derive(Serialize, Deserialize)]
pub struct GQLTokenResponse {
    pub data: GQLTokenResponseData,
}

#[derive(Serialize, Deserialize)]
pub struct GQLAnyResponse<D> {
    pub data: Box<D>,
}
