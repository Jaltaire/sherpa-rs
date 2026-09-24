use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

use serde::Deserialize;
use serde_json::Value;

// Prebuilt sherpa-onnx doesn't have Cuda support
#[cfg(all(
    any(target_os = "windows", target_os = "linux"),
    feature = "download-binaries",
    feature = "cuda"
))]
compile_error!(
    "The 'download-binaries' and 'cuda' features cannot be enabled at the same time.\n\
    To resolve this, please disable the 'download-binaries' feature when using 'cuda'.\n\
    For example, in your Cargo.toml:\n\
    [dependencies]\n\
    sherpa-rs = { default-features = false, features = [\"cuda\"] }"
);

// Prebuilt sherpa-onnx doesn't have DirectML support
#[cfg(all(windows, feature = "download-binaries", feature = "directml"))]
compile_error!(
    "The 'download-binaries' and 'directml' features cannot be enabled at the same time.\n\
    To resolve this, please disable the 'download-binaries' feature when using 'directml'.\n\
    For example, in your Cargo.toml:\n\
    [dependencies]\n\
    sherpa-rs = { default-features = false, features = [\"directml\"] }"
);

// Prebuilt sherpa-onnx does not include TTS in static builds.
#[cfg(all(
    windows,
    feature = "download-binaries",
    feature = "static",
    feature = "tts"
))]
compile_error!(
    "The 'download-binaries', 'static', and 'tts' features cannot be enabled at the same time.\n\
    To resolve this, please disable the 'tts' feature when using 'static' and 'download-binaries' together.\n\
    For example, in your Cargo.toml:\n\
    [dependencies]\n\
    sherpa-rs = { default-features = false, features = [\"static\", \"tts\"] }"
);

macro_rules! debug_log {
    ($($arg:tt)*) => {
        // SHERPA_BUILD_DEBUG=1 cargo build
        if std::env::var("SHERPA_BUILD_DEBUG").unwrap_or_default() == "1" {
            println!("cargo:warning=[DEBUG] {}", format!($($arg)*));
        }
    };
}

pub fn fetch_file(source_url: &str) -> Vec<u8> {
    let resp = ureq::AgentBuilder::new()
        .try_proxy_from_env(true)
        .build()
        .get(source_url)
        .timeout(std::time::Duration::from_secs(1800))
        .call()
        .unwrap_or_else(|err| panic!("Failed to GET `{source_url}`: {err}"));

    let len = resp
        .header("Content-Length")
        .and_then(|s| s.parse::<usize>().ok())
        .expect("Content-Length header should be present on archive response");
    debug_log!("Fetch file {} {}", source_url, len);
    let mut reader = resp.into_reader();
    let mut buffer = Vec::new();
    reader
        .read_to_end(&mut buffer)
        .unwrap_or_else(|err| panic!("Failed to download from `{source_url}`: {err}"));
    assert_eq!(buffer.len(), len);
    buffer
}

static DIST_CONTENT: &str = include_str!("../dist.json");
static DIST_CHECKSUM_CONTENT: &str = include_str!("../checksum.txt");
lazy_static::lazy_static! {
    pub static ref DIST_TABLE: DistTable = DistTable::new(DIST_CONTENT);
    pub static ref DIST_CHECKSUM: HashMap<String, String> = {
        DIST_CHECKSUM_CONTENT
            .lines()
            .map(|line| {
                let mut parts = line.split_whitespace();
                let key = parts.next().unwrap().to_string();
                let value = parts.next().unwrap().to_string();
                (key, value)
            })
            .collect()
    };
}

#[derive(Debug, Deserialize)]
pub struct DistTable {
    pub tag: String,
    pub url: String,
    pub targets: HashMap<String, Value>,
}

#[derive(Debug, Clone)]
pub struct Archive {
    pub url: String,
    pub checksum: String,
}

impl Archive {
    pub fn file_name(&self) -> &str {
        self.url.rsplit('/').next().unwrap_or(&self.url)
    }
}

