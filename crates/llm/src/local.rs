use std::fs::{self, File, OpenOptions};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use crate::openai::OpenAiRefiner;
use crate::{Error, Refiner, Request};

const READY_TIMEOUT: Duration = Duration::from_secs(60);
const READY_POLL: Duration = Duration::from_millis(100);

#[derive(Clone, Debug)]
pub struct LocalConfig {
    pub runtime_dir: PathBuf,
    pub log: PathBuf,
    pub state_dir: PathBuf,
    pub context: u32,
    pub keep_alive: Duration,
}

/// der server startet bei bedarf und stoppt, wenn die keep-alive-zeit abläuft.
pub struct LocalRefiner {
    config: LocalConfig,
    model: PathBuf,
    running: Option<Running>,
}

struct Running {
    child: Child,
    client: OpenAiRefiner,
    pid_path: PathBuf,
    last_used: Instant,
}

impl LocalRefiner {
    pub fn new(config: LocalConfig, model: PathBuf) -> Result<Self, Error> {
        if !model.is_file() {
            return Err(Error::Unavailable(format!("model not installed: {}", model.display())));
        }
        cleanup(&config.state_dir);
        Ok(Self { config, model, running: None })
    }

    fn ensure(&mut self) -> Result<(), Error> {
        if self.running.as_mut().is_some_and(|running| running.child.try_wait().ok().flatten().is_none()) {
            return Ok(());
        }
        self.shutdown();

        let server = find_executable(&self.config.runtime_dir, "llama-server").ok_or_else(|| {
            Error::Unavailable(format!("llama-server not found in {}", self.config.runtime_dir.display()))
        })?;
        fs::create_dir_all(&self.config.state_dir).map_err(io_error)?;
        let pid_path = self.config.state_dir.join("llama-server.pid");
        cleanup_stale(&pid_path);

        let port = free_port()?;
        let api_key = random_key()?;
        if let Some(parent) = self.config.log.parent() {
            let _ = fs::create_dir_all(parent);
        }
        let log = OpenOptions::new().create(true).append(true).open(&self.config.log).map_err(io_error)?;
        let mut command = Command::new(&server);
        command
            .arg("-m")
            .arg(&self.model)
            .args(["--host", "127.0.0.1", "--port"])
            .arg(port.to_string())
            .args(["-c", &self.config.context.to_string(), "-ngl", "999", "--api-key", &api_key])
            .stdin(Stdio::null())
            .stdout(Stdio::from(log.try_clone().map_err(io_error)?))
            .stderr(Stdio::from(log));
        if let Some(parent) = server.parent() {
            command.current_dir(parent);
            command.env("DYLD_LIBRARY_PATH", parent);
        }
        let mut child =
            command.spawn().map_err(|e| Error::Io(format!("could not start {}: {e}", server.display())))?;

        write_pidfile(&pid_path, child.id(), &server);
        if let Err(error) = wait_ready(&mut child, port, &api_key, &self.config.log) {
            terminate(&mut child);
            let _ = fs::remove_file(&pid_path);
            return Err(error);
        }

        let client = OpenAiRefiner::new(&format!("http://127.0.0.1:{port}/v1"), "local", Some(api_key))?
            .without_thinking();
        self.running = Some(Running { child, client, pid_path, last_used: Instant::now() });
        Ok(())
    }

    fn shutdown(&mut self) {
        if let Some(mut running) = self.running.take() {
            terminate(&mut running.child);
            let _ = fs::remove_file(&running.pid_path);
        }
    }
}

impl Refiner for LocalRefiner {
    fn refine(&mut self, request: &Request) -> Result<String, Error> {
        self.ensure()?;
        let Some(running) = self.running.as_mut() else {
            return Err(Error::Unavailable("llama-server did not start".into()));
        };
        let result = running.client.refine(request);
        running.last_used = Instant::now();
        result
    }

    fn warm(&mut self) {
        let _ = self.ensure();
    }

    fn tick(&mut self) {
        if let Some(running) = self.running.as_ref()
            && running.last_used.elapsed() >= self.config.keep_alive
        {
            self.shutdown();
        }
    }
}

