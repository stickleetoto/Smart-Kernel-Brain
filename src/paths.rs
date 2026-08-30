use std::env;
use std::io;
use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct SkbPaths {
    pub home: PathBuf,
    pub index: PathBuf,
    pub state: PathBuf,
}

impl SkbPaths {
    pub fn discover() -> io::Result<Self> {
        let home = if let Ok(explicit) = env::var("SKB_HOME") {
            PathBuf::from(explicit)
        } else if cfg!(windows) {
            let base = env::var_os("LOCALAPPDATA")
                .or_else(|| env::var_os("USERPROFILE"))
                .map(PathBuf::from)
                .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "cannot determine SKB home"))?;
            base.join("SKB")
        } else {
            let base = env::var_os("XDG_DATA_HOME")
                .map(PathBuf::from)
                .or_else(|| env::var_os("HOME").map(|v| PathBuf::from(v).join(".local/share")))
                .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "cannot determine SKB home"))?;
            base.join("skb")
        };

        Ok(Self {
            index: home.join("index.skb"),
            state: home.join("state.json"),
            home,
        })
    }

    pub fn ensure_home(&self) -> io::Result<()> {
        std::fs::create_dir_all(&self.home)
    }
}
