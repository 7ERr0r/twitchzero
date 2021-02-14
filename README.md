# twitchzero
Lowest possible latency on twitch with ffplay

Similar to streamlink, but without bugs and slow+heavy python. 
Fetches m3u8 URL and joins the segments to the stdout at realtime.
Resistant to connection (eg. wifi) loss.


Build
====
`curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh`

`git clone https://github.com/Szperak/twitchlink.git`

`cd twitchlink`

`cargo build --release`


Use
====

`.\target\release\twitchlink --channel monstercat`


```
OPTIONS:
    -c, --channel <channel>    Twitch channel url
    -f, --file <file>          File path or 'out' for stdout, 'ffplay' for player window
    -p, --playlist <m3u8>      Playlist url, overwrites twitch m3u8 resolver
```
