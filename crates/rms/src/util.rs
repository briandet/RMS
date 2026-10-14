//! Small shared helpers.

use percent_encoding::{percent_decode_str, utf8_percent_encode, AsciiSet, CONTROLS};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

pub fn now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

pub fn now_secs() -> i64 {
    now() as i64
}

/// Characters escaped in `file://` URLs by the reference server (`Slim::Utils::Misc::fileURLFromPath`):
/// everything outside the unreserved set plus `/`.
const FILE_URL_SET: &AsciiSet = &CONTROLS
    .add(b' ')
    .add(b'"')
    .add(b'#')
    .add(b'%')
    .add(b'<')
    .add(b'>')
    .add(b'?')
    .add(b'[')
    .add(b'\\')
    .add(b']')
    .add(b'^')
    .add(b'`')
    .add(b'{')
    .add(b'|')
    .add(b'}')
    .add(b';')
    .add(b'&')
    .add(b'=')
    .add(b'+')
    .add(b'$')
    .add(b',')
    .add(b':')
    .add(b'@');

/// `file:///path/to/file.flac` with the same escaping as the reference server.
pub fn file_url_from_path(path: &Path) -> String {
    let s = path.to_string_lossy();
    let s = s.replace('\\', "/");
    let s = s.strip_prefix('/').unwrap_or(&s).to_string();
    format!("file:///{}", utf8_percent_encode(&s, FILE_URL_SET))
}

/// Inverse of [`file_url_from_path`]; also strips a `#start-end` cue fragment.
pub fn path_from_file_url(url: &str) -> Option<PathBuf> {
    let rest = url.strip_prefix("file://")?;
    let rest = rest.split('#').next().unwrap_or(rest);
    let rest = rest.strip_prefix("localhost").unwrap_or(rest);
    let decoded = percent_decode_str(rest).decode_utf8_lossy().to_string();
    Some(PathBuf::from(decoded))
}

pub fn is_remote_url(url: &str) -> bool {
    !url.starts_with("file://") && url.contains("://")
}

/// URL-decode a CLI parameter (`%20` → space; `+` is *not* a space in the LMS CLI).
pub fn cli_unescape(s: &str) -> String {
    percent_decode_str(s).decode_utf8_lossy().into_owned()
}

/// URL-encode a CLI value like `URI::Escape::uri_escape_utf8` with the reference server's
/// unsafe set (everything but `A-Za-z0-9_.!~*'()-` … and we also keep `:` and `/` readable
/// is *not* done by LMS, so we escape them too).
pub fn cli_escape(s: &str) -> String {
    const SET: &AsciiSet = &CONTROLS
        .add(b' ')
        .add(b'"')
        .add(b'#')
        .add(b'%')
        .add(b'<')
        .add(b'>')
        .add(b'?')
        .add(b'[')
        .add(b'\\')
        .add(b']')
        .add(b'^')
        .add(b'`')
        .add(b'{')
        .add(b'|')
        .add(b'}')
        .add(b';')
        .add(b'/')
        .add(b':')
        .add(b'@')
        .add(b'&')
        .add(b'=')
        .add(b'+')
        .add(b'$')
        .add(b',');
    utf8_percent_encode(s, SET).to_string()
}

/// Lower-cased file extension without the dot.
pub fn extension(path: &Path) -> String {
    path.extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default()
}

/// Best guess of this host's LAN IP address (for `serverstatus ip`).
pub fn server_ip() -> String {
    local_ip_address::local_ip()
        .map(|ip| ip.to_string())
        .unwrap_or_else(|_| "127.0.0.1".to_string())
}

/// `tokio::spawn` with the future boxed.  Tokio's task machinery is generic over the future, so
/// each distinct future spawned directly gets its own copy of it (about 1600 lines of LLVM IR,
/// twice: one per scheduler); boxed, all spawns with the same output share one.  Compile time,
/// not run time: the task is allocated anyway.
pub fn spawn<T: Send + 'static>(
    f: impl std::future::Future<Output = T> + Send + 'static,
) -> tokio::task::JoinHandle<T> {
    let f: std::pin::Pin<Box<dyn std::future::Future<Output = T> + Send>> = Box::pin(f);
    #[allow(clippy::disallowed_methods)]
    tokio::spawn(f)
}

/// `tokio::task::spawn_blocking` with the closure and its result boxed, for the same reason as
/// [`spawn`]: all blocking tasks share one copy of the task machinery, whatever they return.
/// Like `spawn_blocking`, the closure starts at once, awaited or not.
pub fn spawn_blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> Blocking<T> {
    let f: Box<dyn FnOnce() -> Erased + Send> = Box::new(move || Box::new(f()) as Erased);
    Blocking {
        #[allow(clippy::disallowed_methods)]
        handle: tokio::task::spawn_blocking(f),
        result: std::marker::PhantomData,
    }
}

type Erased = Box<dyn std::any::Any + Send>;

/// The handle [`spawn_blocking`] returns: a `JoinHandle<T>`.
pub struct Blocking<T> {
    handle: tokio::task::JoinHandle<Erased>,
    result: std::marker::PhantomData<fn() -> T>,
}

impl<T: 'static> std::future::Future for Blocking<T> {
    type Output = Result<T, tokio::task::JoinError>;

    fn poll(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        std::pin::Pin::new(&mut self.handle)
            .poll(cx)
            .map(|r| r.map(|b| *b.downcast::<T>().expect("spawn_blocking result type")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_urls() {
        let p = Path::new("/music/Artist Name/Album #1/01 - Song?.flac");
        let u = file_url_from_path(p);
        assert_eq!(
            u,
            "file:///music/Artist%20Name/Album%20%231/01%20-%20Song%3F.flac"
        );
        assert_eq!(path_from_file_url(&u).unwrap(), p);
        assert!(is_remote_url("http://x/y.mp3"));
        assert!(!is_remote_url("file:///x.mp3"));
    }

    #[test]
    fn cli_escaping() {
        assert_eq!(cli_escape("Hello World/a:b"), "Hello%20World%2Fa%3Ab");
        assert_eq!(cli_unescape("Hello%20World"), "Hello World");
    }
}
