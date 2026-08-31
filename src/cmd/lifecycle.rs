use binport::catalog::{self, Platform};
use binport::toolbox;
use clap::Args;
use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::io;
use std::path::PathBuf;

#[derive(Debug, Args)]
pub struct ProjectArgs {
    #[arg(default_value = ".")]
    path: PathBuf,
}

#[derive(Debug, Args)]
pub struct BuildArgs {
    #[arg(default_value = ".")]
    path: PathBuf,
    #[arg(short = 'f', long, default_value = "Binfile")]
    file: PathBuf,
}

#[derive(Debug, Args)]
pub struct FetchArgs {
    #[arg(required_unless_present = "all")]
    tools: Vec<String>,
    #[arg(long)]
    all: bool,
    #[arg(long, default_value = "linux/amd64")]
    target: String,
}

#[derive(Debug, Args)]
pub struct TransferArgs {
    /// Toolbox archive to write or read
    file: PathBuf,
    /// Project containing .binport
    #[arg(long, default_value = ".")]
    path: PathBuf,
}

pub fn resolve(args: BuildArgs) -> io::Result<u8> {
    let root = args.path.canonicalize()?;
    let binfile = if args.file.is_absolute() {
        args.file
    } else {
        root.join(args.file)
    };
    let lock = binport::lockfile::resolve(&root, &binfile)?;
    println!(
        "Resolved {} artifacts into {}",
        lock.tools.len() + lock.copies.len(),
        root.join(binport::lockfile::LOCKFILE_NAME).display()
    );
    Ok(0)
}

pub fn build(args: BuildArgs) -> io::Result<u8> {
    let root = args.path.canonicalize()?;
    let binfile = if args.file.is_absolute() {
        args.file
    } else {
        root.join(args.file)
    };
    let lock = toolbox::build(&root, &binfile)?;
    println!("\nToolbox built: {} artifacts", lock.tools.len());
    println!("Manifest: {}", root.join(".binport/toolbox.json").display());
    Ok(0)
}

pub fn list(args: ProjectArgs) -> io::Result<u8> {
    let binfile = args.path.join("Binfile");
    let mut rows = Vec::new();
    if binfile.is_file() {
        let spec = binport::binfile::Binfile::read(&binfile)?;
        let platforms = spec
            .platforms
            .iter()
            .map(|p| p.name())
            .collect::<Vec<_>>()
            .join(", ");
        for tool in spec.tools {
            let version = tool.version.unwrap_or_else(|| {
                catalog::tools()
                    .iter()
                    .find(|entry| entry.name == tool.name)
                    .map(|entry| entry.version.clone())
                    .unwrap_or_else(|| "latest".into())
            });
            rows.push(vec![
                tool.name.clone(),
                catalog::replacement(&tool.name).into(),
                catalog::description(&tool.name).into(),
                version,
                platforms.clone(),
            ]);
        }
        for copy in spec.copies {
            rows.push(vec![
                copy.name,
                "-".into(),
                "custom tool".into(),
                "local".into(),
                copy.platform.name().into(),
            ]);
        }
    } else {
        let lock: toolbox::Lockfile =
            serde_json::from_slice(&fs::read(args.path.join(".binport/toolbox.json"))?)
                .map_err(io::Error::other)?;
        let mut tools: BTreeMap<(String, String), Vec<String>> = BTreeMap::new();
        for tool in lock.tools {
            tools
                .entry((tool.name, tool.version))
                .or_default()
                .push(tool.platform);
        }
        for ((name, version), mut platforms) in tools {
            platforms.sort();
            rows.push(vec![
                name.clone(),
                catalog::replacement(&name).into(),
                catalog::description(&name).into(),
                version,
                platforms.join(", "),
            ]);
        }
    }
    print!(
        "{}",
        super::table::render(
            &["TOOL", "REPLACES", "DESCRIPTION", "VERSION", "PLATFORMS"],
            &rows,
        )
    );
    Ok(0)
}

