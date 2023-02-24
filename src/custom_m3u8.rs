use std::{cell::RefCell, collections::HashMap, rc::Rc};

use reqwest::header::{ORIGIN, REFERER};
use tokio::{
    fs::File,
    io::AsyncWriteExt,
    sync::mpsc::{self, Receiver, Sender},
};

use crate::{
    gql::fetch_access_token_gql, model::TokenSig, stderr, utils::sha3_str, TwitchzeroError,
};

pub struct M3USegment<'a> {
    /// only url is needed to download live
    pub url: &'a str,

    // internal flags
    pub is_prefetch: bool,

    /// optional metadata, like EXTINF
    pub meta: Vec<&'a str>,

    /// parsed EXTINF
    pub parsed: M3UParsedMeta,
}

/// parsed EXTINF
pub struct M3UParsedMeta {
    pub duration: Option<f32>,
    pub title: Option<String>,
}

pub fn extract_segments(text: &str) -> Vec<M3USegment> {
    let mut v = Vec::new();
    let mut segment_meta = Vec::new();

    for line in text.lines() {
        let this_url = extract_url_m3u(line);

        if this_url.is_none() {
            if let Some(m) = line.strip_prefix('#') {
                segment_meta.push(m);
            }
        }

        if let Some((is_prefetch, this_url)) = this_url {
            let mut meta = Vec::new();
            std::mem::swap(&mut meta, &mut segment_meta);
            v.push(M3USegment {
                is_prefetch,
                url: this_url,
                parsed: parse_m3u_meta(&meta),
                meta,
            });
        }
    }
    v
}

fn extract_url_m3u(line: &str) -> Option<(bool, &str)> {
    let prefetch_prefix = "#EXT-X-TWITCH-PREFETCH:";
    if let Some(prefetch) = line.strip_prefix(prefetch_prefix) {
        Some((true, prefetch))
    } else if !line.starts_with('#') {
        Some((false, line))
    } else {
        None
    }
}

fn parse_m3u_meta(raw_meta: &[&str]) -> M3UParsedMeta {
    let mut duration = None;
    let mut title = None;

    for m in raw_meta {
        if let Some(extinf) = m.strip_prefix("EXTINF:") {
            let mut s = extinf.splitn(2, ',');
            let duration_str = s.next();
            duration = duration_str.and_then(|d| d.parse().ok());
            title = s.next().map(|s| s.to_owned());
        }
    }

    M3UParsedMeta { duration, title }
}

fn make_url_query(
    q: &mut form_urlencoded::Serializer<'_, url::UrlQuery<'_>>,
    token_sig: &TokenSig,
) {
    let mut rng = rand::thread_rng();
    use rand::prelude::*;
    q.append_pair("player", "twitchweb");
    let p = format!("{}", (99999.0 * rng.gen::<f64>()) as i64);
    q.append_pair("p", &p);
    q.append_pair("type", "any");
    q.append_pair("allow_source", "true");
    q.append_pair("allow_audio_only", "true");
    q.append_pair("allow_spectre", "false");

    q.append_pair("sig", &token_sig.sig);
    q.append_pair("token", &token_sig.token);
    q.append_pair("fast_bread", "true");
}

pub async fn fetch_m3u8_by_channel(
    client: &reqwest::Client,
    channel: String,
) -> Result<String, anyhow::Error> {
    let token_sig = fetch_access_token_gql(client, &channel).await?;

    let channel_addr = format!("https://usher.ttvnw.net/api/channel/hls/{}.m3u8", channel);
    let playlist_url = {
        let mut url = reqwest::Url::parse(&channel_addr)?;
        make_url_query(&mut url.query_pairs_mut(), &token_sig);

        stderr!("channel url: {}\n", url)?;
        let mut req = reqwest::Request::new(reqwest::Method::GET, url);
        let h = req.headers_mut();
        h.insert("Client-ID", "kimne78kx3ncx6brgo4mv6wki5h1ko".parse()?);

        h.insert(REFERER, "https://player.twitch.tv".parse()?);
        h.insert(ORIGIN, "https://player.twitch.tv".parse()?);

        let res = client.execute(req).await?;
        stderr!("channel: Status: {}\n", res.status())?;

        let text = res.text().await?;

        //debug_m3u(&text).await?;

        stderr!("channel: {}\n", &text)?;

        let segments_vec = extract_segments(&text);

        segments_vec
            .get(0)
            .expect("no m3u8 url found")
            .url
            .to_string()
    };

    Ok(playlist_url)
}

pub enum SegmentBytes {
    EOF,
    More(Rc<Vec<u8>>),
}

pub struct Segment {
    pub rx: RefCell<Receiver<SegmentBytes>>,
    pub tx: Sender<SegmentBytes>,
    //done_tx: Sender<bool>,
}

//type OutBufRef = Sender<Rc<Vec<u8>>>;

async fn fetch_segment(
    client: reqwest::Client,
    prefetch_url: &str,
    //outs: &mut Vec<OutBufRef>,
    segment: Rc<Segment>,
) -> Result<(), anyhow::Error> {
    let tx = segment.tx.clone();

    // for _i in 0..2 {
    let mut res = client.get(prefetch_url).send().await?;
    {
        while let Some(chunk) = res.chunk().await? {
            let vec_rc = Rc::new(chunk.to_vec());

            tx.send(SegmentBytes::More(vec_rc.clone()))
                .await
                .map_err(|_| TwitchzeroError::SegmentDataSendError)?;

            //stderr!(".")?;
        }
        //break;
    }
    // }
    tx.send(SegmentBytes::EOF)
        .await
        .map_err(|_| TwitchzeroError::SegmentDataSendError)?;
    drop(tx);

    Ok(())
}

