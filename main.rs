mod bencode;

use crate::bencode::Bencode;
use base16ct::lower::encode_str;
use clap::Parser;
use clap::clap_derive::Subcommand;
use itertools::Itertools;
use memmap2::Mmap;
use rayon::iter::{IndexedParallelIterator, IntoParallelIterator, ParallelIterator};
use sha1::{Digest, Sha1};
use std::borrow::Cow;
use std::collections::HashSet;
use std::ffi::OsStr;
use std::fmt::{Debug, Display, Formatter};
use std::fs::File;
use std::io::ErrorKind;
use std::iter::repeat;
use std::iter::{from_fn, once};
use std::path::{Path, PathBuf};
use std::{fs, io};
use unicode_normalization::UnicodeNormalization;

type Error = Box<dyn std::error::Error + Send + Sync>;

#[derive(Parser, Debug)]
#[command(about, arg_required_else_help = true, flatten_help = true)]
struct Cli {
    #[command(subcommand)]
    command: Subcommand,
}

#[derive(Parser, Debug)]
struct TorrentArgs {
    /// Path to a torrent file or a directory containing torrent files
    torrent_files: Vec<PathBuf>,
    /// Replace characters that are invalid on Windows (e.g. <, >, :, ", |, ?, *) with underscores
    #[arg(long, default_value_t = cfg!(windows), help_heading = Some("Path fixing"))]
    windows_paths: bool,
    /// Normalize Unicode paths (NFC, or NFD on macOS/iOS)
    #[arg(long, default_value_t = true, help_heading = Some("Path fixing"))]
    normalize_utf8_paths: bool,
}

impl TorrentArgs {
    fn read_torrents(&self) -> Result<Vec<(PathBuf, Torrent)>, Error> {
        self.torrent_files
            .iter()
            .map(|path| {
                if path.is_dir() {
                    read_dir_recursive(path)?
                        .filter(|path| match path {
                            Ok(path) => path.extension() == Some(OsStr::new("torrent")),
                            _ => true,
                        })
                        .collect()
                } else {
                    Ok(vec![path.clone()])
                }
            })
            .collect::<Result<Vec<_>, _>>()?
            .into_par_iter()
            .flatten()
            .map(|path| {
                let bytes = fs::read(&path)?;
                let (value, _) = Bencode::decode(&bytes)?;
                let mut torrent: Torrent = value.try_into()?;
                if self.windows_paths {
                    torrent.fix_windows_paths();
                }
                if self.normalize_utf8_paths {
                    torrent.normalize_utf8_paths();
                }
                Ok((path, torrent))
            })
            .collect()
    }
}

#[derive(Parser, Debug)]
struct TorrentsAndDataArgs {
    #[command(flatten)]
    torrents: TorrentArgs,
    /// Path to the directory containing the downloaded data
    #[arg(short, long = "data")]
    data_dir: PathBuf,
}

#[derive(Subcommand, Debug)]
enum Subcommand {
    #[command(about = "Verify downloaded torrent data")]
    Verify(TorrentsAndDataArgs),
    #[command(about = "List files in the data directory that don't belong to a torrent file")]
    Unowned(TorrentsAndDataArgs),
    #[command(about = "List files for each torrent that are missing in the data directory")]
    Missing(TorrentsAndDataArgs),
    #[command(about = "Dump information about the provided torrent files")]
    Dump(TorrentArgs),
    #[command(about = "Get the info-hash of each provided torrent file")]
    InfoHash(TorrentArgs),
    #[command(about = "List the files owned by the provided torrent files")]
    Files(TorrentArgs),
}

#[derive(Debug)]
struct Torrent {
    info_hash: Sha1Hash,
    _announce: Vec<String>,
    files: Vec<(String, usize)>,
    pieces: Vec<Sha1Hash>,
    piece_size: usize,
}

#[derive(Eq, PartialEq, Hash)]
struct Sha1Hash([u8; 20]);

impl Debug for Sha1Hash {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        Display::fmt(self, f)
    }
}

impl Display for Sha1Hash {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str(encode_str(&self.0, &mut [b'\0'; 40])?)
    }
}

fn get_info_hash(info: &Bencode) -> Sha1Hash {
    let mut buf = Vec::new();
    info.encode(&mut buf).unwrap();
    Sha1Hash(Sha1::digest(&buf).into())
}

