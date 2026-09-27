# torrent-verify

Simple tool to validate downloaded BitTorrent files. `rayon` and `memmap2` are used to maximise throughput. Includes a simple Bencode parser/unparser.

Additional subcommands are provided to only show the files that are missing, or files that do not belong to any torrent, and extract information from torrent files.

You may be interested in [imdl](https://crates.io/crates/imdl) for an expanded feature set.

## Installation

```
cargo install torrent-verify
```

## Usage

```
Usage: torrent-verify verify [OPTIONS] --data <DATA_DIR> [TORRENT_FILES]...
       torrent-verify unowned [OPTIONS] --data <DATA_DIR> [TORRENT_FILES]...
       torrent-verify missing [OPTIONS] --data <DATA_DIR> [TORRENT_FILES]...
       torrent-verify dump [OPTIONS] [TORRENT_FILES]...
       torrent-verify info-hash [OPTIONS] [TORRENT_FILES]...
       torrent-verify files [OPTIONS] [TORRENT_FILES]...
       torrent-verify help [COMMAND]...
```

## Example

```
$ torrent-verify verify my_torrents/*.torrent --data my_torrent_data
    0.00%  ○○○○○○○○○○○○○○○○○○○○  my_torrents/ubuntu-26.04.1-desktop-amd64.iso.torrent
   99.25%  ●●●●●●●●●●●●●●●●●●●●  my_torrents/Linux_ISO_Test_Files.torrent
   48.12%  ○○●○○○●●●○●○○●●●●○○○  my_torrents/archlinux-2026.09.01-x86_64.iso.torrent
   98.91%  ●●●●●●●●●●●●●●●●●●●●  my_torrents/debian-13.7.0-amd64-DVD-1.iso.torrent
   14.88%  ●○○○○○○●○○○○○○○○○○○○  my_torrents/ubuntu-26.04.1-desktop-arm64.iso.torrent
   41.34%  ●●●●●●●●●○○○○○○○○○○○  my_torrents/Fedora-Workstation-Live-aarch64-44.torrent
   99.86%  ●●●●●●●●●●●●●●●●●●●●  my_torrents/LibreOffice_26.8.0_Win_x86-64.msi.torrent
  100.00%  ●●●●●●●●●●●●●●●●●●●●  my_torrents/Sample_Dataset_10GB.torrent
   83.97%  ●●●●●●●●●●●●●●○●●●●●  my_torrents/Example_File_Collection_2026.torrent
   64.10%  ○○○○○●●○●●●○●●●●●●●●  my_torrents/Videos_Collection.torrent
  100.00%  ●●●●●●●●●●●●●●●●●●●●  my_torrents/Open_Source_Project_Files_v1.2.torrent
  100.00%  ●●●●●●●●●●●●●●●●●●●●  my_torrents/blender-5.2.0-windows-x64.msi.torrent
  100.00%  ●●●●●●●●●●●●●●●●●●●●  my_torrents/Fedora-Workstation-Live-x86_64-44.torrent
```
