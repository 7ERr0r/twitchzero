use std::{cell::RefCell, collections::HashMap, rc::Rc};

use reqwest::header::{ORIGIN, REFERER};
use tokio::{
    fs::File,
    io::AsyncWriteExt,
    sync::mpsc::{self, Receiver, Sender},
};

use crate::{gql::fetch_access_token_gql, stderr, utils::sha3_str, TwitchlinkError};

pub fn extract_segments(text: &str) -> Vec<&str> {
    let prefix = "#EXT-X-TWITCH-PREFETCH:";
    let mut v = Vec::new();
    for line in text.lines() {
        if line.starts_with(prefix) {
            let l = line.trim_start_matches(prefix);

            v.push(l);
        } else if !line.starts_with("#") {
            v.push(line);
        }
    }
    v
}

pub async fn fetch_m3u8_by_channel(
    client: &reqwest::Client,
    channel: String,
) -> Result<String, anyhow::Error> {
    let token_sig = fetch_access_token_gql(client, &channel).await?;

    let channel_addr = format!("https://usher.ttvnw.net/api/channel/hls/{}.m3u8", channel);
    let playlist_url = {
        let mut rng = rand::thread_rng();
        use rand::prelude::*;

        let mut url = reqwest::Url::parse(&channel_addr)?;
        {
            let mut q = url.query_pairs_mut();
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
        stderr!("channel url: {}\n", url)?;
        let mut req = reqwest::Request::new(reqwest::Method::GET, url);
        let h = req.headers_mut();
        h.insert("Client-ID", "kimne78kx3ncx6brgo4mv6wki5h1ko".parse()?);
        h.insert(REFERER, "https://player.twitch.tv".parse()?);
        h.insert(ORIGIN, "https://player.twitch.tv".parse()?);

        let res = client.execute(req).await?;
        stderr!("channel: Status: {}\n", res.status())?;

        let text = res.text().await?;
        let debug_m3u8 = false;
        if debug_m3u8 {
            let h = sha3_str(&text);
            let _ = stderr!("creating: {}.m3u8\n", h);
            tokio::fs::create_dir_all("./m3u8").await?;
            let mut file = File::create(format!("m3u8/{}.m3u8", h)).await?;
            file.write_all(&text.as_bytes()).await?;
        }

        stderr!("channel: {}\n", &text)?;

        let segments_vec = extract_segments(&text);

        segments_vec.get(0).expect("no m3u8 url found").to_string()
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
    prefetch_url: String,
    //outs: &mut Vec<OutBufRef>,
    segment: Rc<Segment>,
) -> Result<(), anyhow::Error> {
    let tx = segment.tx.clone();

    for _i in 0..2 {
        let mut res = client.get(&prefetch_url).send().await?;
        {
            while let Some(chunk) = res.chunk().await? {
                let vec_rc = Rc::new((&chunk).to_vec());

                tx.send(SegmentBytes::More(vec_rc.clone()))
                    .await
                    .map_err(|_| TwitchlinkError::SegmentDataSendError)?;

                //stderr!(".")?;
            }
            break;
        }
    }
    tx.send(SegmentBytes::EOF)
        .await
        .map_err(|_| TwitchlinkError::SegmentDataSendError)?;
    drop(tx);

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
        let (_left, right) = segments.split_at(segments.len() - limit);
        segments = right;
    }
    let num_segments = segments.len();
    let mut dup_count: usize = 0;
    for segment_url in segments {
        let furl = segment_url.to_string();
        if !used_segments.contains_key(&furl) {
            used_segments.insert(furl.to_string(), epoch_number);

            stderr!("reload_m3u8: new: {}\n", furl)?;
            let cclient = client.clone();
            let ctxwake = txwake.clone();

            let (tx, rx) = mpsc::channel::<SegmentBytes>(512);
            let segment = Rc::new(Segment {
                tx: tx,
                rx: RefCell::new(rx),
            });
            segments_tx
                .send(segment.clone())
                .await
                .map_err(|_| TwitchlinkError::SegmentSendError)?;

            tokio::task::spawn_local(async move {
                let res = fetch_segment(cclient, furl, segment.clone()).await;
                match res {
                    Err(err) => {
                        let _ = stderr!("fetch_segment err: {}", err);
                    }
                    _ => {}
                }
                let _ = ctxwake.send(true).await;
            });

        //let sleep_duration = std::time::Duration::from_millis(10);
        //tokio::time::delay_for(sleep_duration).await;
        } else {
            //stderr!("reload_m3u8: duplicate segment, not fetching\n")?;
            dup_count += 1;
        }
    }

    if false {
        if dup_count == num_segments {
            let h = sha3_str(&text);
            let _ = stderr!("creating: {}.m3u8\n", h);
            let mut file = File::create(format!("m3u8/{}.m3u8", h)).await?;
            file.write_all(&text.as_bytes()).await?;
        }
    }
    Ok(())
}
