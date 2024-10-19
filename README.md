# file-trackr

A simple command-line utility for taking and comparing snapshots of the filesystem.

> [!WARNING]
> This software is unfinished. Breaking changes may happen at any time.

## Quick start

```console
$ git clone https://github.com/LazyPluto/file-trackr
$ cd file-trackr
$ cargo run track.json
```

## Usage

```console
$ file-trackr track.json

# It will ask for the path to track the first time you run it.
Path to track: D:\Folder1

>> snap

>> show
D:\Folder1: 69.69 GiB
    SubFolder1: 1.00 GiB
    SubFolder2: 1.00 GiB
    SubFolder3: 67.69 GiB
    ...

# After some changes to the filesystem..
>> snap

>> show
D:\Folder1: 420.69 GiB
    SubFolder1: 1.00 GiB
    SomeRandomFolder: 350.00 GiB
    SubFolder2: 2.00 GiB
    SubFolder3: 67.69 GiB
    ...

>> compare
Sat 19 Oct 2024 13:41:00 -> Sat 19 Oct 2024 14:02:50
D:\Folder1: 69.69 GiB -> 420.69 GiB: + 351.00 GiB
    SubFolder2: 1.00 GiB -> 2.00 GiB: +1.00 GiB:
        Some other folder: 512.00 MiB -> 1.00 GiB: +512.00 MiB
        FileXYZ.txt: 1.00 GiB -> 512.00 MiB: -512.00 MiB
        Another folder: 1.00 GiB -> 2.00 GiB: +1.00 GiB
    
    New Files:
        SomeRandomFolder: 350.00 GiB
            FileA: 64.00 GiB
            FileB: 1.00 GiB
            ...
```
