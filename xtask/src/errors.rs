use os_xtask_utils::CommandExt;
pub use rootcause::option_ext::OptionExt;
pub use rootcause::prelude::*;
use std::fmt::Display;

#[derive(Debug)]
pub(crate) enum XError {
    EnumParse {
        type_name: &'static str,
        value: String,
    },
}

impl Display for XError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            XError::EnumParse { type_name, value } => {
                write!(f, "Parse {type_name} from {value} failed.")
            }
        }
    }
}

impl std::error::Error for XError {}

pub(crate) trait ExtRunner: CommandExt {
    fn run(&mut self) -> Result<(), Report> {
        let info = self.info();
        let status = self
            .as_mut()
            .status()
            .context(format!("Failed to execute command {:?}", info))?;
        if !status.success() {
            bail!("Command failed with code {:?}: {:?}", status.code(), info);
        }
        Ok(())
    }
}

impl<T: CommandExt> ExtRunner for T {}
