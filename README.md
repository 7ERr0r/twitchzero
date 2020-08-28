# twitchlink
Lowest possible latency on twitch with ffplay


Build
====
`curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh`

`git clone https://github.com/Szperak/twitchlink.git`

`cd twitchlink`

`cargo build --release`


Use
====

`.\target\release\twitchlink --channel monstercat`
