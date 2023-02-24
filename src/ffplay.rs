use std::{cell::RefCell, rc::Rc};

use tokio::{fs::File, io::AsyncWriteExt};

use crate::{isav::IsAudioVideo, stderr};

#[derive(Copy, Clone, Debug, PartialEq)]
pub enum FFplayLocation {
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
            FFplayLocation::CurrentDir => "./ffplay",
            FFplayLocation::NotFound => "",
        }
    }
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

pub async fn spawn_ffplay(
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
            .arg("volume=0.4,atempo=1.02")
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
