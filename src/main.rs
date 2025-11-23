mod bencode;

use crate::bencode::Bencode;
use base16ct::lower::encode_str;
use clap::Parser;
use clap::clap_derive::Subcommand;
use itertools::Itertools;
use memmap2::Mmap;
use rayon::iter::{IndexedParallelIterator, IntoParallelIterator, ParallelIterator};
use sha1::{Digest, Sha1};
use std::collections::HashSet;
use std::ffi::OsStr;
use std::fmt::{Debug, Display, Formatter};
use std::fs::{File, ReadDir};
use std::io::ErrorKind;
use std::iter::once;
use std::iter::repeat;
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
struct TorrentsAndData {
    torrent_files: Vec<PathBuf>,
    #[arg(short, long = "data")]
    data_dir: PathBuf,
}

#[derive(Parser, Debug)]
struct JustTorrents {
    torrent_files: Vec<PathBuf>,
}

#[derive(Subcommand, Debug)]
enum Subcommand {
    #[command(about = "Verify downloaded torrent data")]
    Verify(TorrentsAndData),
    #[command(about = "List files in the data directory that don't belong to a torrent file")]
    Unowned(TorrentsAndData),
    #[command(about = "List files for each torrent that are missing in the data directory")]
    Missing(TorrentsAndData),
    #[command(about = "Dump information about the provided torrent files")]
    Dump(JustTorrents),
    #[command(about = "Get the info-hash of each provided torrent file")]
    InfoHash(JustTorrents),
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
    let mut data = Vec::new();
    info.encode(&mut data).unwrap();
    return Sha1Hash(Sha1::digest(&data).into());
}

