use reqwest::header::ACCEPT;
use tokio::sync::mpsc::error::SendError;

use futures::future::FutureExt;
use futures::select;
use std::cell::RefCell;
use std::rc::Rc;

use std::collections::HashMap;

//use std::sync::Arc;
use tokio::fs::File;
use tokio::io::AsyncWrite;
use tokio::io::{self, AsyncWriteExt};
use tokio::sync::mpsc;
use tokio::sync::mpsc::Receiver;
use tokio::sync::mpsc::Sender;

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
struct TokenSig {
    token: String,
    sig: String,
}

//use tokio::sync::Mutex;

macro_rules! aerr {
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

//type WriteRef = Arc<Mutex<Box<dyn AsyncWrite + Unpin + Send>>>;
type OutBufRef = Sender<Vec<u8>>;

async fn fetch_segment(
    client: reqwest::Client,
    prefetch_url: String,
    mut out: OutBufRef,
    mut rxprevdone: Receiver<bool>,
) -> Result<(), anyhow::Error> {
    for _i in 0..2 {
        let mut res = client.get(&prefetch_url).send().await?;
        {
            let last_segment_timeout = std::time::Duration::from_millis(2000);
            match recv_timeout(&mut rxprevdone, last_segment_timeout).await {
                Err(_) => {
                    aerr!("\nsegment: last one timed out\n")?;
                }
                Ok(_) => {
                    aerr!("\nsegment: last one done\n")?;
                }
            }
            //let mut w = (*out).lock().await;
            // locks the whole segment i/o

            while let Some(chunk) = res.chunk().await? {
                //use tokio::prelude::*;
                //w.write_all(&chunk).await?;
                out.send((&chunk).to_vec()).await?;

                aerr!(".")?;
                //println!("Chunk: {:?}", chunk);
            }
            break;
        }
    }

    Ok(())
}

async fn reload_m3u8(
    used_segments: &mut HashMap<String, bool>,
    client: reqwest::Client,
    m3u8_url: String,
    out: OutBufRef,
    txdone: Sender<bool>,
    chain: &mut ChainSync,
) -> Result<(), anyhow::Error> {
    let res = client.get(&m3u8_url).send().await?;

    aerr!("reload_m3u8: Status: {}\n", res.status())?;

    let text = res.text().await?;

    //let max_in_flight = 100;
    //

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
            used_segments.insert(furl.to_string(), true);

            aerr!("reload_m3u8: 1 new segment\n")?;
            let cclient = client.clone();
            let cout = out.clone();
            let mut ctxdone = txdone.clone();

            let crxprevdone = chain.grab_receiver();
            chain.reset();
            let mut ctxprevdone = chain.txprevdone.clone();
            tokio::spawn(async move {
                let res = fetch_segment(cclient, furl, cout, crxprevdone).await;
                match res {
                    Err(err) => {
                        let _ = aerr!("fetch_segment err: {}", err);
                    }
                    _ => {}
                }
                let _ = ctxprevdone.send(true).await;
                let _ = ctxdone.send(true).await;
            });

        //let sleep_duration = std::time::Duration::from_millis(10);
        //tokio::time::delay_for(sleep_duration).await;
        } else {
            aerr!("reload_m3u8: duplicate segment, not fetching\n")?;
            dup_count += 1;
        }
    }

    if false {
        if dup_count == num_segments {
            let h = sha3(&text);
            let _ = aerr!("creating: {}.m3u8\n", h);
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

struct ChainSync {
    txprevdone: Sender<bool>,
    rxprevdone: Option<Receiver<bool>>,
}

impl ChainSync {
    async fn new() -> Result<Self, SendError<bool>> {
        let (mut txprevdone, rxprevdone) = mpsc::channel::<bool>(1);
        // init chain of prev-next segment sync
        txprevdone.send(true).await?;
        Ok(ChainSync {
            txprevdone,
            rxprevdone: Some(rxprevdone),
        })
    }

    fn reset(&mut self) {
        let (txprevdone, rxprevdone) = mpsc::channel::<bool>(1);
        self.txprevdone = txprevdone;
        self.rxprevdone = Some(rxprevdone);
    }

    fn grab_receiver(&mut self) -> Receiver<bool> {
        let mut crxnextdone: Option<Receiver<bool>> = None;
        std::mem::swap(&mut self.rxprevdone, &mut crxnextdone);

        crxnextdone.unwrap()
    }
}

async fn copy_channel_to_out(
    mut rxbuf: Receiver<Vec<u8>>,
    mut out: Box<dyn AsyncWrite + Unpin>,
) -> Result<(), anyhow::Error> {
    loop {
        let out_bytes_vec = rxbuf.recv().await;
        match out_bytes_vec {
            Some(bytes_vec) => {
                //aerr!("+")?;
                out.write_all(&bytes_vec).await?;
            }
            None => {
                break;
            }
        }
    }
    Ok(())
}

async fn reload_loop(
    client: reqwest::Client,
    m3u8playlist_url: String,
    txbuf: Sender<Vec<u8>>,
    copy_ended: Rc<RefCell<bool>>,
) -> Result<(), anyhow::Error> {
    let (txdone, mut rxdone) = mpsc::channel::<bool>(10);

    let mut chain = ChainSync::new().await?;

    let mut used_segments: HashMap<String, bool> = HashMap::new();
    let sleep_duration = std::time::Duration::from_millis(2000);

    while !*copy_ended.borrow() {
        let reload_result = reload_m3u8(
            &mut used_segments,
            client.clone(),
            m3u8playlist_url.clone(),
            txbuf.clone(),
            txdone.clone(),
            &mut chain,
        )
        .await;
        match reload_result {
            Err(reload_err) => {
                aerr!("reload_err: {}\n", reload_err)?;
            }
            Ok(_) => {}
        }

        match recv_timeout(&mut rxdone, sleep_duration).await {
            Err(_) => {
                aerr!("\nmain: sleep ready\n")?;
            }
            Ok(_) => {
                aerr!("\nmain: done channel ready\n")?;
            }
        }
    }
    Ok(())
}

async fn fetch_m3u8_by_channel(
    client: &reqwest::Client,
    channel: String,
) -> Result<String, anyhow::Error> {
    let access_token_addr = format!(
        "https://api.twitch.tv/api/channels/{}/access_token",
        channel
    );
    let token_sig = {
        let url = reqwest::Url::parse(&access_token_addr)?;
        let mut req = reqwest::Request::new(reqwest::Method::GET, url);
        let h = req.headers_mut();
        h.insert(ACCEPT, "application/vnd.twitchtv.v3+json".parse().unwrap());
        h.insert(
            "Client-ID",
            "kimne78kx3ncx6brgo4mv6wki5h1ko".parse().unwrap(),
        );

        let res = client.execute(req).await?;
        aerr!("access_token: Status: {}\n", res.status())?;

        let bytes_vec = res.bytes().await?;
        aerr!("access_token: {}\n", String::from_utf8_lossy(&bytes_vec))?;

        let token_sig: TokenSig = serde_json::from_slice(&bytes_vec)?;
        token_sig
    };

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
        aerr!("channel url: {}\n", url)?;
        let req = reqwest::Request::new(reqwest::Method::GET, url);
        let res = client.execute(req).await?;
        aerr!("channel: Status: {}\n", res.status())?;

        let text = res.text().await?;
        if false {
            let h = sha3(&text);
            let _ = aerr!("creating: {}.m3u8\n", h);
            let mut file = File::create(format!("m3u8/{}.m3u8", h)).await?;
            file.write_all(&text.as_bytes()).await?;
        }

        aerr!("channel: {}\n", &text)?;

        let segments_vec = extract_segments(&text);

        segments_vec.get(0).unwrap().to_string()
    };

    Ok(playlist_url)
}

async fn ensure_ffplay(client: &reqwest::Client) -> Result<(), anyhow::Error> {
    let ffpath = ".\\ffplay.exe";
    aerr!("Checking for {}\n", ffpath)?;
    let attr_res = tokio::fs::metadata(ffpath).await;
    match attr_res {
        Ok(attr) => {
            if attr.is_file() {
                return Ok(());
            }
            return Err(TwitchlinkError::FileIsNotFFplay.into());
        }
        Err(err) => {
            if err.kind() != std::io::ErrorKind::NotFound {
                return Err(err.into());
            } else {
                aerr!("NotFound: {}\n", ffpath)?;
            }
        }
    }
    aerr!("Downloading ffplay\n")?;
    let res = client
        .get("https://github.com/Szperak/twitchlink/blob/master/ffplay.zip?raw=true")
        .send()
        .await?;
    let bytes_vec = res.bytes().await?;

    let reader = std::io::Cursor::new(bytes_vec);
    let mut zip = zip::ZipArchive::new(reader)?;
    for i in 0..zip.len() {
        let mut file = zip.by_index(i).unwrap();
        aerr!("Extracting filename: {}\n", file.name())?;

        use std::io::prelude::*;
        //let contents = file.bytes();
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

        aerr!("Extracted filename: {}\n", file.name())?;
    }

    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), anyhow::Error> {
    use clap::{App, Arg};
    let matches = App::new("twitch low latency")
        .version("0.1.0")
        .author("Szperak")
        .about("Lowest possible m3u8 latency, acquires playlist url, async segment fetching")
        .arg(
            Arg::with_name("file")
                .short("f")
                .long("file")
                .takes_value(true)
                .help("File path or 'out' for stdout, 'ffplay' for player window"),
        )
        .arg(
            Arg::with_name("m3u8")
                .short("p")
                .long("playlist")
                .takes_value(true)
                .help("Playlist url"),
        )
        .arg(
            Arg::with_name("channel")
                .short("c")
                .long("channel")
                .takes_value(true)
                .help("Twitch channel url"),
        )
        .get_matches();

    let filepath = matches.value_of("file").unwrap_or("ffplay");
    let mut m3u8playlist_url = matches.value_of("m3u8").unwrap_or("null").to_string();
    let channel = matches
        .value_of("channel")
        .unwrap_or("georgehotz");

    let client = reqwest::Client::builder()
        .build()
        .expect("should be able to build reqwest client");

    if filepath == "ffplay" {
        ensure_ffplay(&client).await?;
    }

    if m3u8playlist_url == "null" {
        m3u8playlist_url = fetch_m3u8_by_channel(&client, channel.to_string()).await?;
    }
    aerr!("url: {}\n", m3u8playlist_url)?;

    let local = tokio::task::LocalSet::new();
    // Run the local task set.
    local
        .run_until(async move {
            let copy_ended = Rc::new(RefCell::new(false));
            let copy_endedc = copy_ended.clone();
            let out: Box<dyn AsyncWrite + Unpin>;

            if filepath == "ffplay" {
                // ffplay -window_title "%channel%" -probesize 32 -sync ext -af volume=0.3 -vf setpts=0.5*PTS -x 1280 -y 720 -i -

                use std::process::Stdio;
                use tokio::process::Command;

                let mut cmd = Box::new(Command::new(".\\ffplay"));

                cmd.arg("-window_title")
                    .arg(format!("{} lowest latency", channel))
                    .arg("-probesize")
                    .arg("32")
                    .arg("-sync")
                    .arg("ext")
                    .arg("-vf")
                    .arg("setpts=0.5*PTS")
                    .arg("-x")
                    .arg("1280")
                    .arg("-y")
                    .arg("720")
                    .arg("-i")
                    .arg("-")
                    .stderr(Stdio::null())
                    .stdin(Stdio::piped());

                let mut child = cmd.spawn().expect("failed to spawn command");

                let stdin = child
                    .stdin
                    .take()
                    .expect("child did not have a handle to stdin");

                let process_ended = copy_ended.clone();
                tokio::task::spawn_local(async move {
                    let status = child.await.expect("child process encountered an error");

                    let _ = aerr!("child status was: {}", status);
                    *process_ended.borrow_mut() = true;
                });

                out = Box::new(stdin);
            } else if filepath == "out" {
                out = Box::new(io::stdout());
            } else {
                out = Box::new(File::create(filepath).await.unwrap());
            }

            let (txbuf, rxbuf) = mpsc::channel::<Vec<u8>>(1024 * 16);

            tokio::task::spawn_local(async move {
                let res = copy_channel_to_out(rxbuf, out).await;
                match res {
                    Err(err) => {
                        let _ = aerr!("copy_channel_to_out err: {}\n", err);
                    }
                    Ok(_) => {}
                }
                let _ = aerr!("copy_channel_to_out ending\n");
                *copy_endedc.borrow_mut() = true;
            });

            tokio::task::spawn_local(async move {
                reload_loop(client, m3u8playlist_url, txbuf, copy_ended)
                    .await
                    .unwrap();
            })
            .await
        })
        .await?;

    Ok(())
}

async fn recv_timeout<T>(
    rx: &mut Receiver<T>,
    sleep_duration: std::time::Duration,
) -> Result<Option<T>, ()> {
    let sleep = async { tokio::time::delay_for(sleep_duration).await };
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
}
impl std::error::Error for TwitchlinkError {}