impl TryFrom<Bencode> for Torrent {
    type Error = Error;
    fn try_from(mut value: Bencode) -> std::result::Result<Self, Self::Error> {
        let mut info: Bencode = value.remove_key("info")?;
        let info_hash = get_info_hash(&info);

        let name: String = info.remove_key("name")?.try_into()?;

        let files: Vec<(Vec<String>, usize)> = match info.remove_key("length") {
            Ok(length) => vec![(vec![], length.try_into()?)],
            Err(_) => Vec::<Bencode>::try_from(info.remove_key("files")?)?
                .into_iter()
                .map(|mut file| {
                    Ok((
                        file.remove_key("path")?.try_into()?,
                        file.remove_key("length")?.try_into()?,
                    ))
                })
                .collect::<Result<_, Error>>()?,
        };

        Ok(Torrent {
            info_hash,
            _announce: match value.remove_key("announce") {
                Err(_) => vec![],
                Ok(Bencode::Bytes(x)) => vec![String::try_from(x)?],
                Ok(x) => x.try_into()?,
            },
            files: files
                .into_iter()
                .map(|(path, length)| {
                    let path = once(&*name)
                        .chain(path.iter().flat_map(|s| ["/", s]))
                        .collect();
                    (path, length)
                })
                .collect(),
            pieces: Vec::<u8>::try_from(info.remove_key("pieces")?)?
                .as_chunks::<20>()
                .0
                .into_iter()
                .copied()
                .map(Sha1Hash)
                .collect(),
            piece_size: info.remove_key("piece length")?.try_into()?,
        })
    }
}

impl Torrent {
    fn fix_windows_paths(&mut self) {
        for (path, _) in &mut self.files {
            *path = path
                .chars()
                .map(|c| match c {
                    '<' | '>' | ':' | '"' | '|' | '?' | '*' => '_',
                    '\x00'..='\x1F' => '_',
                    c => c,
                })
                .collect();
        }
    }

    fn normalize_utf8_paths(&mut self) {
        for (path, _) in &mut self.files {
            *path = normalize_utf8(path);
        }
    }

    fn read_files(&self, data_dir: &Path) -> io::Result<Vec<Sha1Hash>> {
        let mmaps = self
            .files
            .iter()
            .map(|(path, size)| {
                with_partial_files(path.into())
                    .flat_map(|path| match File::open(data_dir.join(&*path)) {
                        Err(e) if e.kind() == ErrorKind::NotFound => None,
                        x => Some(x),
                    })
                    .map(|file| unsafe { Mmap::map(&file?) })
                    .next()
                    .transpose()
                    .map(|mmap| (mmap, *size))
            })
            .collect::<io::Result<Vec<_>>>()?;
        let mut carry: &[u8] = &[];

        Ok(mmaps
            .iter()
            .flat_map(|(mmap, size)| {
                mmap.as_deref()
                    .into_iter()
                    // A large buffer here reduces the number of hashing loops for large missing files
                    .chain(repeat(&[0; 8192][..]))
                    .scan(*size, |need, slice| match need {
                        0 => None,
                        _ => {
                            let i = (*need).min(slice.len());
                            *need -= i;
                            Some(&slice[..i])
                        }
                    })
            })
            .batching(|slices| {
                let mut hash = Sha1::default();
                let mut done = 0;

                for slice in once(carry).chain(slices) {
                    let i = (self.piece_size - done).min(slice.len());
                    done += i;
                    hash.update(&slice[..i]);
                    carry = &slice[i..];
                    if !carry.is_empty() {
                        break;
                    }
                }

                match done {
                    0 => None,
                    _ => Some(Sha1Hash(hash.finalize().into())),
                }
            })
            .collect())
    }

    fn check_files(&self, name: &Path, data_dir: &Path) -> io::Result<()> {
        let any_files_exist = self
            .files
            .iter()
            .flat_map(|(path, _size)| with_partial_files(path.into()))
            .any(|path| data_dir.join(&*path).exists());

        let matches = if !any_files_exist {
            vec![false; self.pieces.len()]
        } else {
            self.read_files(data_dir)?
                .iter()
                .zip_eq(self.pieces.iter())
                .map(|(a, b)| a == b)
                .collect_vec()
        };

        let percent = count_true(&matches) * 10000 / self.pieces.len();

        let progress: String = match percent {
            10000 if SHOW_COMPLETE => format!("{:─^20}", "COMPLETE"),
            _ => compress(&matches, 20)
                .map(|x| CHARS[count_true(x) * (CHARS.len() - 1) / x.len()])
                .collect(),
        };

        println!(
            "{: >8.2}%  {}  {}",
            (percent as f64) / 100f64,
            progress,
            name.display()
        );

        Ok(())
    }
}

