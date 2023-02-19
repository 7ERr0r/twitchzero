mod custom_m3u8;
mod gql;
mod isav;
mod model;
mod utils;

use clap::command;
use clap::Parser;
use custom_m3u8::fetch_m3u8_by_channel;
use custom_m3u8::reload_m3u8;
use custom_m3u8::Segment;
use custom_m3u8::SegmentBytes;
use isav::IsAudioVideo;
use tokio::time::Duration;

use futures::future::FutureExt;
use futures::select;
use std::cell::RefCell;
use std::rc::Rc;

use std::collections::HashMap;

use tokio::fs::File;
use tokio::io::AsyncWrite;
use tokio::io::{self, AsyncWriteExt};
use tokio::sync::mpsc;
use tokio::sync::mpsc::Receiver;
use tokio::sync::mpsc::Sender;

async fn copy_channel_to_out(
    mut rxbuf: Receiver<Rc<Vec<u8>>>,
    out: &mut AsyncOutput,
) -> Result<(), anyhow::Error> {
    loop {
        // Give some time to other tasks
        tokio::task::yield_now().await;

        // We'll receive from channel
        // in the future
        let f = rxbuf.recv();

        let once_msg: Option<Option<Rc<Vec<u8>>>> = f.now_or_never();
        let msg: Option<Rc<Vec<u8>>>;
        match once_msg {
            // Received instantly
            Some(m) => {
                msg = m;
            }
            None => {
                out.writer.flush().await?;
                //stderr!("flushed")?;

                msg = rxbuf.recv().await;
            }
        }

        match msg {
            Some(bytes_vec) => {
                //stderr!("+")?;
                out.writer.write_all(&bytes_vec).await?;
            }
            // None from .recv()
            // because channel is closed
            None => {
                break;
            }
        }
    }
    Ok(())
}

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

    let sleep_duration = std::time::Duration::from_millis(2000);

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
                stderr!("\nmain: sleep ready\n")?;
            }
            Ok(_) => {
                stderr!("\nmain: wake channel ready\n")?;
            }
        }
    }
    Ok(())
}

async fn check_ffplay_exists() -> Result<FFplayLocation, anyhow::Error> {
    use std::process::Stdio;
    use tokio::process::Command;
    let env_path = FFplayLocation::SystemEnvPath.ffplay_path();
    match Command::new(env_path)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(_) => {
            //stderr!("ffplay was spawned from path :)\n")?;
            return Ok(FFplayLocation::SystemEnvPath);
        }
        Err(e) => {
            if let std::io::ErrorKind::NotFound = e.kind() {
                // check next
            } else {
                return Err(e.into());
            }
        }
    }
    let ffpath = ".\\ffplay.exe";
    stderr!("Checking for file {}\n", ffpath)?;
    let attr_res = tokio::fs::metadata(ffpath).await;
    match attr_res {
        Ok(attr) => {
            if attr.is_file() {
                return Ok(FFplayLocation::CurrentDir);
            }
        }

        Err(err) => {
            if err.kind() != std::io::ErrorKind::NotFound {
                return Err(err.into());
            }
        }
    }
    return Ok(FFplayLocation::NotFound);
}

#[derive(Copy, Clone, Debug, PartialEq)]
enum FFplayLocation {
    NotChecked,
    SystemEnvPath,
    CurrentDir,
    NotFound,
}

impl FFplayLocation {
    pub fn ffplay_path(&self) -> &str {
        match self {
            FFplayLocation::NotChecked => "",
            FFplayLocation::SystemEnvPath => "ffplay",
            FFplayLocation::CurrentDir => ".//ffplay",
            FFplayLocation::NotFound => "",
        }
    }
}

