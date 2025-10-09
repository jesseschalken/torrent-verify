mod bencode;

use crate::bencode::Bencode;
use base16ct::lower::encode_str;
use camino::{Utf8Path, Utf8PathBuf};
use clap::clap_derive::Subcommand;
use clap::Parser;
use itertools::Itertools;
use rayon::iter::{ParallelBridge, ParallelIterator};
use rayon::prelude::IntoParallelRefIterator;
use sha1::{Digest, Sha1};
use std::collections::HashSet;
use std::fmt::{Debug, Display, Formatter};
use std::fs::File;
use std::io::{ErrorKind, Read};
use std::iter::once;
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
    torrent_files: Vec<Utf8PathBuf>,
    #[arg(short, long)]
    data_dir: Utf8PathBuf,
}

#[derive(Parser, Debug)]
struct JustTorrents {
    torrent_files: Vec<Utf8PathBuf>,
}

#[derive(Subcommand, Debug)]
enum Subcommand {
    VerifyData(TorrentsAndData),
    ShowOrphaned(TorrentsAndData),
    ShowMissing(TorrentsAndData),
    Dump(JustTorrents),
    GetInfoHash(JustTorrents),
}

#[derive(Debug)]
struct Torrent {
    info_hash: Sha1Hash,
    _announce: Vec<String>,
    files: Vec<(Utf8PathBuf, usize)>,
    pieces: Vec<Sha1Hash>,
    piece_size: usize,
}

#[derive(Eq, PartialEq, Hash)]
struct Sha1Hash([u8; 20]);

impl Sha1Hash {
    fn hash(value: &[u8]) -> Self {
        Self(Sha1::digest(value).into())
    }
}

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
                .map(|(path, length)| (once(&name).chain(&path).collect(), length))
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
                .as_str()
                .chars()
                .map(|c| match c {
                    '<' | '>' | ':' | '"' | '|' | '?' | '*' => '_',
                    '\x00'..='\x1F' => '_',
                    c => c,
                })
                .collect::<String>()
                .into();
        }
    }

    fn normalize_utf8_paths(&mut self) {
        for (path, _) in &mut self.files {
            *path = normalize_utf8(path.as_str()).into();
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

fn read_dir_recursive(dir: &Utf8Path) -> io::Result<Vec<Utf8PathBuf>> {
    fn read_into(dir: &Utf8Path, out: &mut Vec<Utf8PathBuf>) -> io::Result<()> {
        for ent in dir.read_dir_utf8()? {
            let ent = ent?;
            if ent.file_type()?.is_dir() {
                read_into(&ent.path(), out)?;
            } else {
                out.push(ent.into_path());
            }
        }
        Ok(())
    }
    let mut ret = Vec::new();
    read_into(dir, &mut ret)?;
    Ok(ret)
}

fn find_torrents(dir: &Utf8Path) -> io::Result<Vec<Utf8PathBuf>> {
    Ok(read_dir_recursive(dir)?
        .into_iter()
        .filter(|path| path.extension() == Some("torrent"))
        .collect())
}

fn read_torrents(torrents: Vec<Utf8PathBuf>) -> Result<Vec<(Utf8PathBuf, Torrent)>, Error> {
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
        .into_iter()
        .flatten()
        .par_bridge()
        .map(|path| {
            let bytes = fs::read(&path)?;
            let (bencode, _) = Bencode::decode(&fs::read(&path)?)?;
            let mut torrent: Torrent = bencode.try_into()?;
            torrent.fix_windows_paths();
            torrent.normalize_utf8_paths();
            Ok((path, torrent))
        })
        .collect()
}

fn read_torrent_files(torrent: &Torrent, data_dir: &Utf8Path) -> Result<Vec<Sha1Hash>, Error> {
    let mut results = Vec::with_capacity(torrent.pieces.len());

    let mut buffer = Vec::new();
    buffer.resize(torrent.piece_size, 0);

    let mut buf_pos = 0usize;

    for (path, file_size) in &torrent.files {
        let mut file_pos = 0usize;
        let mut file_handle = match File::open(data_dir.join(path)) {
            Err(e) if e.kind() == ErrorKind::NotFound => None,
            x => Some(x?),
        };

        while file_pos < *file_size {
            let piece_len = buffer.len();
            let file_remaining = file_size - file_pos;
            let output_range = &mut buffer[buf_pos..(buf_pos + file_remaining).min(piece_len)];

            if let Some(f) = &mut file_handle {
                let bytes_read = f.read(output_range)?;
                if bytes_read == 0 {
                    // EOF
                    file_handle = None;
                }
                buf_pos += bytes_read;
                file_pos += bytes_read;
            } else {
                // no file to read, just fill with zeros
                output_range.fill(0);
                buf_pos += output_range.len();
                file_pos += output_range.len();
            };

            if buf_pos == buffer.len() {
                results.push(Sha1Hash::hash(&buffer));
                buf_pos = 0;
            }
        }
    }

    if buf_pos != 0 {
        results.push(Sha1Hash::hash(&buffer[..buf_pos]))
    }

    assert_eq!(torrent.pieces.len(), results.len());

    Ok(results)
}

fn check_file_contents(
    torrents: &[(Utf8PathBuf, Torrent)],
    data_dir: &Utf8Path,
) -> Result<(), Error> {
    torrents
        .par_iter()
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

            println!("{: >8.2}%  {}  {}", percent, progress, name);

            Ok(())
        })
        .collect()
}

