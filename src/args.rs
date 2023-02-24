use clap::command;
use clap::Parser;

/// twitch low latency
#[derive(Parser, Debug)]
#[command(
    author = "7ERr0r",
    version,
    about,
    long_about = "Lowest possible m3u8 latency on twitch. Acquires playlist url, then downloads with async segment/bytes fetching"
)]
pub struct TwitchzeroArgs {
    /// File path or 'out' for stdout, 'ffplay' for player window
    #[arg(short, long)]
    pub out: Option<Vec<String>>,

    /// Playlist url
    #[arg(short, long)]
    pub playlist_m3u8: Option<String>,

    /// Twitch channel url
    pub channel: String,
}
