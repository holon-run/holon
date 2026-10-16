use std::{
    ffi::OsString,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::PathBuf,
};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

use super::RuntimeControlAuthMode;
use crate::config::AppConfig;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ServeProfileAccess {
    Local,
    Tunnel,
    Lan,
    Tailnet,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "mode", rename_all = "snake_case", deny_unknown_fields)]
pub enum ServeProfileAuthentication {
    None,
    TokenFile { path: PathBuf },
    ExternalRequired,
}

/// Complete non-secret serve intent, independent of clap and process metadata.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DaemonServeProfile {
    pub schema_version: u32,
    pub access: ServeProfileAccess,
    pub host: Option<String>,
    pub listen: Option<String>,
    pub port: Option<u16>,
    pub advertise: Option<String>,
    pub desktop_integration: bool,
    pub web_dist: Option<PathBuf>,
    pub authentication: ServeProfileAuthentication,
    pub control_auth_mode: RuntimeControlAuthMode,
}

impl DaemonServeProfile {
    pub fn validate(&self) -> Result<()> {
        if self.schema_version != 1 {
            bail!("unsupported daemon serve profile schema version");
        }
        if self.listen.is_some() == self.port.is_some() || self.port == Some(0) {
            bail!("daemon serve profile must contain exactly one valid listen or port");
        }
        if let Some(listen) = &self.listen {
            listen
                .parse::<std::net::SocketAddr>()
                .context("invalid daemon serve profile listen address")?;
        }
        if self.host.as_ref().is_some_and(|host| {
            host.is_empty()
                || host.contains(['@', '/', '?', '#'])
                || host.chars().any(char::is_whitespace)
        }) {
            bail!("daemon serve profile host must be a host name or address without credentials");
        }
        if let Some(advertise) = &self.advertise {
            let url = reqwest::Url::parse(advertise)
                .context("invalid daemon serve profile advertise URL")?;
            if !matches!(url.scheme(), "http" | "https")
                || url.host_str().is_none()
                || !url.username().is_empty()
                || url.password().is_some()
                || url.query().is_some()
            {
                bail!("daemon serve profile advertise URL must be a non-secret HTTP base URL");
            }
        }
        if self
            .web_dist
            .as_ref()
            .is_some_and(|path| !path.is_absolute())
        {
            bail!("daemon serve profile web_dist must be absolute");
        }
        if let ServeProfileAuthentication::TokenFile { path } = &self.authentication {
            if !path.is_absolute() {
                bail!("daemon serve profile token file path must be absolute");
            }
        }
        Ok(())
    }
}

pub struct ResolvedDaemonServePlan {
    pub config: AppConfig,
    pub profile: DaemonServeProfile,
    pub serve_args: Vec<OsString>,
    pub control_token_env: Option<String>,
    pub startup_summary: String,
}

impl ResolvedDaemonServePlan {
    pub(crate) fn matches_effective_serve_options(&self, recorded: &Self) -> bool {
        self.config.http_addr == recorded.config.http_addr
            && self.profile.access == recorded.profile.access
            && self.profile.advertise == recorded.profile.advertise
            && self.profile.desktop_integration == recorded.profile.desktop_integration
            && self.profile.web_dist == recorded.profile.web_dist
            && self.profile.authentication == recorded.profile.authentication
            && self.profile.control_auth_mode == recorded.profile.control_auth_mode
    }
}

pub fn serve_profile_path(config: &AppConfig) -> PathBuf {
    config.home_dir.join("daemon").join("serve_profile.json")
}

