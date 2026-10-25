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
