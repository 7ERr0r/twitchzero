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

    #[allow(unused)]
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