pub fn fetch(args: FetchArgs) -> io::Result<u8> {
    let platform = Platform::parse(&args.target).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("unsupported target {:?}", args.target),
        )
    })?;
    let tools: Vec<String> = if args.all {
        catalog::tools()
            .iter()
            .map(|entry| entry.name.clone())
            .collect()
    } else {
        args.tools
    };
    for tool in tools {
        println!("{}\t{}", tool, toolbox::fetch(&tool, platform)?.display());
    }
    Ok(0)
}

pub fn status(args: ProjectArgs) -> io::Result<u8> {
    let binfile = args.path.join("Binfile");
    let manifest = args.path.join(".binport/toolbox.json");
    if binfile.is_file() {
        let spec = binport::binfile::Binfile::read(&binfile)?;
        println!("Binfile:    {}", binfile.display());
        println!("Tools:      {}", spec.tools.len() + spec.copies.len());
        println!(
            "Platforms:  {}",
            spec.platforms
                .iter()
                .map(|p| p.name())
                .collect::<Vec<_>>()
                .join(", ")
        );
    } else {
        let lock: toolbox::Lockfile =
            serde_json::from_slice(&fs::read(&manifest)?).map_err(io::Error::other)?;
        let names = lock
            .tools
            .iter()
            .map(|tool| tool.name.as_str())
            .collect::<HashSet<_>>();
        let mut platforms = lock
            .tools
            .iter()
            .map(|tool| tool.platform.as_str())
            .collect::<HashSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        platforms.sort();
        println!("Binfile:    not present (imported toolbox)");
        println!("Tools:      {}", names.len());
        println!("Platforms:  {}", platforms.join(", "));
    }
    println!(
        "Lock:       {}",
        if args.path.join(binport::lockfile::LOCKFILE_NAME).is_file() {
            args.path
                .join(binport::lockfile::LOCKFILE_NAME)
                .display()
                .to_string()
        } else {
            "no".into()
        }
    );
    println!(
        "Built:      {}",
        if manifest.is_file() {
            manifest.display().to_string()
        } else {
            "no".into()
        }
    );
    println!("Cache:      {}", toolbox::cache_root()?.display());
    Ok(0)
}

pub fn clean() -> io::Result<u8> {
    let root = toolbox::cache_root()?;
    if root.exists() {
        fs::remove_dir_all(&root)?;
        println!("Removed {}", root.display());
    } else {
        println!("Cache is already empty");
    }
    Ok(0)
}

pub fn export(args: TransferArgs) -> io::Result<u8> {
    let root = args.path.canonicalize()?;
    let output = if args.file.is_absolute() {
        args.file
    } else {
        std::env::current_dir()?.join(args.file)
    };
    toolbox::export(&root, &output)?;
    println!("Exported {}", output.display());
    Ok(0)
}

pub fn load(args: TransferArgs) -> io::Result<u8> {
    let root = args.path.canonicalize()?;
    let input = args.file.canonicalize()?;
    let lock = toolbox::load(&root, &input)?;
    println!(
        "Loaded {} artifacts into {}",
        lock.tools.len(),
        root.display()
    );
    Ok(0)
}

pub fn pack(args: TransferArgs) -> io::Result<u8> {
    let root = args.path.canonicalize()?;
    let output = if args.file.is_absolute() {
        args.file
    } else {
        std::env::current_dir()?.join(args.file)
    };
    binport::oci::pack(&root, &output)?;
    println!("Packed OCI toolbox into {}", output.display());
    Ok(0)
}

pub fn unpack(args: TransferArgs) -> io::Result<u8> {
    let root = args.path.canonicalize()?;
    let input = args.file.canonicalize()?;
    let lock = binport::oci::unpack(&input, &root)?;
    println!(
        "Unpacked {} artifacts into {}",
        lock.tools.len(),
        root.display()
    );
    Ok(0)
}