fn profile_directory(config: &AppConfig, create: bool) -> Result<Option<File>> {
    let path = config.home_dir.join("daemon");
    if create {
        fs::create_dir_all(&config.home_dir)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            match fs::DirBuilder::new().mode(0o700).create(&path) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(error) => {
                    return Err(error).context("failed to create daemon profile directory")
                }
            }
        }
        #[cfg(not(unix))]
        fs::create_dir_all(&path)?;
        File::open(&config.home_dir)?.sync_all()?;
    }
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC);
    }
    let dir = match options.open(&path) {
        Ok(dir) => dir,
        Err(error) if !create && error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).context("cannot open daemon profile directory safely"),
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        if dir.metadata()?.uid() != unsafe { libc::geteuid() } {
            bail!("daemon profile directory is owned by another user");
        }
        if create {
            dir.set_permissions(fs::Permissions::from_mode(0o700))?;
        }
    }
    Ok(Some(dir))
}

#[cfg(unix)]
fn open_profile_at(dir: &File, name: &str, flags: i32) -> Result<File, std::io::Error> {
    use std::os::fd::{AsRawFd, FromRawFd};
    let name = std::ffi::CString::new(name).expect("profile filename contains no NUL");
    let fd = unsafe {
        libc::openat(
            dir.as_raw_fd(),
            name.as_ptr(),
            flags | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            0o600,
        )
    };
    if fd < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(unsafe { File::from_raw_fd(fd) })
}

pub fn load_serve_profile(config: &AppConfig) -> Result<Option<DaemonServeProfile>> {
    let Some(dir) = profile_directory(config, false)? else {
        return Ok(None);
    };
    #[cfg(unix)]
    let opened = open_profile_at(
        &dir,
        "serve_profile.json",
        libc::O_RDONLY | libc::O_NONBLOCK,
    );
    #[cfg(not(unix))]
    let opened = File::open(serve_profile_path(config));
    let mut file = match opened {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).context("cannot read daemon serve profile safely"),
    };
    if !file.metadata()?.is_file() || file.metadata()?.len() > 64 * 1024 {
        bail!("daemon serve profile must be a regular file of at most 64 KiB");
    }
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    let profile: DaemonServeProfile =
        serde_json::from_slice(&bytes).context("invalid daemon serve profile")?;
    profile.validate()?;
    Ok(Some(profile))
}

pub fn persist_serve_profile(config: &AppConfig, profile: &DaemonServeProfile) -> Result<()> {
    profile.validate()?;
    let dir = profile_directory(config, true)?.expect("created profile directory");
    // Refuse symlink/special-file targets even though rename itself would not follow them.
    #[cfg(unix)]
    match open_profile_at(
        &dir,
        "serve_profile.json",
        libc::O_RDONLY | libc::O_NONBLOCK,
    ) {
        Ok(file) if !file.metadata()?.is_file() => {
            bail!("daemon serve profile is not a regular file")
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error).context("cannot replace daemon serve profile safely"),
    }
    let name = format!(
        ".serve_profile.{}.{}.tmp",
        std::process::id(),
        chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
    );
    #[cfg(unix)]
    let mut file = open_profile_at(&dir, &name, libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL)?;
    #[cfg(not(unix))]
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(config.home_dir.join("daemon").join(&name))?;
    let result = (|| -> Result<()> {
        file.write_all(&serde_json::to_vec_pretty(profile)?)?;
        file.sync_all()?;
        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd;
            let source = std::ffi::CString::new(name.as_str())?;
            let target = c"serve_profile.json";
            if unsafe {
                libc::renameat(
                    dir.as_raw_fd(),
                    source.as_ptr(),
                    dir.as_raw_fd(),
                    target.as_ptr(),
                )
            } != 0
            {
                return Err(std::io::Error::last_os_error()).context("cannot commit serve profile");
            }
        }
        #[cfg(not(unix))]
        fs::rename(
            config.home_dir.join("daemon").join(&name),
            serve_profile_path(config),
        )?;
        dir.sync_all()
            .context("cannot fsync daemon profile directory")
    })();
    if result.is_err() {
        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd;
            if let Ok(name) = std::ffi::CString::new(name) {
                unsafe { libc::unlinkat(dir.as_raw_fd(), name.as_ptr(), 0) };
            }
        }
        #[cfg(not(unix))]
        let _ = fs::remove_file(config.home_dir.join("daemon").join(name));
    }
    result
}