#[derive(Debug, Clone)]
pub struct Dist {
    pub archives: Vec<Archive>,
    pub name: String,
    pub checksum: String,
    pub libs: Option<Vec<String>>, // Paths to the extracted libraries
}

impl DistTable {
    fn new(content: &str) -> Self {
        let mut table: DistTable = serde_json::from_str(content)
            .unwrap_or_else(|_| panic!("Failed to parse dist.json: {content}"));
        table.url = table.url.replace("{tag}", &table.tag);
        for value in table.targets.values_mut() {
            // expand static with {tag}
            if let Some(static_value) = value.get("static") {
                let static_value = static_value.as_str().unwrap();
                value["static"] = Value::String(static_value.replace("{tag}", &table.tag));
            }
            // expand dynamic with {tag}
            if let Some(dynamic_value) = value.get("dynamic") {
                let dynamic_value = dynamic_value.as_str().unwrap();
                value["dynamic"] = Value::String(dynamic_value.replace("{tag}", &table.tag));
            }
            // expand archive with {tag}
            if let Some(archive_value) = value.get("archive") {
                let archive_value = archive_value.as_str().unwrap();
                value["archive"] = Value::String(archive_value.replace("{tag}", &table.tag));
            }
            if let Some(archives) = value.get("archives") {
                let expanded: Vec<Value> = archives
                    .as_array()
                    .expect("archives must be a list of addresses")
                    .iter()
                    .map(|url| Value::String(url.as_str().unwrap().replace("{tag}", &table.tag)))
                    .collect();
                value["archives"] = Value::Array(expanded);
            }
        }
        table
    }

    pub fn get(&self, target: &str, is_dynamic: &mut bool) -> Option<Dist> {
        debug_log!("Extracting dist for target: {}", target);
        // debug_log!("dist table: {:?}", self);
        let target_dist = if target.contains("android") {
            self.targets.get("android").unwrap()
        } else if target.contains("ios") {
            self.targets.get("ios").unwrap()
        } else {
            self.targets
                .get(target)
                .unwrap_or_else(||
                    panic!("Target {target} not found. try to disable download-feature with --no-default-features.")
                )
        };
        debug_log!(
            "raw target_dist: {:?}",
            serde_json::to_string(target_dist).unwrap()
        );
        if let Some(addresses) = target_dist.get("archives") {
            let mut archives = Vec::new();
            for url in addresses.as_array()? {
                let url = url.as_str()?.to_string();
                let file_name = url.rsplit('/').next()?.to_string();
                let checksum = DIST_CHECKSUM.get(&file_name)?;
                archives.push(Archive {
                    url,
                    checksum: checksum.to_string(),
                });
            }
            let libs: Option<Vec<String>> = target_dist["targets"][target].as_array().map(|libs| {
                libs.iter()
                    .map(|lib| lib.as_str().unwrap().to_string())
                    .collect()
            });
            if let Some(target_dist) = target_dist.get("is_dynamic") {
                *is_dynamic = target_dist.as_bool()?;
            }
            let joined: Vec<&str> = archives
                .iter()
                .map(|archive| archive.checksum.as_str())
                .collect();
            let checksum = if archives.len() == 1 {
                archives[0].checksum.clone()
            } else {
                sha256(joined.join("\n").as_bytes())
            };
            let name = archives.first()?.file_name().replace(".zip", "");
            let dist = Dist {
                archives,
                name,
                checksum,
                libs,
            };
            debug_log!("dist: {:?}", dist);
            return Some(dist);
        }

        let archive = if target_dist.get("archive").is_some() {
            // archive name
            // static/dynamic located in 'is_dynamic' field
            target_dist.get("archive").unwrap().as_str().unwrap()
        } else if *is_dynamic {
            // dynamic archive name
            target_dist.get("dynamic").unwrap().as_str().unwrap()
        } else {
            // static archive name
            target_dist.get("static").unwrap().as_str().unwrap()
        };
        let name = archive.replace(".tar.bz2", "");
        let name = name.replace(".tar.gz", "");

        let libs: Option<Vec<String>> = target_dist["targets"][target].as_array().map(|libs| {
            libs.iter()
                .map(|lib| lib.as_str().unwrap().to_string())
                .collect()
        });

        let url = self.url.replace("{archive}", archive);
        let checksum = DIST_CHECKSUM.get(archive)?;

        // modify is_dynamic
        debug_log!("checking is_dynamic");
        if let Some(target_dist) = target_dist.get("is_dynamic") {
            *is_dynamic = target_dist.as_bool()?;
            debug_log!("is_dynamic: {}", *is_dynamic);
        }

        let dist = Dist {
            archives: vec![Archive {
                url,
                checksum: checksum.to_string(),
            }],
            name,
            checksum: checksum.to_string(),
            libs,
        };
        debug_log!("dist: {:?}", dist);
        Some(dist)
    }
}