impl Drop for LocalRefiner {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn wait_ready(child: &mut Child, port: u16, key: &str, log: &Path) -> Result<(), Error> {
    let deadline = Instant::now() + READY_TIMEOUT;
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_millis(500))
        .timeout_read(Duration::from_millis(500))
        .build();
    while Instant::now() < deadline {
        if let Ok(Some(status)) = child.try_wait() {
            return Err(Error::Io(format!("llama-server exited with {status}: {}", log_tail(log))));
        }
        match agent
            .get(&format!("http://127.0.0.1:{port}/health"))
            .set("Authorization", &format!("Bearer {key}"))
            .call()
        {
            Ok(response) if response.status() == 200 => return Ok(()),
            _ => std::thread::sleep(READY_POLL),
        }
    }
    Err(Error::Unavailable("llama-server did not become ready within 60 s".into()))
}

fn terminate(child: &mut Child) {
    let pid = child.id().to_string();
    let _ = Command::new("/bin/kill").args(["-TERM", &pid]).status();
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline {
        if let Ok(Some(_)) = child.try_wait() {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let _ = child.kill();
    let _ = child.wait();
}

fn write_pidfile(path: &Path, pid: u32, server: &Path) {
    let text = format!("{pid}\n{}\n", server.display());
    if let Err(e) = fs::write(path, text) {
        eprintln!("tf: could not write {}: {e}", path.display());
    }
}

/// killt einen server aus einem früheren crash, aber nur wenn die pid noch zu unserem llama-server-binary gehört.
pub(crate) fn cleanup(state_dir: &Path) {
    cleanup_stale(&state_dir.join("llama-server.pid"));
}

fn cleanup_stale(pid_path: &Path) {
    let Ok(text) = fs::read_to_string(pid_path) else {
        return;
    };
    let mut lines = text.lines();
    let Some(pid) = lines.next().and_then(|pid| pid.trim().parse::<u32>().ok()) else {
        let _ = fs::remove_file(pid_path);
        return;
    };
    let expected = lines.next().unwrap_or("llama-server");
    if let Some(actual) = process_path(pid)
        && (actual.contains(expected) || actual.contains("llama-server"))
    {
        let _ = Command::new("/bin/kill").args(["-TERM", &pid.to_string()]).status();
    }
    let _ = fs::remove_file(pid_path);
}

fn process_path(pid: u32) -> Option<String> {
    let output = Command::new("/bin/ps").args(["-p", &pid.to_string(), "-o", "comm="]).output().ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

fn log_tail(path: &Path) -> String {
    let Ok(text) = fs::read_to_string(path) else {
        return String::new();
    };
    let lines: Vec<&str> = text.lines().rev().take(3).collect();
    lines.into_iter().rev().collect::<Vec<_>>().join(" | ")
}

fn free_port() -> Result<u16, Error> {
    let listener = TcpListener::bind("127.0.0.1:0").map_err(io_error)?;
    listener.local_addr().map(|addr| addr.port()).map_err(io_error)
}

fn random_key() -> Result<String, Error> {
    let mut bytes = [0u8; 16];
    let mut file = File::open("/dev/urandom").map_err(io_error)?;
    use std::io::Read;
    file.read_exact(&mut bytes).map_err(io_error)?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

fn find_executable(root: &Path, name: &str) -> Option<PathBuf> {
    if !root.exists() {
        return None;
    }
    let mut stack = vec![(root.to_path_buf(), 0usize)];
    while let Some((dir, depth)) = stack.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_file() && entry.file_name() == name {
                return Some(path);
            }
            if kind.is_dir() && depth < 3 {
                stack.push((path, depth + 1));
            }
        }
    }
    None
}

fn io_error(error: std::io::Error) -> Error {
    Error::Io(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn free_ports_are_available() {
        assert!(free_port().unwrap() > 0);
    }

    #[test]
    fn executable_search_handles_nested_builds() {
        let root = std::env::temp_dir().join(format!("tf-llama-{}", std::process::id()));
        let bin = root.join("build/bin");
        fs::create_dir_all(&bin).unwrap();
        fs::write(bin.join("llama-server"), b"").unwrap();
        assert_eq!(find_executable(&root, "llama-server"), Some(bin.join("llama-server")));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn missing_model_is_reported() {
        let config = LocalConfig {
            runtime_dir: PathBuf::from("/nope"),
            log: PathBuf::from("/tmp/tf-llama.log"),
            state_dir: PathBuf::from("/tmp"),
            context: 4096,
            keep_alive: Duration::from_secs(1),
        };
        assert!(LocalRefiner::new(config, PathBuf::from("/nope/model.gguf")).is_err());
    }
}
