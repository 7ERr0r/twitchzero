mod custom_m3u8;
mod downloader;
mod ffplay;
mod gql;
mod isav;
mod model;
mod outsender;
mod outwriter;
mod tzerror;
mod utils;

#[cfg(feature = "uiwindow")]
mod ui;

use crate::tzerror::TwitchzeroError;
use clap::command;
use clap::Parser;
use custom_m3u8::fetch_m3u8_by_channel;
use downloader::ordered_download;
use ffplay::FFplayLocation;
use std::cell::RefCell;
use std::rc::Rc;

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
