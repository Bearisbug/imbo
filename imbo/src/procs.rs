//! Other processes on this computer: whether they run, their parents and
//! names, the files they have open. Tools that give their commands no
//! session ID are recognised through these (ADR-018).

use std::path::PathBuf;

pub fn alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    let r = unsafe { libc::kill(pid as i32, 0) };
    r == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

/// Parent of `pid`; `None` once the process is gone.
#[cfg(target_os = "macos")]
pub fn parent(pid: u32) -> Option<u32> {
    bsd_info(pid).map(|i| i.pbi_ppid)
}

#[cfg(not(target_os = "macos"))]
pub fn parent(pid: u32) -> Option<u32> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    // The name in parentheses may hold spaces; the parent comes two fields
    // after it.
    let rest = &stat[stat.rfind(')')? + 1..];
    rest.split_whitespace().nth(1)?.parse().ok()
}

/// When `pid` started, in seconds since 1970; tells a process from a later
/// one that got the same pid.
#[cfg(target_os = "macos")]
pub fn started(pid: u32) -> Option<u64> {
    bsd_info(pid).map(|i| i.pbi_start_tvsec)
}

#[cfg(not(target_os = "macos"))]
pub fn started(_pid: u32) -> Option<u64> {
    None
}

/// Short name of the program `pid` runs, like `zsh` or `node`.
#[cfg(target_os = "macos")]
pub fn name(pid: u32) -> Option<String> {
    let info = bsd_info(pid)?;
    let raw: Vec<u8> = info
        .pbi_name
        .iter()
        .chain(info.pbi_comm.iter())
        .map(|c| *c as u8)
        .collect();
    let name = raw
        .split(|b| *b == 0)
        .find(|s| !s.is_empty())
        .map(|s| String::from_utf8_lossy(s).into_owned())?;
    Some(name)
}

#[cfg(not(target_os = "macos"))]
pub fn name(pid: u32) -> Option<String> {
    let s = std::fs::read_to_string(format!("/proc/{pid}/comm")).ok()?;
    Some(s.trim().to_string())
}

