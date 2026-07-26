use crate::config::Config;
use std::process::{Child, ChildStdout, Command, Stdio};

pub struct GameProcess {
    child: Child,
}

/// Builds the launch command. Separated from `launch` so it is testable without
/// actually spawning the game.
///
/// `--verbose` enables _VERBOSE via argv (main.lua:37). We deliberately do NOT
/// create a `debug` file in the save directory: that would set t.console
/// (conf.lua:76), which calls AllocConsole() and detaches stdout from our pipe.
fn build_command(cfg: &Config) -> Command {
    let mut cmd = Command::new(&cfg.lovec_path);
    cmd.arg(&cfg.game_dir).arg("--verbose");
    cmd
}

impl GameProcess {
    pub fn launch(cfg: &Config) -> Result<Self, crate::Error> {
        Self::launch_with_env(cfg, &[])
    }

    /// Launch with extra environment variables for the child.
    ///
    /// SDL reads its hints from the environment, so this is how we influence SDL's
    /// input behaviour without modifying the game.
    pub fn launch_with_env(
        cfg: &Config, env: &[(&str, &str)],
    ) -> Result<Self, crate::Error> {
        let mut cmd = build_command(cfg);
        for (k, v) in env {
            cmd.env(k, v);
        }
        cmd.stdout(Stdio::piped()).stderr(Stdio::null());
        Ok(Self { child: cmd.spawn()? })
    }

    pub fn pid(&self) -> u32 {
        self.child.id()
    }

    /// Takes ownership of the stdout pipe. Callable once.
    pub fn stdout(&mut self) -> Option<ChildStdout> {
        self.child.stdout.take()
    }

    pub fn is_running(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    /// Terminates the game. Spikes and the loop must not leave stray processes.
    pub fn kill(&mut self) -> Result<(), crate::Error> {
        let _ = self.child.kill();
        let _ = self.child.wait();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn cfg() -> Config {
        Config {
            game_dir: PathBuf::from(r"C:\game"),
            lovec_path: PathBuf::from(r"C:\love\lovec.exe"),
            ..Default::default()
        }
    }

    #[test]
    fn command_passes_game_dir_and_verbose_flag() {
        let cmd = build_command(&cfg());
        let args: Vec<_> = cmd.get_args().map(|a| a.to_string_lossy().into_owned()).collect();
        assert_eq!(args, vec![r"C:\game".to_string(), "--verbose".to_string()]);
        assert_eq!(cmd.get_program().to_string_lossy(), r"C:\love\lovec.exe");
    }
}
