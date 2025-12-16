# MWDH

[![GitHub release (latest by date)](https://img.shields.io/github/v/release/earomc/mwdh)](https://github.com/earomc/mwdh/releases)
[![GitHub Stars](https://img.shields.io/github/stars/earomc/mwdh?style=social)](https://github.com/earomc/mwdh/stargazers)
[![License](https://img.shields.io/github/license/earomc/mwdh)](LICENSE)
[![Downloads](https://img.shields.io/github/downloads/earomc/mwdh/total)](https://github.com/earomc/mwdh/releases)

MWDH stands for "Minecraft World Download Hoster" and is an easy command line utility (CLI), Minecraft world file compressor and HTTP file server to provide a world download for your Minecraft server's world.

# Features
   - **Blazingly fast** 🔥🚀 - Compresses a 4000x4000 world in ~2 seconds 🤯 on my trusty 16GB RAM, Ryzen 5 7520U Laptop 😌
   - **Parallel compression** - Utilizes all your CPU cores, or just some of them if you want (e.g. pass in ``-t 2`` to only use 2 threads 😎)
   - **Self-hosted** - No cloud storage fees or upload waits
   - **Simple** - One command and you're hosting!

![mwdh](https://github.com/user-attachments/assets/45002047-72d7-428d-978a-90672eb3bd8f)


# Intro / Motivation

Picture this: You're the admin of an SMP server, the season just ended and everyone is screaming for a world download. Of course, you don't want all the good work and memories go to waste, so you want to help. 

But it seems kinda like a chore to package up gibibytes on gibibytes of world files and then somehow find a way to make them downloadable for everyone? Manually fiddling with commands or random tools and then throwing money into big companies' throats for cloud storage when you're already hosting your own server? 

NAH that sh*t STINKS! 

I understand you. Of course you want to just host your own world download. 

Well, look no more because on a sleepless night I wrote this lil CLI/HTTP server thing that allows you to do just that with minimal setup.
With just a single command it compresses and packages up a .zip archive of the world and then hosts it for others to download. Yay!
And leveraging Rust and concurrent programming, you won't have to wait ages until the gosh-darned thang is compressed and uploaded to Google Drive! Double yay! 

# Quick Start

1. **Install**: Check out the [releases tab](https://github.com/earomc/mwdh/releases/) to download a pre-built binary for your specific system or use the install script.
2. Run the ol' command:
   
   ```sh
   mwdh -w <path-to-your-mc-server-directory> -one
   ```
   > -one means 'include Overworld, Nether and End'
   
   Alternatively, make sure that your current working directory is the server directory containing the world directories and just do:
   ```sh
   mwdh -one
   ```
3. Once it says "Hosting world files at", open your webbrowser at ``<your-server-ip>:3000/world`` and it should download the file. ez peezy.    

# Firewall Settings

You may need to fiddle around with your proxy/firewall settings so that others can actually reach the port from the external network n stuff. An internet search "open firewall port on <your-distro>" might do the trick.

> MWDH's default port number is 3000, but if you need to change that you easily can by passing the ``--port`` or ``-p`` argument:
```sh
mwdh [...] -p <port>
```

## Configuring UFW (uncomplicated firewall)
If your server is running Ubuntu Linux or similar **and/or you have ufw installed** you might wanna try:

```sh
sudo ufw allow <port-number>/tcp
```

and make sure it's enabled:

```sh
sudo ufw enable
```

# Building From Source

If you don't trust me or 'dist' that the binaries are actually cool n all, you can of course build them yourself:

1. Make sure you have [Rust and Cargo](https://rustup.rs/) installed
2. Clone the repo and replace ``<release-tag>`` with your desired release:

   ```sh
   git clone --depth 1 --branch <release-tag> https://github.com/earomc/mwdh.git
   ```
3. Invoke Cargo:

   ```sh
   cargo b -r
   ```
## Note about platform compatibility
This repo only provides pre-built binaries for Linux because pretty much all servers and therefore pretty much all Minecraft Servers are on Linux. 
But even though it hasn't been tested, I see no specific reason why MWDH shouldn't work on Windows or Mac. 