async fn ensure_ffplay(client: &reqwest::Client) -> Result<FFplayLocation, anyhow::Error> {
    let ffplay_location = check_ffplay_exists().await?;
    if let FFplayLocation::NotFound = ffplay_location {
    } else {
        return Ok(ffplay_location);
    }
    stderr!("Not found ffplay, Downloading ffplay\n")?;
    let res = client
        .get("https://github.com/7ERr0r/twitchzero/blob/master/ffplay.zip?raw=true")
        .send()
        .await?;
    let bytes_vec = res.bytes().await?;

    let reader = std::io::Cursor::new(bytes_vec);
    let mut zip = zip::ZipArchive::new(reader)?;
    for i in 0..zip.len() {
        let mut file = zip.by_index(i).expect("file by_index");
        stderr!("Extracting filename: {}\n", file.name())?;

        use std::io::prelude::*;
        let mut outfile = File::create(file.name()).await?;
        let mut buf = [0u8; 1024 * 8];

        loop {
            let size = match file.read(&mut buf) {
                Err(err) => {
                    if err.kind() == std::io::ErrorKind::Interrupted {
                        break;
                    }
                    return Err(err.into());
                }
                Ok(size) => size,
            };
            if size == 0 {
                break;
            }
            outfile.write_all(&buf[0..size]).await?;
        }

        stderr!("Extracted filename: {}\n", file.name())?;
    }

    Ok(FFplayLocation::CurrentDir)
}

async fn spawn_ffplay(
    client: &reqwest::Client,
    copy_ended: Option<Rc<RefCell<bool>>>,
    channel: &str,
    isav: IsAudioVideo,
    ffplay_location: Rc<RefCell<FFplayLocation>>,
) -> Result<tokio::process::ChildStdin, anyhow::Error> {
    if *ffplay_location.borrow() == FFplayLocation::NotChecked {
        *ffplay_location.borrow_mut() = ensure_ffplay(&client).await?;
    }

    use std::process::Stdio;
    use tokio::process::Command;

    let mut cmd = Box::new(Command::new(ffplay_location.borrow().ffplay_path()));

    // audio
    // ffplay -window_title "%channel%" -fflags nobuffer -flags low_delay -af volume=0.2 -x 1280 -y 720 -i -

    // video
    // ffplay -window_title "%channel%" -probesize 32 -sync ext -af volume=0.3 -vf setpts=0.5*PTS -x 1280 -y 720 -i -

    if isav.is_audio() {
        cmd.arg("-window_title").arg(format!("{} audio", channel));
        cmd.arg("-fflags")
            .arg("nobuffer")
            .arg("-flags")
            .arg("low_delay")
            .arg("-vn")
            .arg("-framedrop")
            .arg("-af")
            .arg("volume=0.4,atempo=1.005")
            .arg("-strict")
            .arg("experimental")
            .arg("-x")
            .arg("320")
            .arg("-y")
            .arg("240")
            .arg("-i")
            .arg("-");
    } else {
        cmd.arg("-window_title")
            .arg(format!("{} lowest latency", channel));

        cmd.arg("-probesize")
            .arg("32")
            .arg("-sync")
            .arg("ext")
            .arg("-framedrop")
            .arg("-vf")
            .arg("setpts=0.5*PTS")
            .arg("-x")
            .arg("1280")
            .arg("-y")
            .arg("720")
            .arg("-i")
            .arg("-");
    }

    let write_stdout = false;
    let (err, out) = if write_stdout {
        let file_out = File::create(format!("ffplay_{}_stdout.txt", isav.name())).await?;
        let file_err = File::create(format!("ffplay_{}_stderr.txt", isav.name())).await?;
        (
            Stdio::from(file_out.into_std().await),
            Stdio::from(file_err.into_std().await),
        )
    } else {
        (Stdio::null(), Stdio::null())
    };

    cmd.stdout(out).stderr(err).stdin(Stdio::piped());

    let mut child = cmd.spawn().expect("failed to spawn ffplay");

    let stdin = child
        .stdin
        .take()
        .expect("child did not have a handle to stdin");

    let copy_ended = copy_ended.clone();
    tokio::task::spawn_local(async move {
        let status = child
            .wait()
            .await
            .expect("child process encountered an error");

        let _ = stderr!("child status was: {}\n", status);
        if let Some(copy_ended) = copy_ended {
            *copy_ended.borrow_mut() = true;
        }
    });

    Ok(stdin)
}

