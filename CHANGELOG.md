# mwdh 0.2.0

This is a big update for MWDH adding zstd compression, subcommands and more!

- Added parallel and sequential [zstd](https://github.com/facebook/zstd) compression for ~5x faster compression!
- Adding the `-t 1` flag and using zstd compression switches to sequential mode, which on high compression levels *may* lead to slightly better compression ratios (smaller archive sizes)
- zstd is now the new default and will result in a .tar.zst file. For compatibilty reasons, ZIP compression can still be used by passing in the `--compression-format zip` argument. This is if you dont care about compression speeds and want to make it as easy as possible for users to decompress the files
- Archiving/compressing are now seperate steps that can be invoked with their respective subcommands `mwdh compress` (short: `mwdh c`) and `mwdh host` (short: `mwdh h`). If you want the old functionality of compressing and hosting in one step, use `mwdh compress-host` or `mwdh ch` for short
- Just hosting your world archive (not compressingn and hosting in a single step) requires you to specify a path to to an archive like this: `-a yourworld.tar.zst`. The file ending has to be .zip or .tar.zst though. (It parses information for the http header from the file ending)
- **IMPORTANT**: Added new `--bukkit` flag. You have to add this flag if you're using a Bukkit/Spigot/Paper server. Because for some reason these servers use a different kind of world directory structure. The world is split across three directories, one for each dimension:
```
world
 |- region
 |- ...
world_nether
 |- region
 |- ...
world_the_end
 |- region
 |- ...
```
While on vanilla, Fabric or other Minecraft instances, the structure looks like this:
```
world
 |- region (Overworld regions)
 |- ...
 |- DIM-1 (Nether)
    |- region
    |- ...
 |- DIM1 (The End)
    |- region
    |- ...
```
So when scanning for files, the `--bukkit` flag tells MWDH to consider the different format.
Note that on earlier versions of MWDH, the behavior of `--bukkit` was the default, but also would not consider when you would not include the Nether or The End when exporting a Vanilla/Fabric/Non-Bukkit world. So this was a bug which is now fixed. 

- Better feedback for bad arguments thanks to manual parsing with clap builder
- Now you need to specify if you want to include the overworld. If you wish to compress the entire world, just the `-neo` or `-one` flags
- You can now specify the server and compression threads seperately with `--server-threads` and `--compression-threads` respectively. Or just use `-t` to set both to the same value
- `-p` becomes `-w` with the long-form `--worlds-path`
- `-P` (for port) becomes `-p`
- `--host-ip` becomes `--bind`
