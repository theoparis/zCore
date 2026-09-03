use crate::errors::*;
use std::{ffi::OsStr, path::Path};

macro_rules! fetch_online {
    ($dst:expr, $f:expr) => {{
        use crate::errors::*;
        use os_xtask_utils::{dir, CommandExt};
        use std::{fs, path::PathBuf};

        let _ = dir::rm(&$dst);
        let tmp: usize = rand::random();
        let tmp = PathBuf::from("/tmp").join(tmp.to_string());
        let mut ext = $f(tmp.clone());
        let info = ext.info();
        let status = ext
            .as_mut()
            .status()
            .context(format!("Failed to spawn download command for {:?}", info))?;
        if status.success() {
            dir::create_parent(&$dst)
                .context(format!("Failed to create parent directory for {:?}", $dst))?;
            if tmp.is_dir() {
                dircpy::copy_dir(&tmp, &$dst).context(format!(
                    "Failed to copy directory from {tmp:?} to {:?}",
                    $dst
                ))?;
            } else {
                fs::copy(&tmp, &$dst)
                    .context(format!("Failed to copy file from {tmp:?} to {:?}", $dst))?;
            }
            let _ = dir::rm(tmp);
            Ok::<(), Report>(())
        } else {
            let _ = dir::rm(tmp);
            bail!(
                "Download command failed with code {:?} from {:?}",
                status.code(),
                info
            );
        }
    }};
}

pub(crate) use fetch_online;

pub(crate) fn wget(url: impl AsRef<OsStr>, dst: impl AsRef<Path>) -> Result<(), Report> {
    use os_xtask_utils::Ext;

    let dst = dst.as_ref();
    if dst.exists() {
        println!("{dst:?} already exist. You can delete it manually to re-download.");
        return Ok(());
    }

    println!("wget {} from {:?}", dst.display(), url.as_ref());
    fetch_online!(dst, |tmp| {
        if std::process::Command::new("wget")
            .arg("--version")
            .output()
            .is_ok()
        {
            let mut wget = Ext::new("wget");
            wget.arg(&url).arg("-O").arg(tmp);
            wget
        } else {
            let mut curl = Ext::new("curl");
            curl.arg("-fSL").arg(&url).arg("-o").arg(tmp);
            curl
        }
    })
}
