use std::{cell::RefCell, collections::HashMap, rc::Rc, time::Duration};

use tokio::{
    sync::mpsc::{self, Sender},
    time::timeout,
};

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
    if reloads % n == 0 {
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

        if let Err(reload_err) = reload_result {
            stderr!("reload_err: {}\n", reload_err)?;
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

        let tresult = timeout(Duration::from_millis(4000), one_segment_future).await;
        if tresult.is_err() {
            stderr!("\nordered_download: segment timed out\n")?;
        }
    }

    Ok(())
}

// TODO
//pub struct TwitchzeroApp {}

// pub enum OrderingEvent {
//     SegmentStart(i64),
//     SegmentData(i64, Bytes),
//     SegmentEof(i64),
// }

// async fn handle_single_event(
//     segment_event: OrderingEvent,
//     isav: IsAudioVideo,
//     segment_map: &mut BTreeMap<i64, SegmentsBuf>,
// ) -> Result<()> {
//     match segment_event {
//         OrderingEvent::SegmentStart(sq) => {
//             // if consts.should_discard_sq(sq) {
//             //     stderr!("{} ignoring sq={}\n", isav, sq,);
//             //     //return Ok(JoinSegmentResult::GoNext);
//             // } else {
//             //     stderr!("{} SegmentStart sq={}\n", isav, sq,);
//             //     segment_map.insert(sq, SegmentsBuf::default());
//             // }
//         }
//         OrderingEvent::SegmentEof(sq) => {
//             let buf = segment_map.get_mut(&sq);
//             if let Some(buf) = buf {
//                 buf.eof = true;
//             }
//         }

//         OrderingEvent::SegmentData(sq, chunk) => {
//             let buf = segment_map.get_mut(&sq);
//             if let Some(buf) = buf {
//                 buf.chunks.push_back(chunk);
//             }
//         }
//     }
//     Ok(())
// }
