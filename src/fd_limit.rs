use std::io;

pub const DEFAULT_NOFILE_TARGET: u64 = 8192;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileDescriptorLimit {
    pub soft: u64,
    pub hard: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileDescriptorLimitReport {
    pub target: u64,
    pub before: Option<FileDescriptorLimit>,
    pub after: Option<FileDescriptorLimit>,
    pub raised: bool,
    pub unsupported: bool,
    pub error: Option<String>,
}

impl FileDescriptorLimitReport {
    pub fn startup_summary(&self) -> String {
        if self.unsupported {
            return "fd limits: unsupported on this platform".into();
        }
        if let Some(error) = &self.error {
            return format!("fd limits: failed to inspect or raise NOFILE limit: {error}");
        }

        let Some(before) = self.before else {
            return "fd limits: unavailable".into();
        };
        let after = self.after.unwrap_or(before);
        let action = if self.raised {
            format!("raised soft limit from {} to {}", before.soft, after.soft)
        } else if after.soft < self.target && after.hard < self.target {
            format!(
                "soft limit remains {} because OS hard limit {} is below target {}",
                after.soft, after.hard, self.target
            )
        } else {
            format!("soft limit already {}", after.soft)
        };
        format!(
            "fd limits: {action}; hard limit {}; target {}",
            after.hard, self.target
        )
    }
}

pub fn apply_nofile_limit_policy(target: u64) -> FileDescriptorLimitReport {
    platform::apply_nofile_limit_policy(target)
}

pub fn is_fd_exhaustion_error(error: &io::Error) -> bool {
    platform::is_fd_exhaustion_error(error)
}

#[cfg(unix)]
mod platform {
    use super::{FileDescriptorLimit, FileDescriptorLimitReport};
    use std::io;

    pub(super) fn apply_nofile_limit_policy(target: u64) -> FileDescriptorLimitReport {
        let before = match current_nofile_limit() {
            Ok(limit) => limit,
            Err(error) => {
                return FileDescriptorLimitReport {
                    target,
                    before: None,
                    after: None,
                    raised: false,
                    unsupported: false,
                    error: Some(error.to_string()),
                };
            }
        };

        let desired_soft = desired_soft_limit(before.soft, before.hard, target);
        let mut raised = false;
        let mut error = None;
        if desired_soft > before.soft {
            let rlim = libc::rlimit {
                rlim_cur: desired_soft as libc::rlim_t,
                rlim_max: before.hard as libc::rlim_t,
            };
            // SAFETY: setrlimit reads the provided rlimit value and does not retain pointers.
            let result = unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &rlim) };
            if result == 0 {
                raised = true;
            } else {
                error = Some(io::Error::last_os_error().to_string());
            }
        }

        let after = current_nofile_limit().ok();
        FileDescriptorLimitReport {
            target,
            before: Some(before),
            after,
            raised,
            unsupported: false,
            error,
        }
    }

    fn current_nofile_limit() -> io::Result<FileDescriptorLimit> {
        let mut limit = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        // SAFETY: getrlimit writes into the provided rlimit struct.
        let result = unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut limit) };
        if result != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(FileDescriptorLimit {
            soft: limit.rlim_cur as u64,
            hard: limit.rlim_max as u64,
        })
    }

    fn desired_soft_limit(soft: u64, hard: u64, target: u64) -> u64 {
        soft.max(target.min(hard))
    }

    pub(super) fn is_fd_exhaustion_error(error: &io::Error) -> bool {
        matches!(
            error.raw_os_error(),
            Some(code) if code == libc::EMFILE || code == libc::ENFILE
        )
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn desired_soft_limit_raises_to_target_when_hard_allows() {
            assert_eq!(desired_soft_limit(256, 65_536, 8192), 8192);
        }

        #[test]
        fn desired_soft_limit_caps_at_hard_limit() {
            assert_eq!(desired_soft_limit(256, 1024, 8192), 1024);
        }

        #[test]
        fn fd_exhaustion_error_matches_emfile_and_enfile() {
            assert!(is_fd_exhaustion_error(&io::Error::from_raw_os_error(
                libc::EMFILE
            )));
            assert!(is_fd_exhaustion_error(&io::Error::from_raw_os_error(
                libc::ENFILE
            )));
            assert!(!is_fd_exhaustion_error(&io::Error::from_raw_os_error(
                libc::ECONNREFUSED
            )));
        }
    }
}

#[cfg(not(unix))]
mod platform {
    use super::FileDescriptorLimitReport;
    use std::io;

    pub(super) fn apply_nofile_limit_policy(target: u64) -> FileDescriptorLimitReport {
        FileDescriptorLimitReport {
            target,
            before: None,
            after: None,
            raised: false,
            unsupported: true,
            error: None,
        }
    }

    pub(super) fn is_fd_exhaustion_error(_error: &io::Error) -> bool {
        false
    }
}
