use reqwest::header::HeaderMap;
use reqwest::header::ACCEPT;
use reqwest::header::ORIGIN;
use reqwest::header::REFERER;
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

use serde::{Deserialize, Serialize};

#[derive(Copy, Clone, Default, PartialEq, Eq)]
pub struct IsAudioVideo(bool);

impl IsAudioVideo {
    pub fn audio() -> Self {
        Self(true)
    }
    pub fn video() -> Self {
        Self(false)
    }

    pub fn is_audio(&self) -> bool {
        self.0
    }
    pub fn is_video(&self) -> bool {
        !self.is_audio()
    }

    pub fn name(&self) -> &str {
        if self.is_audio() {
            "audio"
        } else {
            "video"
        }
    }
}

impl std::fmt::Display for IsAudioVideo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.is_audio() {
            write!(f, "[AUD]")?;
        } else {
            write!(f, "[VID]")?;
        }
        Ok(())
    }
}

#[derive(Serialize, Deserialize)]
struct TokenSig {
    token: String,
    sig: String,
}

macro_rules! stderr {
    () => (io::stderr().write_all(&[0; 0]).await);
    ($($arg:tt)*) => ({
        io::stderr().write_all(&std::format!($($arg)*).as_bytes()).await
    })
}

fn extract_segments(text: &str) -> Vec<&str> {
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

enum SegmentBytes {
    EOF,
    More(Rc<Vec<u8>>),
}

struct Segment {
    rx: RefCell<Receiver<SegmentBytes>>,
    tx: Sender<SegmentBytes>,
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

async fn reload_m3u8(
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
            let h = sha3(&text);
            let _ = stderr!("creating: {}.m3u8\n", h);
            let mut file = File::create(format!("m3u8/{}.m3u8", h)).await?;
            file.write_all(&text.as_bytes()).await?;
        }
    }
    Ok(())
}

fn sha3(text: &String) -> String {
    use sha3::{Digest, Sha3_256};
    let mut hasher = Sha3_256::new();
    hasher.update(text.as_bytes());
    let result = hasher.finalize();
    let hashname = hex::encode(result);
    hashname
}

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

#[derive(Serialize, Deserialize)]
struct GQLTokenSignature {
    value: String,
    signature: String,
}

#[derive(Serialize, Deserialize)]
#[allow(non_snake_case)]
struct GQLTokenResponseData {
    streamPlaybackAccessToken: GQLTokenSignature,
}

#[derive(Serialize, Deserialize)]
struct GQLTokenResponse {
    data: GQLTokenResponseData,
}
async fn fetch_access_token_gql(
    client: &reqwest::Client,
    channel: &String,
) -> Result<TokenSig, anyhow::Error> {
    let access_token_addr = format!("https://gql.twitch.tv/gql");
    let token_sig = {
        let params = [("platform", "_")];
        let url = reqwest::Url::parse_with_params(&access_token_addr, &params)?;

        let is_live = true;
        let login = if is_live { channel } else { "" };
        let vod_id = if is_live { "" } else { "" };
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

async fn fetch_m3u8_by_channel(
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
        if false {
            let h = sha3(&text);
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

#[derive(Copy, Clone, Debug)]
#[derive(PartialEq)]
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
            }else{
                let _ = stderr!("copy_channel_to_out ending (not flagging end channel)\n");
            }
        });
    }
    txbufs
}

#[tokio::main]
async fn main() -> Result<(), anyhow::Error> {
    use clap::{App, Arg};
    let matches = App::new("twitch low latency")
        .version("0.1.0")
        .author("Szperak")
        .about("Lowest possible m3u8 latency, acquires playlist url, async segment fetching")
        .arg(
            Arg::with_name("out")
                .short('O')
                .long("out")
                .takes_value(true)
                .multiple(true)
                .min_values(1)
                .help("File path or 'out' for stdout, 'ffplay' for player window"),
        )
        .arg(
            Arg::with_name("m3u8")
                .short('p')
                .long("playlist")
                .takes_value(true)
                .help("Playlist url"),
        )
        .arg(
            Arg::with_name("channel")
                .short('c')
                .long("channel")
                .takes_value(true)
                .help("Twitch channel url"),
        )
        .get_matches();

    let def = vec!["ffplay"];
    let mut out_names: Vec<&str> = matches
        .values_of("out")
        .map(|v| v.collect::<Vec<_>>())
        .unwrap_or(def);

    let mut m3u8playlist_url = matches.value_of("m3u8").unwrap_or("null").to_string();
    let channel = matches.value_of("channel").unwrap_or("monstercat");

    let client = reqwest::Client::builder()
        .build()
        .expect("should be able to build reqwest client");

    let ffplay_location = Rc::new(RefCell::new(FFplayLocation::NotChecked));
    // // TODO: shorten but check only once
    // if out_names.contains(&"ffplay") || out_names.contains(&"audio") || out_names.contains(&"video")
    // {
    //     ffplay_location = ensure_ffplay(&client).await?;
    // }

    if m3u8playlist_url == "null" {
        m3u8playlist_url = fetch_m3u8_by_channel(&client, channel.to_string()).await?;
    }

    stderr!("url: {}\n", m3u8playlist_url)?;

    let local = tokio::task::LocalSet::new();
    // Run the local task set.
    local
        .run_until(async move {
            let copy_ended = Rc::new(RefCell::new(false));

            let result = ordered_download(
                client,
                &mut out_names,
                copy_ended,
                m3u8playlist_url,
                channel,
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
    mut out_names: &mut Vec<&str>,
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
