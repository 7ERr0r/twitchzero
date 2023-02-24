use std::{cell::RefCell, collections::HashMap, rc::Rc, time::Duration};

use tokio::sync::mpsc::{self, Sender};

use crate::{
    custom_m3u8::{reload_m3u8, Segment},
    ffplay::FFplayLocation,
    outsender::join_one_segment,
    outwriter::{make_out_writers, make_outs},
    stderr,
    utils::recv_timeout,
};

fn clear_timeout_hashmap(used_segments: &mut HashMap<String, u64>, reloads: u64) {
    // clear hashmap sometimes, after n reloads
    let n = 16;
    if reloads > n && reloads & 7 == 0 {
        let min_epoch = reloads - n;

        used_segments.retain(|_key, value| *value >= min_epoch);
    }
}

async fn reload_loop(
    client: reqwest::Client,
    m3u8playlist_url: String,
    //txbufs: &mut Vec<OutBufRef>,
    segments_tx: Sender<Rc<Segment>>,
    copy_ended: Rc<RefCell<bool>>,
) -> Result<(), anyhow::Error> {
    let (txwake, mut rxwake) = mpsc::channel::<bool>(10);

    let mut used_segments: HashMap<String, u64> = HashMap::new();

    let sleep_duration = std::time::Duration::from_millis(2400);

    let mut reloads: u64 = 0;

    while !*copy_ended.borrow() {
        let reload_result = reload_m3u8(
            &mut used_segments,
            client.clone(),
            m3u8playlist_url.clone(),
            segments_tx.clone(),
            txwake.clone(),
            //&mut chain,
            reloads,
        )
        .await;
        reloads += 1;

        match reload_result {
            Err(reload_err) => {
                stderr!("reload_err: {}\n", reload_err)?;
            }
            Ok(_) => {}
        }

        clear_timeout_hashmap(&mut used_segments, reloads);

        match recv_timeout(&mut rxwake, sleep_duration).await {
            Err(_) => {
                stderr!("\nmain: timeout, refreshing\n")?;
            }
            Ok(_) => {
                stderr!("\nmain: wake channel ready\n")?;
            }
        }
    }
    Ok(())
}

pub async fn ordered_download(
    client: reqwest::Client,
    mut out_names: Vec<&str>,
    m3u8playlist_url: String,
    channel: &str,
    ffplay_location: Rc<RefCell<FFplayLocation>>,
) -> Result<(), anyhow::Error> {
    let copy_ended = Rc::new(RefCell::new(false));
    let outs = make_outs(
        &client,
        &mut out_names,
        copy_ended.clone(),
        channel,
        ffplay_location.clone(),
    )
    .await?;

    let mut txbufs = make_out_writers(outs, copy_ended.clone());

    let (segments_tx, mut segments_rx) = mpsc::channel::<Rc<Segment>>(200);

    {
        let copy_ended = copy_ended.clone();
        let client = client.clone();
        tokio::task::spawn_local(async move {
            reload_loop(client, m3u8playlist_url, segments_tx, copy_ended.clone())
                .await
                .expect("reload_loop");
        });
    }

    while !*copy_ended.borrow() {
        let one_segment_future = join_one_segment(&mut segments_rx, &mut txbufs);

        match tokio::time::timeout(Duration::from_millis(4000), one_segment_future).await {
            Err(_) => {
                stderr!("\nordered_download: segment timed out\n")?;
            }
            Ok(_) => {
                // we got the whole segment
            }
        }
    }

    Ok(())
}
