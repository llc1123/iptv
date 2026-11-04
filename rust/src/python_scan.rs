use std::fs;
use std::path::{Path, PathBuf};

use crate::preflight::{
    DEFAULT_GUIDE_CACHE, DEFAULT_HOST, DEFAULT_INTERFACE, DEFAULT_LOGO_CATALOG, PythonPreflight,
    PythonProcessSpec, StartupRefusal,
};

impl PythonPreflight {
    /// Scans numeric process directories without reading `/proc/*/environ`.
    ///
    /// # Errors
    /// Returns a fixed [`StartupRefusal`] when a Python candidate conflicts or
    /// when a required process field cannot be observed.
    pub fn inspect(&self) -> Result<(), StartupRefusal> {
        let entries =
            fs::read_dir(&self.proc_root).map_err(|_| StartupRefusal::ProcFieldInaccessible)?;
        let mut processes = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|_| StartupRefusal::ProcFieldInaccessible)?;
            let name = entry.file_name();
            let name = name.to_str().ok_or(StartupRefusal::ProcFieldInaccessible)?;
            if let Ok(pid) = name.parse::<u64>() {
                processes.push((pid, entry.path()));
            }
        }
        processes.sort_by_key(|(pid, _)| *pid);
        let Some(expected) = self.expected.as_ref() else {
            for (_, process) in processes {
                let argv = read_cmdline(&process)?;
                if is_unidentified_python_candidate(&argv) {
                    return Err(StartupRefusal::ProcFieldInaccessible);
                }
            }
            return Ok(());
        };
        let mut candidates = Vec::new();
        for (_, process) in processes {
            let argv = read_cmdline(&process)?;
            if !is_candidate(&argv, &expected.entrypoint) {
                continue;
            }
            if is_wrapper(&argv, &expected.entrypoint) {
                return Err(StartupRefusal::WrapperMismatch);
            }
            candidates.push(read_candidate(&process, argv, &expected.entrypoint)?);
        }
        match candidates.as_slice() {
            [] => Ok(()),
            [_] if candidates[0] == *expected => Err(StartupRefusal::PythonConflict),
            [_] => Err(StartupRefusal::PythonIdentityMismatch),
            _ => Err(StartupRefusal::MultiplePythonCandidates),
        }
    }
}

fn is_unidentified_python_candidate(argv: &[String]) -> bool {
    argv.iter().any(|argument| {
        let name = std::path::Path::new(argument)
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or_default();
        name.starts_with("python") || argument.ends_with("service_cli.py")
    })
}

fn read_cmdline(process: &Path) -> Result<Vec<String>, StartupRefusal> {
    let bytes =
        fs::read(process.join("cmdline")).map_err(|_| StartupRefusal::ProcFieldInaccessible)?;
    bytes
        .split(|byte| *byte == 0)
        .filter(|part| !part.is_empty())
        .map(|part| {
            String::from_utf8(part.to_vec()).map_err(|_| StartupRefusal::ProcFieldInaccessible)
        })
        .collect()
}

fn is_candidate(argv: &[String], entrypoint: &str) -> bool {
    argv.iter()
        .any(|argument| argument == entrypoint || argument.contains("iptv_client.service_cli"))
}

fn is_wrapper(argv: &[String], entrypoint: &str) -> bool {
    let Some(position) = argv.iter().position(|argument| argument == entrypoint) else {
        return true;
    };
    position != 0
        || argv
            .iter()
            .any(|argument| argument == "-m" || argument == "-c")
}

fn read_candidate(
    process: &Path,
    argv: Vec<String>,
    entrypoint: &str,
) -> Result<PythonProcessSpec, StartupRefusal> {
    let interpreter =
        fs::canonicalize(process.join("exe")).map_err(|_| StartupRefusal::ProcFieldInaccessible)?;
    let uid = read_uid(&process.join("status"))?;
    let cwd =
        fs::canonicalize(process.join("cwd")).map_err(|_| StartupRefusal::ProcFieldInaccessible)?;
    let interface = option(&argv, "--interface").unwrap_or_else(|| DEFAULT_INTERFACE.to_owned());
    let host = option(&argv, "--host").unwrap_or_else(|| DEFAULT_HOST.to_owned());
    let port = option(&argv, "--port")
        .unwrap_or_else(|| "8080".to_owned())
        .parse()
        .map_err(|_| StartupRefusal::PythonIdentityMismatch)?;
    let guide_cache = option(&argv, "--guide-cache")
        .map_or_else(|| PathBuf::from(DEFAULT_GUIDE_CACHE), PathBuf::from);
    let logo_catalog = option(&argv, "--logo-catalog")
        .map_or_else(|| PathBuf::from(DEFAULT_LOGO_CATALOG), PathBuf::from);
    let state_path = option(&argv, "--state-path")
        .or_else(|| option(&argv, "--state-dir"))
        .map_or_else(|| cwd.clone(), PathBuf::from);
    Ok(PythonProcessSpec::new(
        interpreter,
        entrypoint.to_owned(),
        uid,
        cwd,
        interface,
        host,
        port,
        guide_cache,
        logo_catalog,
        state_path,
        argv,
    ))
}

fn option(argv: &[String], name: &str) -> Option<String> {
    argv.windows(2)
        .find(|pair| pair[0] == name)
        .map(|pair| pair[1].clone())
}

fn read_uid(status: &Path) -> Result<u32, StartupRefusal> {
    let contents = fs::read_to_string(status).map_err(|_| StartupRefusal::ProcFieldInaccessible)?;
    let line = contents
        .lines()
        .find(|line| line.starts_with("Uid:"))
        .ok_or(StartupRefusal::ProcFieldInaccessible)?;
    line.split_whitespace()
        .nth(1)
        .ok_or(StartupRefusal::ProcFieldInaccessible)?
        .parse()
        .map_err(|_| StartupRefusal::ProcFieldInaccessible)
}
