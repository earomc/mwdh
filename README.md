# mwdh


MWDH stands for "Minecraft World Download Hoster" and is an easy command line utility (CLI) and HTTP server to provide a world download for your Minecraft server's world.

You're the admin of an SMP server, the season ends and you dont want all the good work and memories go to waste? You want to provide a world download but it seems like a chore to package up gibibytes on gibibytes of world files and then somehow find a way to make them downloadable for everyone?

Well look no more because on a sleepless night I wrote this lil CLI/HTTP server thing that allows you to do just that with minimal setup.

Just run the ol' command:

``mwdh -p <path-to-mc-server-folder> -ne``

And *boom* it creates a .zip archive of the world with nether and end and hosts it for others to download. yay. 

You may need to fiddle around with your proxy so that others can actually download it from the external network but I'll add that to this README in just a sec. hold on ok? patient pls :3
