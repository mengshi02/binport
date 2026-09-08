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

pub fn download_directory(path: &str) -> io::Result<String> {
    Ok(format!(
        "sh -c 'test -d \"$1\" || exit 66; cd -- \"$1\" && exec tar -cf - .' sh {}",
        shell_quote(OsStr::new(path))?
    ))
}

pub fn upload_directory(path: &str) -> io::Result<String> {
    Ok(format!(
        "sh -c 'umask 077; mkdir -p -- \"$1\" && exec tar -xf - -C \"$1\"' sh {}",
        shell_quote(OsStr::new(path))?
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

pub fn list_directory(path: &str) -> io::Result<String> {
    Ok(format!(
        "sh -c 'test -d \"$1\" || exit 66; find \"$1\" -type f -printf '\"'\"'%s %P\\n'\"'\"'' sh {}",
        shell_quote(OsStr::new(path))?
    ))
}

pub fn list_directory_dirs(path: &str) -> io::Result<String> {
    Ok(format!(
        "sh -c 'test -d \"$1\" || exit 66; find \"$1\" -mindepth 1 -type d -printf '\"'\"'%P\\n'\"'\"'' sh {}",
        shell_quote(OsStr::new(path))?
    ))
}

pub fn create_directories(base: &str) -> io::Result<String> {
    Ok(format!(
        "sh -c 'base=$1; while IFS= read -r dir; do mkdir -p -- \"$base/$dir\" || exit; done' sh {}",
        shell_quote(OsStr::new(base))?
    ))
}

pub fn download_file_offset(path: &str, offset: u64) -> io::Result<String> {
    if offset == 0 {
        download_file(path)
    } else {
        Ok(format!(
            "sh -c 'test -f \"$1\" || exit 66; exec tail -c +{} -- \"$1\"' sh {}",
            offset + 1,
            shell_quote(OsStr::new(path))?
        ))
    }
}

pub fn upload_file_append(path: &str, source_name: &str, sync_dir: &str) -> io::Result<String> {
    Ok(format!(
        "sh -c 'umask 077; dest=$1; case \"$dest\" in */) dest=${{dest%/}}/$2;; *) if [ -d \"$dest\" ]; then dest=$dest/$2; fi;; esac; dir=$(dirname -- \"$dest\") || exit; mkdir -p -- \"$dir\" || exit; sync_dir=$3; mkdir -p -- \"$sync_dir\" || exit; partial=$sync_dir/$(basename -- \"$dest\").part; cat >>\"$partial\" && mv -f -- \"$partial\" \"$dest\"; status=$?; exit $status' sh {} {} {}",
        shell_quote(OsStr::new(path))?,
        shell_quote(OsStr::new(source_name))?,
        shell_quote(OsStr::new(sync_dir))?
    ))
}

pub fn check_partials(sync_dir: &str) -> io::Result<String> {
    Ok(format!(
        "sh -c 'test -d \"$1\" || exit 0; find \"$1\" -maxdepth 1 -name \"*.part\" -type f -printf '\"'\"'%s %f\\n'\"'\"'' sh {}",
        shell_quote(OsStr::new(sync_dir))?
    ))
}

pub fn download_chunk(path: &str, offset: u64, size: u64) -> io::Result<String> {
    Ok(format!(
        "sh -c 'test -f \"$1\" || exit 66; exec tail -c +{} -- \"$1\" | head -c {}' sh {}",
        offset + 1,
        size,
        shell_quote(OsStr::new(path))?
    ))
}

pub fn merge_chunks(chunk_dir: &str, dest: &str) -> io::Result<String> {
    Ok(format!(
        "sh -c 'chunk_dir=$1; dest=$2; dir=$(dirname -- \"$dest\") || exit; mkdir -p -- \"$dir\" || exit; tmp=$dest.binport-merge.$$; trap '\"'\"'rm -f -- \"$tmp\"'\"'\"' EXIT HUP INT TERM; i=0; while [ -f \"$chunk_dir/chunk.$i\" ]; do cat -- \"$chunk_dir/chunk.$i\" >> \"$tmp\" || exit; i=$((i + 1)); done; [ $i -gt 0 ] || {{ printf \"no chunks found in %s\\n\" \"$chunk_dir\" >&2; exit 1; }}; mv -f -- \"$tmp\" \"$dest\" && rm -rf -- \"$chunk_dir\"; status=$?; trap - EXIT; exit $status' sh {} {}",
        shell_quote(OsStr::new(chunk_dir))?,
        shell_quote(OsStr::new(dest))?
    ))
}

pub fn list_chunks(chunk_dir: &str) -> io::Result<String> {
    Ok(format!(
        "sh -c 'test -d \"$1\" || exit 0; find \"$1\" -maxdepth 1 -name \"chunk.*\" -type f -printf '\"'\"'%s %f\\n'\"'\"'' sh {}",
        shell_quote(OsStr::new(chunk_dir))?
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
            list_directory(malicious).unwrap(),
            list_directory_dirs(malicious).unwrap(),
            create_directories(malicious).unwrap(),
            download_file_offset(malicious, 100).unwrap(),
            upload_file_append(malicious, "a;bad", malicious).unwrap(),
            check_partials(malicious).unwrap(),
            download_chunk(malicious, 0, 1000).unwrap(),
            merge_chunks(malicious, malicious).unwrap(),
            list_chunks(malicious).unwrap(),
        ] {
            assert!(command.contains("'\\''"));
            assert!(!command.contains("path=/tmp"));
        }
    }
}