async fn make_outs(
    client: &reqwest::Client,
    out_names: &mut Vec<&str>,
    copy_ended: Rc<RefCell<bool>>,
    channel: &str,
    ffplay_location: Rc<RefCell<FFplayLocation>>,
) -> Result<Vec<AsyncOutput>, anyhow::Error> {
    let mut outs: Vec<AsyncOutput> = Vec::new();

    while let Some(out_name) = out_names.pop() {
        match out_name {
            "ffplay" => {
                out_names.push("video");
                out_names.push("audio");
            }
            "video" => {
                let stdin_video = spawn_ffplay(
                    client,
                    Some(copy_ended.clone()),
                    channel,
                    IsAudioVideo::video(),
                    ffplay_location.clone(),
                )
                .await?;
                outs.push(AsyncOutput::new_unreliable(Box::new(stdin_video)));
            }
            "audio" => {
                let stdin_audio = spawn_ffplay(
                    client,
                    None,
                    channel,
                    IsAudioVideo::audio(),
                    ffplay_location.clone(),
                )
                .await?;
                outs.push(AsyncOutput::new_unreliable_ignored(Box::new(stdin_audio)));
            }
            "out" => {
                outs.push(AsyncOutput::new(Box::new(io::stdout())));
            }
            "stdout" => {
                outs.push(AsyncOutput::new(Box::new(io::stdout())));
            }
            "stderr" => {
                outs.push(AsyncOutput::new(Box::new(io::stderr())));
            }
            other_out => {
                const TCP_O: &str = "tcp:";
                const UNIX_O: &str = "unix:";
                if other_out.starts_with(TCP_O) {
                    let addr = &other_out[TCP_O.len()..];
                    use tokio::net::TcpStream;
                    let stream = TcpStream::connect(addr)
                        .await
                        .map_err(|err| TwitchlinkError::TCPConnectError(err))?;
                    outs.push(AsyncOutput::new(Box::new(stream)));
                } else if other_out.starts_with(UNIX_O) {
                    #[cfg(target_os = "linux")]
                    {
                        let addr = &other_out[UNIX_O.len()..];
                        use tokio::net::UnixStream;

                        let stream = UnixStream::connect(addr)
                            .await
                            .map_err(|err| TwitchlinkError::UnixConnectError(err))?;
                        outs.push(Box::new(stream));
                    }
                    #[cfg(not(target_os = "linux"))]
                    {
                        return Err(TwitchlinkError::NoUnixSocketError.into());
                    }
                } else {
                    let file = File::create(other_out)
                        .await
                        .map_err(|err| TwitchlinkError::FileCreateError(err))?;
                    outs.push(AsyncOutput::new(Box::new(file)));
                }
            }
        }
    }

    Ok(outs)
}

struct AsyncOutput {
    writer: Box<dyn AsyncWrite + Unpin>,
    pub reliable: bool,
    // false to ignore process exit code or pipe being closed
    pub end_on_broken_pipe: bool,
}

impl AsyncOutput {
    fn new(writer: Box<dyn AsyncWrite + Unpin>) -> Self {
        Self {
            writer,
            reliable: true,
            end_on_broken_pipe: true,
        }
    }
    fn new_unreliable(writer: Box<dyn AsyncWrite + Unpin>) -> Self {
        Self {
            writer,
            reliable: false,
            end_on_broken_pipe: true,
        }
    }
    fn new_unreliable_ignored(writer: Box<dyn AsyncWrite + Unpin>) -> Self {
        Self {
            writer,
            reliable: false,
            end_on_broken_pipe: false,
        }
    }
}

fn make_out_writers(
    outs: Vec<AsyncOutput>,
    copy_ended: Rc<RefCell<bool>>,
) -> Vec<OutputStreamSender> {
    let mut txbufs = Vec::new();
    for mut out in outs {
        let channel_size = 256;
        let (txbuf, rxbuf) = mpsc::channel::<Rc<Vec<u8>>>(channel_size);
        let copy_endedc = copy_ended.clone();

        txbufs.push(OutputStreamSender {
            tx: txbuf,
            reliable: out.reliable,
        });
        let end_on_broken_pipe = out.end_on_broken_pipe;
        tokio::task::spawn_local(async move {
            let res = copy_channel_to_out(rxbuf, &mut out).await;
            match res {
                Err(err) => {
                    let _ = stderr!("copy_channel_to_out err: {}\n", err);
                }
                Ok(_) => {}
            }

            if end_on_broken_pipe {
                let _ = stderr!("copy_channel_to_out ending (flagging copy_endedc)\n");
                *copy_endedc.borrow_mut() = true;
            } else {
                let _ = stderr!("copy_channel_to_out ending (not flagging end channel)\n");
            }
        });
    }
    txbufs
}

