# mwdh

MWDH stands for "Minecraft World Download Hoster" and is an easy command line utility (CLI) and HTTP file server to provide a world download for your Minecraft server's world.

# Intro / Motivation

Picture this: You're the admin of an SMP server, the season just ended and everyone is screaming for a world download. Of course, you dont want all the good work and memories go to waste, so you want to help. But it seems like a chore to package up gibibytes on gibibytes of world files and then somehow find a way to make them downloadable for everyone? Manually fiddling with commands or random tools and then throwing money into big companies' throats for cloud storage when you're hosting your own server? NAH that sh*t STINKS. 

I understand you. You want to just host your own world download. 

Well, look no more because on a sleepless night I wrote this lil CLI/HTTP server thing that allows you to do just that with minimal setup.
With just a singe command it compresses and packages up a .zip archive of the world with Nether and End with super-duper multi-threaded Rust performance and then hosts it for others to download. Yay!

# Quick Start

1. Acquire a MWDH binary (either download a pre-built or build it yourself)
2. Run the ol' command ``mwdh -p <path-to-your-mc-server-folder> -ne`` (-ne means 'include Nether and End')
3. Once it says "Hosting world files at", open yo webbrowser at that link and it should download the file. ez peezy.

# Firewall Settings

You may need to fiddle around with your proxy/firewall settings so that others can actually reach the port from the external network n stuff.
If your server is on Ubuntu Linux or similar you might try ``sudo ufw allow [port-number]`` MWDH's default default port-number is 3000, but you can configure it with the --port/-P argument