const CHARS: &'static [char] = &['○', '●'];
// const CHARS: &'static [char] = &['○', '◒', '●'];
// const CHARS: &'static [char] = &['○', '◔', '◑', '◕', '●'];
// const CHARS: &'static [char] = &[' ', '▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];

fn char_for(bools: &[bool]) -> char {
    let num = bools.iter().filter(|x| **x).count();
    CHARS[num * (CHARS.len() - 1) / bools.len()]
}

fn compress(bools: &[bool], size: usize) -> Vec<&[bool]> {
    let factor = bools.len() as f64 / size as f64;
    (0..size)
        .map(|i| {
            let i1 = (i as f64 * factor) as usize;
            let i2 = ((i + 1) as f64 * factor) as usize;
            if i1 == i2 {
                &bools[i1..=i2]
            } else {
                &bools[i1..i2]
            }
        })
        .collect()
}

fn main() -> Result<(), Error> {
    let cli = Cli::parse();

    match cli.command {
        Subcommand::VerifyData(args) => {
            let torrents = read_torrents(args.torrent_files)?;
            check_file_contents(&torrents, &args.data_dir)?;
        }
        Subcommand::ShowOrphaned(args) => {
            let torrent_files: HashSet<_> = read_torrents(args.torrent_files)?
                .into_iter()
                .flat_map(|x| x.1.files)
                .map(|x| args.data_dir.join(x.0))
                .collect();
            for file in read_dir_recursive(&args.data_dir)? {
                if !torrent_files.contains(&file) {
                    println!("{}", file);
                }
            }
        }
        Subcommand::ShowMissing(args) => {
            let existing: HashSet<Utf8PathBuf> =
                read_dir_recursive(&args.data_dir)?.into_iter().collect();
            for (path, torrent) in read_torrents(args.torrent_files)? {
                let missing = torrent
                    .files
                    .into_iter()
                    .map(|x| args.data_dir.join(x.0))
                    .filter(|x| !existing.contains(x))
                    .collect_vec();
                if missing.len() > 0 {
                    println!("{}", path);
                    for file in missing {
                        println!("  {}", file);
                    }
                }
            }
        }
        Subcommand::Dump(args) => {
            for (_, torrent) in read_torrents(args.torrent_files)? {
                println!("{:#?}", torrent);
            }
        }
        Subcommand::GetInfoHash(args) => {
            for (path, torrent) in read_torrents(args.torrent_files)? {
                println!("{}  {}", torrent.info_hash, path)
            }
        }
    }
    Ok(())
}
