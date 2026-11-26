//! Lossless ERNIE folder preparation for a single-card host. A made MXFP4 stack has no
//! file runs for the CPU tier. The same upstream quantizer used for the F8M4 release
//! writes those runs once. The inference process can then map and lock them normally.
use serde_json::{Value, json};
use std::error::Error;
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

type Result<T> = std::result::Result<T, Box<dyn Error>>;

fn files(dir: &Path, keep: impl Fn(&Path) -> bool) -> Result<Vec<PathBuf>> {
    let mut paths = Vec::new();
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        if keep(&path) && path.is_file() {
            paths.push(path);
        }
    }
    paths.sort();
    Ok(paths)
}

fn stamp(path: &Path) -> Result<Value> {
    let file = File::open(path)?;
    let stat = file.metadata()?;
    let (volume, id) = os::file_identity(&file)?;
    let time = stat.modified()?.duration_since(UNIX_EPOCH)?;
    Ok(json!([
        path.file_name()
            .and_then(|n| n.to_str())
            .ok_or("invalid shard filename")?,
        stat.len(),
        time.as_secs(),
        time.subsec_nanos(),
        volume,
        id
    ]))
}

fn source(dir: &Path) -> Result<Value> {
    let mut assets = Vec::new();
    for path in files(dir, |p| {
        matches!(
            p.extension().and_then(|x| x.to_str()),
            Some("json" | "safetensors")
        )
    })? {
        let mut entry = stamp(&path)?;
        if path.extension().is_some_and(|e| e == "json") {
            entry = json!([entry, gguf::digest::sha256_file(&path)?]);
        }
        assets.push(entry);
    }
    Ok(json!({"version": 1, "source": dir, "assets": assets, "rule": RULE}))
}

const RULE: &str = r"\.weight$=fp8_b128";
const STEM: &str = "ERNIE-F8M4";
fn outputs(cache: &Path) -> Result<Vec<PathBuf>> {
    files(cache, |p| {
        p.extension().is_some_and(|x| x == "gguf")
            && p.file_stem()
                .and_then(|s| s.to_str())
                .is_some_and(|s| s == STEM || s.starts_with("ERNIE-F8M4-"))
    })
}

fn record(path: &Path, value: &Value) -> Result<()> {
    use std::io::Write;
    let temp = path.with_extension("writing");
    let mut file = File::create(&temp)?;
    file.write_all(&serde_json::to_vec(value)?)?;
    file.sync_all()?;
    drop(file);
    fs::rename(temp, path)?;
    Ok(())
}

/// Resolve a local vendor folder to a reusable GGUF. Plan-only probes never create files.
/// A lock serializes preparation; the complete marker is published only after the full split opens.
pub fn resolve(dir: &Path, cache: Option<&Path>, plan_only: bool) -> Result<PathBuf> {
    let dir = dir.canonicalize()?;
    let cache = match cache {
        Some(c) => c.to_owned(),
        None => {
            let mut name = dir
                .file_name()
                .ok_or("the model folder must have a name")?
                .to_owned();
            name.push(".bloomery-F8M4");
            dir.with_file_name(name)
        }
    };
    let ready = cache.join(".bloomery-complete.json");
    if plan_only && !ready.is_file() {
        return Ok(dir);
    }
    let cfg: Value = serde_json::from_slice(&fs::read(dir.join("config.json"))?)?;
    if cfg["model_type"] != "ernie5_5_moe" {
        return Err("expected model_type ernie5_5_moe in the ERNIE checkpoint folder".into());
    }
    if !plan_only {
        fs::create_dir_all(&cache)?;
    }
    if cache.canonicalize()? == dir {
        return Err("select a cache directory separate from the checkpoint folder".into());
    }
    let lock = File::options()
        .read(true)
        .write(true)
        .create(!plan_only)
        .truncate(false)
        .open(cache.join(".bloomery-prepare.lock"))?;
    lock.lock()?;
    let identity = source(&dir)?;
    let owner = cache.join(".bloomery-source.json");
    if owner.exists() {
        if serde_json::from_slice::<Value>(&fs::read(&owner)?)? != identity {
            return Err(format!(
                "{} belongs to a different or changed checkpoint. Select an empty cache directory.",
                cache.display()
            )
            .into());
        }
    } else {
        if plan_only || !outputs(&cache)?.is_empty() {
            return Err(format!("{} contains an unrecognized GGUF cache", cache.display()).into());
        }
        record(&owner, &identity)?;
    }
    if !ready.is_file() {
        let rule = cache.join("ernie-f8m4.rules");
        fs::write(&rule, format!("{RULE}\n"))?;
        eprintln!(
            "Preparing lossless ERNIE F8M4 shards in {}. The full Baidu checkpoint uses about 145 GiB. Completed shards are reusable.",
            cache.display()
        );
        // This is the upstream release recipe. Omitting --allow-requantize makes each
        // tensor a copy of the folder reader's native representation.
        let out = cache.join("ERNIE-F8M4.gguf");
        let args = [
            "--hf".to_owned(),
            dir.to_string_lossy().into_owned(),
            "--tensor-type-file".to_owned(),
            rule.to_string_lossy().into_owned(),
            "--cpu".to_owned(),
            "--keep-split".to_owned(),
            "--resume".to_owned(),
            out.to_string_lossy().into_owned(),
            "Q8_0".to_owned(),
        ];
        let quantize::cli::Parsed::Run(args) = quantize::cli::parse(args)? else {
            return Err("expected an ERNIE repack run".into());
        };
        let quantize::cli::Input::Hf(input) = &args.input else {
            return Err("expected an ERNIE checkpoint folder".into());
        };
        let prepared = quantize::run::prepare_from(&args, crate::quantize_source::open(input)?)?;
        quantize::run::check_room(&args, &prepared.plan)?;
        quantize::run(
            &args,
            prepared,
            &mut quantize::host::HostBackend::new(),
            &mut |line| eprintln!("{line}"),
        )?;
        let shards = outputs(&cache)?;
        let first = shards
            .first()
            .ok_or("quantizer produced no ERNIE GGUF shards")?;
        let model = gguf::Split::open(first)?;
        if model.architecture() != Some("ernie5_5") {
            return Err("prepared GGUF is not an ERNIE checkpoint".into());
        }
        if source(&dir)? != identity {
            return Err(
                "checkpoint changed during preparation; select an empty cache directory".into(),
            );
        }
        record(
            &ready,
            &serde_json::to_value(
                shards
                    .iter()
                    .map(|p| stamp(p))
                    .collect::<Result<Vec<_>>>()?,
            )?,
        )?;
    }
    let paths = outputs(&cache)?;
    let actual = serde_json::to_value(paths.iter().map(|p| stamp(p)).collect::<Result<Vec<_>>>()?)?;
    if actual != serde_json::from_slice::<Value>(&fs::read(ready)?)? {
        return Err(format!("incomplete or changed F8M4 shards in {}", cache.display()).into());
    }
    paths
        .into_iter()
        .next()
        .ok_or_else(|| "empty ERNIE GGUF cache".into())
}
