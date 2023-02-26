use std::{cell::RefCell, rc::Rc};

use tokio::{
    fs::File,
    io::{self, AsyncWrite, AsyncWriteExt},
    sync::mpsc::{self, Receiver},
};

use crate::{
    ffplay::spawn_ffplay, isav::IsAudioVideo, outsender::OutputStreamSender, stderr,
    tzerror::TwitchzeroError, FFplayLocation,
};

async fn copy_channel_to_out(
    mut rxbuf: Receiver<Rc<Vec<u8>>>,
    out: &mut AsyncOutput,
) -> Result<(), anyhow::Error> {
    loop {
        let msg = rxbuf.recv().await;
        match msg {
            Some(bytes_vec) => {
                out.writer.write_all(&bytes_vec).await?;
            }
            // None from .recv()
            // because channel is closed
            None => {
                break;
            }
        }
        while let Ok(msg) = rxbuf.try_recv() {
            out.writer.write_all(&msg).await?;
        }
        out.writer.flush().await?;
    }
    Ok(())
}

pub async fn make_outs(
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
                if let Some(addr) = other_out.strip_prefix(TCP_O) {
                    use tokio::net::TcpStream;
                    let stream = TcpStream::connect(addr)
                        .await
                        .map_err(TwitchzeroError::TCPConnect)?;
                    outs.push(AsyncOutput::new(Box::new(stream)));
                } else if let Some(addr) = other_out.strip_prefix(UNIX_O) {
                    #[cfg(target_os = "linux")]
                    {
                        use tokio::net::UnixStream;

                        let stream = UnixStream::connect(addr)
                            .await
                            .map_err(|err| TwitchzeroError::UnixConnectError(err))?;
                        outs.push(Box::new(stream));
                    }
                    #[cfg(not(target_os = "linux"))]
                    {
                        let _ = addr;
                        return Err(TwitchzeroError::NoUnixSocket.into());
                    }
                } else {
                    let file = File::create(other_out)
                        .await
                        .map_err(TwitchzeroError::FileCreate)?;
                    outs.push(AsyncOutput::new(Box::new(file)));
                }
            }
        }
    }

    Ok(outs)
}

pub struct AsyncOutput {
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

pub fn make_out_writers(
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
            if let Err(err) = res {
                let _ = stderr!("copy_channel_to_out err: {}\n", err);
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