/// The arguments `pid` was started with, its program first; empty once the
/// process is gone.
#[cfg(target_os = "macos")]
pub fn args(pid: u32) -> Vec<String> {
    let mut mib = [libc::CTL_KERN, libc::KERN_PROCARGS2, pid as libc::c_int];
    let mut size: libc::size_t = 0;
    // SAFETY: a null buffer asks for the size; the second call writes at
    // most `size` bytes into `buf`.
    let sized = unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            3,
            std::ptr::null_mut(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    if sized != 0 {
        return vec![];
    }
    let mut buf = vec![0u8; size];
    let read = unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            3,
            buf.as_mut_ptr().cast(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    if read != 0 || size < 4 {
        return vec![];
    }
    buf.truncate(size);
    // The count of arguments, the program's path, padding, then the
    // arguments, each ended by a zero byte.
    let argc = i32::from_ne_bytes([buf[0], buf[1], buf[2], buf[3]]).max(0) as usize;
    let rest = &buf[4..];
    let path_end = rest.iter().position(|b| *b == 0).unwrap_or(rest.len());
    let rest = &rest[path_end..];
    let start = rest.iter().position(|b| *b != 0).unwrap_or(rest.len());
    rest[start..]
        .split(|b| *b == 0)
        .take(argc)
        .map(|s| String::from_utf8_lossy(s).into_owned())
        .collect()
}

#[cfg(not(target_os = "macos"))]
pub fn args(pid: u32) -> Vec<String> {
    let Ok(raw) = std::fs::read(format!("/proc/{pid}/cmdline")) else {
        return vec![];
    };
    raw.strip_suffix(&[0])
        .unwrap_or(&raw)
        .split(|b| *b == 0)
        .map(|s| String::from_utf8_lossy(s).into_owned())
        .collect()
}

#[cfg(target_os = "macos")]
fn bsd_info(pid: u32) -> Option<libc::proc_bsdinfo> {
    let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of::<libc::proc_bsdinfo>() as i32;
    // SAFETY: proc_pidinfo writes at most `size` bytes into `info`.
    let n = unsafe {
        libc::proc_pidinfo(
            pid as i32,
            libc::PROC_PIDTBSDINFO,
            0,
            (&mut info as *mut libc::proc_bsdinfo).cast(),
            size,
        )
    };
    (n == size).then_some(info)
}

/// This process's parent, grandparent and so on, nearest first, up to but
/// not including launchd/init.
pub fn ancestors() -> Vec<u32> {
    let mut out = vec![];
    let mut pid = std::process::id();
    while let Some(p) = parent(pid) {
        if p <= 1 || out.len() >= 64 {
            break;
        }
        out.push(p);
        pid = p;
    }
    out
}

/// Every process this user can see.
#[cfg(target_os = "macos")]
pub fn pids() -> Vec<u32> {
    // SAFETY: a null buffer asks for the count; the second call writes at
    // most `cap` pids into `buf`.
    let n = unsafe { libc::proc_listallpids(std::ptr::null_mut(), 0) };
    if n <= 0 {
        return vec![];
    }
    let cap = n as usize + 64;
    let mut buf = vec![0 as libc::c_int; cap];
    let size = (cap * std::mem::size_of::<libc::c_int>()) as libc::c_int;
    let n = unsafe { libc::proc_listallpids(buf.as_mut_ptr().cast(), size) };
    buf.truncate(n.max(0) as usize);
    buf.into_iter()
        .filter(|p| *p > 0)
        .map(|p| p as u32)
        .collect()
}

#[cfg(not(target_os = "macos"))]
pub fn pids() -> Vec<u32> {
    let Ok(dir) = std::fs::read_dir("/proc") else {
        return vec![];
    };
    dir.filter_map(|e| e.ok()?.file_name().to_str()?.parse().ok())
        .collect()
}

/// The directory `pid` works in.
#[cfg(target_os = "macos")]
pub fn cwd(pid: u32) -> Option<PathBuf> {
    let mut info: libc::proc_vnodepathinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of::<libc::proc_vnodepathinfo>() as i32;
    // SAFETY: proc_pidinfo writes at most `size` bytes into `info`.
    let n = unsafe {
        libc::proc_pidinfo(
            pid as i32,
            libc::PROC_PIDVNODEPATHINFO,
            0,
            (&mut info as *mut libc::proc_vnodepathinfo).cast(),
            size,
        )
    };
    if n != size {
        return None;
    }
    let raw: Vec<u8> = info
        .pvi_cdir
        .vip_path
        .iter()
        .flatten()
        .map(|c| *c as u8)
        .take_while(|b| *b != 0)
        .collect();
    (!raw.is_empty()).then(|| PathBuf::from(String::from_utf8_lossy(&raw).into_owned()))
}

#[cfg(not(target_os = "macos"))]
pub fn cwd(pid: u32) -> Option<PathBuf> {
    std::fs::read_link(format!("/proc/{pid}/cwd")).ok()
}

/// The program `pid` runs; `None` once the process is gone.
#[cfg(target_os = "macos")]
pub fn exe(pid: u32) -> Option<PathBuf> {
    let mut buf = vec![0u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
    // SAFETY: proc_pidpath writes at most `buf.len()` bytes and returns how
    // many it wrote.
    let n = unsafe { libc::proc_pidpath(pid as i32, buf.as_mut_ptr().cast(), buf.len() as u32) };
    if n <= 0 {
        return None;
    }
    buf.truncate(n as usize);
    Some(PathBuf::from(String::from_utf8(buf).ok()?))
}

#[cfg(not(target_os = "macos"))]
pub fn exe(pid: u32) -> Option<PathBuf> {
    std::fs::read_link(format!("/proc/{pid}/exe")).ok()
}

/// Shells sit between a tool and the commands and hooks it runs.
pub fn is_shell(pid: u32) -> bool {
    name(pid).is_some_and(|n| {
        matches!(
            n.trim_start_matches('-'),
            "sh" | "bash" | "zsh" | "dash" | "fish"
        )
    })
}

/// Files `pid` has open.
#[cfg(target_os = "macos")]
pub fn open_files(pid: u32) -> Vec<PathBuf> {
    let out = std::process::Command::new("/usr/sbin/lsof")
        .args(["-a", "-n", "-P", "-p", &pid.to_string(), "-Fn"])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output();
    let Ok(out) = out else { return vec![] };
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| l.strip_prefix('n'))
        .filter(|p| p.starts_with('/'))
        .map(PathBuf::from)
        .collect()
}

#[cfg(not(target_os = "macos"))]
pub fn open_files(pid: u32) -> Vec<PathBuf> {
    let Ok(dir) = std::fs::read_dir(format!("/proc/{pid}/fd")) else {
        return vec![];
    };
    dir.filter_map(|e| std::fs::read_link(e.ok()?.path()).ok())
        .filter(|p| p.is_absolute())
        .collect()
}

/// The file this process's standard output goes to, when it is a file.
#[cfg(target_os = "macos")]
pub fn stdout_path() -> Option<PathBuf> {
    let mut buf = vec![0u8; libc::PATH_MAX as usize];
    // SAFETY: F_GETPATH writes a NUL-terminated path of at most PATH_MAX bytes.
    let r = unsafe { libc::fcntl(1, libc::F_GETPATH, buf.as_mut_ptr()) };
    if r == -1 {
        return None;
    }
    let len = buf.iter().position(|b| *b == 0)?;
    buf.truncate(len);
    Some(PathBuf::from(String::from_utf8(buf).ok()?))
}

#[cfg(not(target_os = "macos"))]
pub fn stdout_path() -> Option<PathBuf> {
    std::fs::read_link("/proc/self/fd/1")
        .ok()
        .filter(|p| p.is_absolute())
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_process_names_its_own_program() {
        let me = std::fs::canonicalize(std::env::current_exe().unwrap()).unwrap();
        let found = super::exe(std::process::id()).unwrap();
        assert_eq!(std::fs::canonicalize(found).unwrap(), me);
    }
}