fn normalize_utf8(s: &str) -> String {
    // https://github.com/syncthing/syncthing/blob/29f7510f5a441f4e05c37ffeb035c7d3a3459ea7/lib/scanner/walk.go#L556
    // https://pkg.go.dev/golang.org/x/text/unicode/norm#Form
    if cfg!(any(target_os = "macos", target_os = "ios")) {
        s.nfd().collect()
    } else {
        s.nfc().collect()
    }
}

fn read_dir_recursive(dir: &Path) -> io::Result<impl Iterator<Item = io::Result<PathBuf>>> {
    let mut stack = vec![dir.read_dir()?];
    let mut next = move || {
        while let Some(iter) = stack.last_mut() {
            match &iter.next().transpose()? {
                Some(d) if d.file_type()?.is_dir() => {
                    stack.push(d.path().read_dir()?);
                }
                Some(f) => return Ok(Some(f.path())),
                None => drop(stack.pop()),
            }
        }
        Ok(None)
    };
    Ok(from_fn(move || next().transpose()))
}

fn with_partial_files(path: Cow<str>) -> impl Iterator<Item = Cow<str>> {
    [path.clone(), path.clone() + ".part", path + ".!qB"].into_iter()
}

const SHOW_COMPLETE: bool = true;
const CHARS: &[char] = &[' ', '▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
// const CHARS: &[char] = &['○', '●'];
// const CHARS: &[char] = &['·', '•'];
// const CHARS: &[char] = &[' ', '•'];

fn count_true(bools: &[bool]) -> usize {
    bools.iter().filter(|x| **x).count()
}

/// Group the bools into `size` groups
fn compress(bools: &[bool], size: usize) -> impl Iterator<Item = &[bool]> {
    let n = bools.len();

    (0..size).map(move |i| {
        let [i, j] = [i, i + 1].map(|i| i * n / size);

        &bools[match j - i {
            // Make sure the range isn't empty if there is space either side
            0 if j != n => i..j + 1,
            0 if i != 0 => i - 1..j,
            _ => i..j,
        }]
    })
}

fn main() -> Result<(), Error> {
    let cli = Cli::parse();

    match cli.command {
        Subcommand::Verify(args) => {
            let torrents = args.torrents.read_torrents()?;
            torrents
                .into_par_iter()
                .with_max_len(1)
                .map(|(name, torrent)| torrent.check_files(&name, &args.data_dir))
                .collect::<io::Result<Vec<_>>>()?;
        }
        Subcommand::Unowned(args) => {
            let torrent_files: HashSet<_> = args
                .torrents
                .read_torrents()?
                .into_iter()
                .flat_map(|(_, torrent)| torrent.files)
                .flat_map(|(path, _)| with_partial_files(path.into()))
                .map(|path| args.data_dir.join(&*path))
                .collect();

            for file in read_dir_recursive(&args.data_dir)? {
                let file = file?;
                if file.file_name() == Some(OsStr::new(".DS_Store")) {
                    continue;
                }
                if !torrent_files.contains(&file) {
                    println!("{}", file.display());
                }
            }
        }
        Subcommand::Missing(args) => {
            let existing: HashSet<PathBuf> =
                read_dir_recursive(&args.data_dir)?.collect::<io::Result<_>>()?;

            for (path, torrent) in args.torrents.read_torrents()? {
                let missing = torrent
                    .files
                    .iter()
                    .map(|(path, _)| path)
                    .filter(|&path| {
                        !with_partial_files(path.into())
                            .any(|path| existing.contains(&args.data_dir.join(&*path)))
                    })
                    .map(|path| args.data_dir.join(path))
                    .collect_vec();

                if missing.len() > 0 {
                    println!("{}", path.display());
                    for file in missing {
                        println!("  {}", file.display());
                    }
                }
            }
        }
        Subcommand::Dump(args) => {
            for (_, torrent) in args.read_torrents()? {
                println!("{:#?}", torrent);
            }
        }
        Subcommand::Files(args) => {
            for (path, torrent) in args.read_torrents()? {
                println!("{}", path.display());
                for (file, _) in torrent.files {
                    println!("  {}", file);
                }
            }
        }
        Subcommand::InfoHash(args) => {
            for (path, torrent) in args.read_torrents()? {
                println!("{}  {}", torrent.info_hash, path.display())
            }
        }
    }
    Ok(())
}
