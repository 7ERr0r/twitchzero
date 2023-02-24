pub mod args;
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

use args::TwitchzeroArgs;
use clap::Parser;
use custom_m3u8::fetch_m3u8_by_channel;
use downloader::ordered_download;
use ffplay::FFplayLocation;
use std::cell::RefCell;
use std::rc::Rc;

#[cfg(not(feature = "uiwindow"))]
fn main() {
    twitchzero_cli_thread();
}

#[cfg(feature = "uiwindow")]
fn main() {
    use ui::ui_iced::ui_main;

    std::thread::spawn(twitchzero_cli_thread);
    ui_main().unwrap();
}

fn twitchzero_cli_thread() {
    let io_runtime = tokio::runtime::Builder::new_current_thread()
        .thread_name("twitchzero-io")
        .worker_threads(1)
        .enable_all()
        .build()
        .expect("runtime to build");

    let result = io_runtime.block_on(twitchzero_cli_main());
    if let Err(err) = result {
        println!("{}", err);
    }
}

async fn twitchzero_cli_main() -> Result<(), anyhow::Error> {
    let args = TwitchzeroArgs::parse();

    twitchzero_cli(&args).await
}
async fn twitchzero_cli(args: &TwitchzeroArgs) -> Result<(), anyhow::Error> {
    let out_names = args.out.clone().unwrap_or_else(|| vec!["ffplay".into()]);

    let client = reqwest::Client::builder()
        .build()
        .expect("should be able to build reqwest client");

    let ffplay_location = Rc::new(RefCell::new(FFplayLocation::NotChecked));
    // // TODO: shorten but check only once
    // if out_names.contains(&"ffplay") || out_names.contains(&"audio") || out_names.contains(&"video")
    // {
    //     ffplay_location = ensure_ffplay(&client).await?;
    // }

    let m3u8playlist_url = if let Some(playlist) = &args.playlist_m3u8 {
        playlist.clone()
    } else {
        fetch_m3u8_by_channel(&client, args.channel.to_string()).await?
    };

    stderr!("url: {:?}\n", m3u8playlist_url)?;

    let twitchzero_event_loop = ordered_download(
        client,
        out_names.iter().map(|s| s.as_ref()).collect(),
        m3u8playlist_url,
        &args.channel,
        ffplay_location,
    );
    let local = tokio::task::LocalSet::new();
    // Run the local task set.
    local.run_until(twitchzero_event_loop).await?;

    Ok(())
}
