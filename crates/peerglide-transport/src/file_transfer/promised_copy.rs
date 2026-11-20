//! Materialize an already accepted copy at the native drop destination.
use super::PreparedDropPath;
use anyhow::{ensure, Context, Result};
use rand::{rngs::OsRng, RngCore};
use std::{
    fs::{self, DirBuilder, File, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
};

impl PreparedDropPath {
    /// The local OS supplies the destination after the drop. The original open
    /// handle provides each verified chunk. Publication is one no-clobber link.
    pub fn copy_to_drop_destination(
        mut self,
        destination: &Path,
        mut check: impl FnMut() -> io::Result<()>,
    ) -> Result<()> {
        check()?;
        ensure!(
            destination.is_absolute(),
            "native drop requires an absolute destination"
        );
        let parent = destination
            .parent()
            .context("native drop has no parent directory")?;
        match fs::symlink_metadata(destination) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => (),
            Ok(_) => anyhow::bail!("native drop destination already exists"),
            Err(error) => return Err(error).context("cannot inspect native drop destination"),
        }

        let (stage, mut output) = Staging::create(parent)?;
        for index in 0..self.manifest.chunks() {
            check()?;
            let chunk = self.manifest.read_chunk(&mut self._source, index)?;
            output.write_all(&chunk)?;
        }
        ensure!(
            self._source.metadata()?.len() == self.manifest.size(),
            "prepared drop source size changed"
        );
        output.sync_all()?;
        drop(output);
        check()?;
        fs::hard_link(stage.file(), destination).context("cannot publish native drop copy")?;
        Ok(())
    }
}

struct Staging(PathBuf);

impl Staging {
    fn create(parent: &Path) -> Result<(Self, File)> {
        let mut nonce = [0u8; 16];
        OsRng
            .try_fill_bytes(&mut nonce)
            .map_err(|_| anyhow::anyhow!("native drop randomness unavailable"))?;
        let name = format!(".linko-drop-{:032x}", u128::from_ne_bytes(nonce));
        let root = parent.join(name);
        let mut directory = DirBuilder::new();
        directory.recursive(false);
        let mut file = OpenOptions::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
            directory.mode(0o700);
            file.mode(0o600);
        }
        directory.create(&root)?;
        let stage = Self(root);
        let file = file.write(true).create_new(true).open(stage.file())?;
        Ok((stage, file))
    }

    fn file(&self) -> PathBuf {
        self.0.join("content")
    }
}

impl Drop for Staging {
    fn drop(&mut self) {
        let _ = fs::remove_file(self.file());
        let _ = fs::remove_dir(&self.0);
    }
}
