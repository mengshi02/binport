use crate::shell_quote;
use std::ffi::OsStr;
use std::io;

pub fn file_size(path: &str) -> io::Result<String> {
    Ok(format!(
        "sh -c 'test -f \"$1\" || exit 66; wc -c <\"$1\"' sh {}",
        shell_quote(OsStr::new(path))?
    ))
}

pub fn download_file(path: &str) -> io::Result<String> {
    Ok(format!(
        "sh -c 'test -f \"$1\" || exit 66; exec cat -- \"$1\"' sh {}",
        shell_quote(OsStr::new(path))?
    ))
}

pub fn upload_file(path: &str, source_name: &str) -> io::Result<String> {
    Ok(format!(
        "sh -c 'umask 077; dest=$1; case \"$dest\" in */) dest=${{dest%/}}/$2;; *) if [ -d \"$dest\" ]; then dest=$dest/$2; fi;; esac; dir=$(dirname -- \"$dest\") || exit; mkdir -p -- \"$dir\" || exit; tmp=$dest.binport-part.$$; trap '\"'\"'rm -f -- \"$tmp\"'\"'\"' EXIT HUP INT TERM; cat >\"$tmp\" && mv -f -- \"$tmp\" \"$dest\"; status=$?; trap - EXIT; exit $status' sh {} {}",
        shell_quote(OsStr::new(path))?,
        shell_quote(OsStr::new(source_name))?
    ))
}

pub fn remove(path: &str, recursive: bool, force: bool) -> io::Result<String> {
    let mode = if recursive { "recursive" } else { "file" };
    let force = if force { "force" } else { "normal" };
    Ok(format!(
        "sh -c 'path=$1; if [ -d \"$path\" ] && [ ! -L \"$path\" ]; then [ \"$2\" = recursive ] || {{ printf \"refusing to remove directory without --recursive: %s\\n\" \"$path\" >&2; exit 64; }}; if [ \"$3\" = force ]; then exec rm -rf -- \"$path\"; else exec rm -r -- \"$path\"; fi; else if [ \"$3\" = force ]; then exec rm -f -- \"$path\"; else exec rm -- \"$path\"; fi; fi' sh {} {} {}",
        shell_quote(OsStr::new(path))?,
        shell_quote(OsStr::new(mode))?,
        shell_quote(OsStr::new(force))?
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_are_passed_as_data_not_shell_source() {
        let malicious = "/tmp/a;$(touch /tmp/binport-injected)'";
        for command in [
            file_size(malicious).unwrap(),
            download_file(malicious).unwrap(),
            upload_file(malicious, "a;bad").unwrap(),
            remove(malicious, true, false).unwrap(),
        ] {
            assert!(command.contains("'\\''"));
            assert!(!command.contains("path=/tmp"));
        }
    }
}
