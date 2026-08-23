use std::path::{Path, PathBuf};

const IDENTITY: &str = "SternlyWordedAdventures";

/// Where a FUSED build writes — LOVE omits the "LOVE" folder when the game is
/// fused into its executable. The Steam build of this game does exactly that.
pub fn fused(appdata: &Path) -> PathBuf {
    appdata.join(IDENTITY)
}

/// Where an UNFUSED run writes — `lovec.exe <game_dir>`, which is how Diggle
/// launches the game. Note this is a DIFFERENT directory from `fused`, so Diggle
/// starts from a clean save rather than the Steam build's.
pub fn unfused(appdata: &Path) -> PathBuf {
    appdata.join("LOVE").join(IDENTITY)
}

/// Resolves the save directory.
///
/// `override_dir` wins outright when set. Otherwise `expect_unfused` selects the
/// path matching how we launch the game — true for Diggle's normal `lovec.exe`
/// launch. The directory need not exist yet: LOVE creates it on first save, and
/// on a clean unfused run it genuinely will not exist until then.
pub fn locate(
    override_dir: Option<PathBuf>, expect_unfused: bool,
) -> Result<PathBuf, crate::Error> {
    if let Some(dir) = override_dir {
        return Ok(dir);
    }
    let appdata = std::env::var("APPDATA").map_err(|_| crate::Error::NoAppData)?;
    let appdata = Path::new(&appdata);
    Ok(if expect_unfused { unfused(appdata) } else { fused(appdata) })
}

#[cfg(test)]
mod tests {
    use super::*;

    const APPDATA: &str = r"C:\Users\x\AppData\Roaming";

    #[test]
    fn fused_path_omits_the_love_folder() {
        // Verified against the real Steam install, which writes here.
        assert_eq!(
            fused(Path::new(APPDATA)),
            PathBuf::from(r"C:\Users\x\AppData\Roaming\SternlyWordedAdventures")
        );
    }

    #[test]
    fn unfused_path_includes_the_love_folder() {
        // This is where lovec.exe <game_dir> will write, which is how Diggle launches.
        assert_eq!(
            unfused(Path::new(APPDATA)),
            PathBuf::from(r"C:\Users\x\AppData\Roaming\LOVE\SternlyWordedAdventures")
        );
    }

    #[test]
    fn an_explicit_override_wins_over_both() {
        let want = PathBuf::from(r"E:\elsewhere");
        assert_eq!(locate(Some(want.clone()), true).unwrap(), want);
    }
}