/// twitch low latency
#[derive(Parser, Debug)]
#[command(
    author = "7ERr0r",
    version,
    about,
    long_about = "Lowest possible m3u8 latency on twitch. Acquires playlist url, then downloads with async segment/bytes fetching"
)]
struct Args {
    /// File path or 'out' for stdout, 'ffplay' for player window
    #[arg(short, long)]
    out: Option<Vec<String>>,

    /// Playlist url
    #[arg(short, long)]
    playlist_m3u8: Option<String>,

    /// Twitch channel url
    channel: String,
}

#[tokio::main]
async fn main() -> Result<(), anyhow::Error> {
    let args = Args::parse();

    let out_names = args.out.unwrap_or_else(|| vec!["ffplay".into()]);

    let client = reqwest::Client::builder()
        .build()
        .expect("should be able to build reqwest client");

    let ffplay_location = Rc::new(RefCell::new(FFplayLocation::NotChecked));
    // // TODO: shorten but check only once
    // if out_names.contains(&"ffplay") || out_names.contains(&"audio") || out_names.contains(&"video")
    // {
    //     ffplay_location = ensure_ffplay(&client).await?;
    // }

    let m3u8playlist_url = if let Some(playlist) = args.playlist_m3u8 {
        playlist
    } else {
        fetch_m3u8_by_channel(&client, args.channel.to_string()).await?
    };

    stderr!("url: {:?}\n", m3u8playlist_url)?;

    let local = tokio::task::LocalSet::new();
    // Run the local task set.
    local
        .run_until(async move {
            let copy_ended = Rc::new(RefCell::new(false));

            let result = ordered_download(
                client,
                out_names.iter().map(|s| s.as_ref()).collect(),
                copy_ended,
                m3u8playlist_url,
                &args.channel,
                ffplay_location,
            )
            .await;

            result
        })
        .await?;

    Ok(())
}

async fn ordered_download(
    client: reqwest::Client,
    mut out_names: Vec<&str>,
    copy_ended: Rc<RefCell<bool>>,
    m3u8playlist_url: String,
    channel: &str,
    ffplay_location: Rc<RefCell<FFplayLocation>>,
) -> Result<(), anyhow::Error> {
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

struct OutputStreamSender {
    reliable: bool,
    tx: Sender<Rc<Vec<u8>>>,
}

// copies from Segment-s in segments_rx
// to channels of Vec<u8> in txbufs
async fn join_one_segment(
    segments_rx: &mut Receiver<Rc<Segment>>,
    txbufs: &mut Vec<OutputStreamSender>,
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
                    Some(SegmentBytes::EOF) => {
                        break;
                    }
                    Some(SegmentBytes::More(bytes)) => {
                        for out in txbufs.iter_mut() {
                            //stderr!("x")?;
                            if out.reliable {
                                out.tx
                                    .send(bytes.clone())
                                    .await
                                    .map_err(|_| TwitchlinkError::SegmentJoinSendError)?;
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

async fn recv_timeout<T>(
    rx: &mut Receiver<T>,
    sleep_duration: std::time::Duration,
) -> Result<Option<T>, ()> {
    let sleep = tokio::time::sleep(sleep_duration);
    let done = rx.recv();

    let mut f1sleep = Box::pin(sleep.fuse());
    let mut f2done = Box::pin(done.fuse());
    select! {
        _ = f1sleep => {
            return Err(());
        },
        x = f2done => {
            return Ok(x);
        },
    };
}

#[derive(Debug, derive_more::Display)]
pub enum TwitchlinkError {
    #[display(fmt = "file ffplay.exe is not a file")]
    FileIsNotFFplay,

    #[display(fmt = "TCPConnectError: {}", _0)]
    TCPConnectError(std::io::Error),

    #[display(fmt = "UnixConnectError: {}", _0)]
    UnixConnectError(std::io::Error),

    #[display(fmt = "FileCreateError: {}", _0)]
    FileCreateError(std::io::Error),

    #[display(fmt = "NoUnixSocketError: target_os != linux")]
    NoUnixSocketError,

    #[display(fmt = "SegmentDataSendError: can't send")]
    SegmentDataSendError,

    #[display(fmt = "SegmentSendError")]
    SegmentSendError,

    #[display(fmt = "SegmentJoinSendError")]
    SegmentJoinSendError,
}
impl std::error::Error for TwitchlinkError {}