impl TryFrom<Bencode> for Torrent {
    type Error = Error;
    fn try_from(mut value: Bencode) -> std::result::Result<Self, Self::Error> {
        let mut info: Bencode = value.remove_key("info")?;
        let info_hash = get_info_hash(&info);

        let name: String = info.remove_key("name")?.try_into()?;

        let files: Vec<(Vec<String>, usize)> = if let Ok(length) = info.remove_key("length") {
            vec![(vec![], length.try_into()?)]
        } else {
            info.remove_key("files")?
                .try_into_list()?
                .into_iter()
                .map(|mut file| {
                    Ok((
                        file.remove_key("path")?.try_into()?,
                        file.remove_key("length")?.try_into()?,
                    ))
                })
                .collect::<Result<_, Error>>()?
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
            pieces: info
                .remove_key("pieces")?
                .try_into_bytes()?
                .chunks(20)
                .map(<[u8; 20]>::try_from)
                .map(|r| r.map(Sha1Hash))
                .collect::<Result<Vec<_>, _>>()?,
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

struct ReadDirRecursive {
    // LIFO queue of directories to read
    queue: Vec<PathBuf>,
    current: ReadDir,
}

impl ReadDirRecursive {
    fn next_or_error(&mut self) -> io::Result<Option<PathBuf>> {
        loop {
            match self.current.next() {
                Some(Ok(ref entry)) => {
                    if entry.file_type()?.is_dir() {
                        self.queue.push(entry.path());
                    } else {
                        return Ok(Some(entry.path()));
                    }
                }
                Some(Err(e)) => return Err(e),
                None => match self.queue.pop() {
                    Some(next) => self.current = next.read_dir()?,
                    None => return Ok(None),
                },
            }
        }
    }
}

impl Iterator for ReadDirRecursive {
    type Item = io::Result<PathBuf>;
    fn next(&mut self) -> Option<Self::Item> {
        self.next_or_error().transpose()
    }
}

fn read_dir_recursive(dir: &Path) -> io::Result<ReadDirRecursive> {
    Ok(ReadDirRecursive {
        queue: Vec::new(),
        current: dir.read_dir()?,
    })
}

fn find_torrents(dir: &Path) -> io::Result<Vec<PathBuf>> {
    read_dir_recursive(dir)?
        .into_iter()
        .filter(|path| match path {
            Ok(path) if path.extension() == Some(OsStr::new("torrent")) => true,
            _ => false,
        })
        .collect()
}

fn read_torrents(torrents: Vec<PathBuf>) -> Result<Vec<(PathBuf, Torrent)>, Error> {
    torrents
        .into_iter()
        .map(|path| {
            if path.is_dir() {
                find_torrents(&path)
            } else {
                Ok(vec![path])
            }
        })
        .collect::<Result<Vec<_>, _>>()?
        .into_par_iter()
        .flatten()
        .map(|path| {
            let bytes = fs::read(&path)?;
            let (value, _) = Bencode::decode(&bytes)?;
            let mut torrent: Torrent = value.try_into()?;
            torrent.fix_windows_paths();
            torrent.normalize_utf8_paths();
            Ok((path, torrent))
        })
        .collect()
}

fn read_torrent_files(torrent: &Torrent, data_dir: &Path) -> io::Result<Vec<Sha1Hash>> {
    let mmaps = torrent
        .files
        .iter()
        .map(|(path, size)| {
            let mmap = match File::open(data_dir.join(path)) {
                Err(e) if e.kind() == ErrorKind::NotFound => None,
                Err(e) => return Err(e),
                Ok(ref file) => Some(unsafe { Mmap::map(file) }?),
            };
            Ok((mmap, *size))
        })
        .collect::<io::Result<Vec<_>>>()?;

    let results = mmaps
        .iter()
        .flat_map(|(mmap, size)| {
            mmap.as_deref()
                .into_iter()
                // SHA-1 processes in 64-byte blocks
                .chain(repeat(&[0u8; 64][..]))
                .scan(*size, |need, mut slice| match need {
                    0 => None,
                    _ => {
                        if slice.len() > *need {
                            slice = &slice[..*need];
                        }
                        *need -= slice.len();
                        Some(slice)
                    }
                })
        })
        .peekable()
        .batching(|slices| {
            let mut hash = Sha1::default();
            let mut need = torrent.piece_size;

            // Take any slices that are <= the bytes we need
            while let Some(slice) = slices.next_if(|s| s.len() <= need) {
                need -= slice.len();
                hash.update(slice);
            }

            // If there is a slice remaining, take as much as possible and leave behind the unused bytes
            if need > 0
                && let Some(slice) = slices.peek_mut()
            {
                let (head, unused) = slice.split_at(need);
                *slice = unused;
                need -= head.len();
                hash.update(head);
            }

            if need == torrent.piece_size {
                None
            } else {
                Some(Sha1Hash(hash.finalize().into()))
            }
        })
        .collect_vec();

    assert_eq!(torrent.pieces.len(), results.len());

    Ok(results)
}

fn check_file_contents(torrents: &[(PathBuf, Torrent)], data_dir: &Path) -> Result<(), Error> {
    torrents
        .into_par_iter()
        .with_max_len(1)
        .map(|(name, torrent)| -> Result<_, _> {
            let any_files_exist = torrent
                .files
                .iter()
                .any(|(path, _)| data_dir.join(path).exists());

            let matches = if !any_files_exist {
                torrent.pieces.iter().map(|_| false).collect()
            } else {
                read_torrent_files(torrent, data_dir)?
                    .iter()
                    .zip_eq(torrent.pieces.iter())
                    .map(|(a, b)| a == b)
                    .collect_vec()
            };

            let num_matches = matches.iter().filter(|x| **x).count();
            let percent = (num_matches as f64) * 100f64 / torrent.pieces.len() as f64;

            let progress = compress(&matches, 20)
                .into_iter()
                .map(char_for)
                .collect::<String>();

            println!("{: >8.2}%  {}  {}", percent, progress, name.display());

            Ok(())
        })
        .collect()
}

const CHARS: [char; 3] = [' ', '·', '•'];

fn char_for(bools: &[bool]) -> char {
    let [empty, partial, complete] = CHARS;
    match bools.iter().filter(|x| **x).count() {
        x if x == bools.len() => complete,
        0 => empty,
        _ => partial,
    }
}

fn compress(bools: &[bool], size: usize) -> Vec<&[bool]> {
    (0..size)
        .map(|i| {
            let [i, j] = [i, i + 1].map(|i| (i * bools.len()) / size);
            if i == j && j < bools.len() {
                &bools[i..j + 1]
            } else if i == j && i > 0 {
                &bools[i - 1..j]
            } else {
                &bools[i..j]
            }
        })
        .collect()
}

fn main() -> Result<(), Error> {
    let cli = Cli::parse();

    match cli.command {
        Subcommand::Verify(args) => {
            let torrents = read_torrents(args.torrent_files)?;
            check_file_contents(&torrents, &args.data_dir)?;
        }
        Subcommand::Unowned(args) => {
            let torrent_files: HashSet<_> = read_torrents(args.torrent_files)?
                .into_iter()
                .flat_map(|x| x.1.files)
                .map(|x| args.data_dir.join(x.0))
                .collect();
            for file in read_dir_recursive(&args.data_dir)? {
                let file = file?;
                if !torrent_files.contains(&file) {
                    println!("{}", file.display());
                }
            }
        }
        Subcommand::Missing(args) => {
            let existing: HashSet<PathBuf> = read_dir_recursive(&args.data_dir)?
                .into_iter()
                .collect::<io::Result<_>>()?;
            for (path, torrent) in read_torrents(args.torrent_files)? {
                let missing = torrent
                    .files
                    .into_iter()
                    .map(|x| args.data_dir.join(x.0))
                    .filter(|x| !existing.contains(x))
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
            for (_, torrent) in read_torrents(args.torrent_files)? {
                println!("{:#?}", torrent);
            }
        }
        Subcommand::InfoHash(args) => {
            for (path, torrent) in read_torrents(args.torrent_files)? {
                println!("{}  {}", torrent.info_hash, path.display())
            }
        }
    }
    Ok(())
}