#[allow(unused)]
fn hex_str_to_bytes(c: impl AsRef<[u8]>) -> Vec<u8> {
    fn nibble(c: u8) -> u8 {
        match c {
            b'A'..=b'F' => c - b'A' + 10,
            b'a'..=b'f' => c - b'a' + 10,
            b'0'..=b'9' => c - b'0',
            _ => panic!(),
        }
    }

    c.as_ref()
        .chunks(2)
        .map(|n| (nibble(n[0]) << 4) | nibble(n[1]))
        .collect()
}

fn bytes_to_hex_str(bytes: Vec<u8>) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        s.push_str(&format!("{byte:02x}"));
    }
    s
}

pub fn sha256(buf: &[u8]) -> String {
    let hash_bytes: Vec<u8> = <sha2::Sha256 as sha2::Digest>::digest(buf).to_vec();
    bytes_to_hex_str(hash_bytes)
}

pub fn get_cache_dir() -> Option<PathBuf> {
    dirs::cache_dir().map(|p| p.join("sherpa-rs"))
}

#[allow(unused)]
pub fn extract_tgz(buf: &[u8], output: &Path) {
    let buf: std::io::BufReader<&[u8]> = std::io::BufReader::new(buf);
    let tar = flate2::read::GzDecoder::new(buf);
    let mut archive = tar::Archive::new(tar);
    archive.unpack(output).expect("Failed to extract .tgz file");
}

pub fn extract(buf: &[u8], file_name: &str, output: &Path) {
    if file_name.ends_with(".zip") {
        extract_zip(buf, file_name, output);
    } else {
        extract_tbz(buf, output);
    }
}

pub fn extract_zip(buf: &[u8], file_name: &str, output: &Path) {
    debug_log!("extracting zip to {}", output.display());
    let archive = output.join(file_name);
    std::fs::write(&archive, buf).expect("Failed to write the downloaded zip");
    let status = std::process::Command::new("unzip")
        .arg("-q")
        .arg("-o")
        .arg(&archive)
        .arg("-d")
        .arg(output)
        .status()
        .expect("Failed to run unzip");
    assert!(status.success(), "unzip failed for {}", archive.display());
    std::fs::remove_file(&archive).expect("Failed to remove the downloaded zip");
}

pub fn extract_tbz(buf: &[u8], output: &Path) {
    debug_log!("extracging tbz to {}", output.display());
    let buf: std::io::BufReader<&[u8]> = std::io::BufReader::new(buf);
    let tar = bzip2::read::BzDecoder::new(buf); // Use BzDecoder for .bz2
    let mut archive = tar::Archive::new(tar);
    archive.unpack(output).expect("Failed to extract .tbz file");
}

pub fn extract_lib_name<P: AsRef<Path>>(path: P) -> String {
    path.as_ref()
        .file_name()
        .and_then(|name| name.to_str())
        .map(|name| {
            name.strip_prefix("lib")
                .unwrap_or(name)
                .replace(".so", "")
                .replace(".dylib", "")
                .replace(".a", "")
        })
        .unwrap_or_else(|| "".to_string())
}