pub fn is_segment_ad_start(segment: &M3USegment) -> bool {
    let mut is_discontinuity = false;
    let mut contains_ad_meta = false;
    for &m in &segment.meta {
        if m.contains("X-TV-TWITCH-AD") {
            contains_ad_meta = true;
        }
        if m == "EXT-X-DISCONTINUITY" {
            is_discontinuity = true;
        }
    }
    is_discontinuity && contains_ad_meta
}
pub fn is_segment_ad_end(segment: &M3USegment) -> bool {
    let mut is_discontinuity = false;
    let mut contains_ad_end_meta = false;
    for &m in &segment.meta {
        // regular live segment
        if m.contains("EXT-X-TWITCH-LIVE-SEQUENCE") {
            contains_ad_end_meta = true;
        }
        if m == "EXT-X-DISCONTINUITY" {
            is_discontinuity = true;
        }
    }
    is_discontinuity && contains_ad_end_meta
}

pub async fn check_ad_start_end<'a>(segment: &M3USegment<'a>, is_ad_range: &mut bool) {
    if is_segment_ad_start(segment) {
        *is_ad_range = true;
        let _ = stderr!("reload_m3u8: found ad start\n");
    }
    if is_segment_ad_end(segment) {
        *is_ad_range = false;
        let _ = stderr!("reload_m3u8: found ad end\n");
    }
}

pub async fn m3u_fetch_segment<'a>(
    client: &reqwest::Client,
    segments_tx: &Sender<Rc<Segment>>,
    txwake: &Sender<bool>,
    is_ad_range: bool,
    m3u_segment: &M3USegment<'a>,
) -> Result<(), anyhow::Error> {
    for meta in &m3u_segment.meta {
        stderr!("reload_m3u8: meta: {}\n", meta)?;
    }
    if m3u_segment.is_prefetch {
        stderr!("reload_m3u8: new(prefetch): {}\n", m3u_segment.url)?;
    } else {
        stderr!(
            "reload_m3u8: new({}): {}\n",
            m3u_segment.parsed.title.as_deref().unwrap_or(""),
            m3u_segment.url
        )?;
    }
    if is_ad_range {
        stderr!("skipping ads: found ad start\n")?;
        return Ok(());
    }

    if let Some(title) = &m3u_segment.parsed.title {
        if title.starts_with("Amazon") {
            stderr!("skipping Amazon ads, title: {}\n", title)?;
            return Ok(());
        }
    }

    let cclient = client.clone();
    let ctxwake = txwake.clone();

    let (tx, rx) = mpsc::channel::<SegmentBytes>(512);
    let segment = Rc::new(Segment {
        tx,
        rx: RefCell::new(rx),
    });
    segments_tx
        .send(segment.clone())
        .await
        .map_err(|_| TwitchzeroError::SegmentSendError)?;

    let segment_url = m3u_segment.url.to_string();
    tokio::task::spawn_local(async move {
        let res = fetch_segment(cclient, &segment_url, segment.clone()).await;
        if let Err(err) = res {
            let _ = stderr!("fetch_segment err: {}", err);
        }
        let _ = ctxwake.send(true).await;
    });

    //let sleep_duration = std::time::Duration::from_millis(10);
    //tokio::time::delay_for(sleep_duration).await;

    Ok(())
}

pub async fn reload_m3u8(
    used_segments: &mut HashMap<String, u64>,
    client: reqwest::Client,
    m3u8_url: String,
    segments_tx: Sender<Rc<Segment>>,
    txwake: Sender<bool>,
    epoch_number: u64,
) -> Result<(), anyhow::Error> {
    let res = client.get(&m3u8_url).send().await?;

    stderr!("reload_m3u8: Status: {}\n", res.status())?;

    let text = res.text().await?;

    let segments_vec = extract_segments(&text);
    let mut segments = segments_vec.as_slice();
    let limit = 5;
    if segments.len() > limit {
        let offset = segments.len() - limit;
        segments = &segments[offset..];
    }

    let num_segments = segments.len();
    let mut dup_count: usize = 0;
    let mut is_ad_range = false;
    for segment in segments {
        check_ad_start_end(segment, &mut is_ad_range).await;

        let furl = segment.url.to_string();
        if !used_segments.contains_key(&furl) {
            used_segments.insert(furl.to_string(), epoch_number);

            m3u_fetch_segment(&client, &segments_tx, &txwake, is_ad_range, segment).await?;
        } else {
            //stderr!("reload_m3u8: duplicate segment, not fetching\n")?;
            dup_count += 1;
        }
    }

    if dup_count == num_segments {
        //debug_m3u8("", &text).await?;
    }

    Ok(())
}

#[allow(unused)]
/// Creates file in /m3u8/ directory
async fn debug_m3u(prefix: &str, text: &str) -> Result<(), anyhow::Error> {
    let h = sha3_str(text);
    let _ = stderr!("creating: {}.m3u8\n", h);
    let mut file = File::create(format!("m3u8/{}{}.m3u8", prefix, h)).await?;
    file.write_all(text.as_bytes()).await?;
    Ok(())
}
