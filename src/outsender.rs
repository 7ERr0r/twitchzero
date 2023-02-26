use std::rc::Rc;

use tokio::sync::mpsc::{Receiver, Sender};

use crate::{
    custom_m3u8::{Segment, SegmentBytes},
    stderr,
    tzerror::TwitchzeroError,
};

pub struct OutputStreamSender {
    pub reliable: bool,
    pub tx: Sender<Rc<Vec<u8>>>,
}

#[allow(clippy::await_holding_refcell_ref)]
/// copies
/// from: Segment-s in segments_rx
///   to: channels of Vec<u8> in txbufs
pub async fn join_one_segment(
    segments_rx: &mut Receiver<Rc<Segment>>,
    txbufs: &mut [OutputStreamSender],
) -> Result<(), anyhow::Error> {
    let res = segments_rx.recv().await;
    match res {
        None => {
            stderr!("\nordered_download: no more segments\n")?;
        }
        Some(segment) => {
            loop {
                let rx = &segment.rx;
                let mut rx = rx.borrow_mut();
                let out_segment_bytes = rx.recv().await;

                match out_segment_bytes {
                    Some(SegmentBytes::Eof) => {
                        break;
                    }
                    Some(SegmentBytes::More(bytes)) => {
                        for out in txbufs.iter_mut() {
                            //stderr!("x")?;
                            if out.reliable {
                                out.tx
                                    .send(bytes.clone())
                                    .await
                                    .map_err(|_| TwitchzeroError::SegmentJoinSend)?;
                            } else {
                                // non-blocking
                                let _r = out.tx.try_send(bytes.clone());
                            }
                        }
                    }
                    None => {
                        // Sender channel dropped
                        break;
                    }
                }
            }
        }
    }

    Ok(())
}
