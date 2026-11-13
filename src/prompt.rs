//! Asking for a password in a terminal without showing it.
//!
//! The password is read from the controlling terminal (`/dev/tty`) with echo
//! off, so it never reaches the shell history, the process list or a log.
//! Without a terminal (a script), one line is read from standard input.

use std::io::{BufRead, Write};

use zeroize::Zeroizing;

/// Shows `label`, reads a password without echo and returns it (the line
/// break removed).
pub fn password(label: &str) -> Result<Zeroizing<String>, String> {
    #[cfg(unix)]
    if let Ok(tty) = std::fs::OpenOptions::new().read(true).write(true).open("/dev/tty") {
        return read_tty(tty, label);
    }
    eprint!("{label}");
    let _ = std::io::stderr().flush();
    read_line(&mut std::io::stdin().lock())
}

/// Asks twice and checks both match (a new password).
pub fn new_password(label: &str, min: usize) -> Result<Zeroizing<String>, String> {
    let first = password(label)?;
    if first.chars().count() < min {
        return Err(format!("Use a password of at least {min} characters."));
    }
    let second = password("Repeat it: ")?;
    if *first != *second {
        return Err("The two passwords differ.".into());
    }
    Ok(first)
}

fn read_line(input: &mut impl BufRead) -> Result<Zeroizing<String>, String> {
    let mut line = Zeroizing::new(String::new());
    input.read_line(&mut line).map_err(|e| format!("Could not read the password: {e}"))?;
    let trimmed = Zeroizing::new(line.trim_end_matches(['\n', '\r']).to_string());
    if trimmed.is_empty() {
        return Err("No password given.".into());
    }
    Ok(trimmed)
}

#[cfg(unix)]
fn read_tty(mut tty: std::fs::File, label: &str) -> Result<Zeroizing<String>, String> {
    use std::os::fd::AsRawFd;
    let fd = tty.as_raw_fd();
    let _ = write!(tty, "{label}");
    let _ = tty.flush();
    // SAFETY: termios is plain data; tcgetattr fills it for a valid fd.
    let mut old: libc::termios = unsafe { std::mem::zeroed() };
    // SAFETY: `fd` is the open terminal.
    let echo_off = unsafe { libc::tcgetattr(fd, &mut old) } == 0 && {
        let mut quiet = old;
        quiet.c_lflag &= !libc::ECHO;
        quiet.c_lflag |= libc::ECHONL;
        // SAFETY: as above, with a copy of the current settings.
        unsafe { libc::tcsetattr(fd, libc::TCSANOW, &quiet) == 0 }
    };
    let result = match tty.try_clone() {
        Ok(reader) => read_line(&mut std::io::BufReader::new(reader)),
        Err(e) => Err(format!("Could not read the password: {e}")),
    };
    if echo_off {
        // SAFETY: restores the settings read above.
        unsafe { libc::tcsetattr(fd, libc::TCSANOW, &old) };
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_line_without_its_break() {
        let mut input = std::io::Cursor::new(b"s3cret pass\r\nnext\n".to_vec());
        assert_eq!(read_line(&mut input).unwrap().as_str(), "s3cret pass");
        assert!(read_line(&mut std::io::Cursor::new(b"\n".to_vec())).is_err());
    }
}
