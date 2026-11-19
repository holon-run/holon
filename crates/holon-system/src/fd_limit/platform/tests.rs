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
