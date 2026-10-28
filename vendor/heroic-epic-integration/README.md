# heroic-epic-integration

Vendored, unmodified, from <https://github.com/Etaash-mathamsetty/heroic-epic-integration>.

Copyright (c) 2025 Etaash Mathamsetty, MIT licensed — see `LICENSE`.

Divine builds `main.c` into `EpicGamesLauncher.exe` and copies it into a game's
Wine prefix. Games from Rockstar check how they were started: the wrapper carries
the file name the Epic Games Launcher would have, clears `SteamAppId` so the
Rockstar launcher stops trying to initialise the Steam API, and then starts the
real executable and waits for it. Without it those titles come up as not owned
and offer to sell you the game.

Enable it per game with the "Fake Epic Games Launcher" switch. Heroic exposes the
same thing as the `USE_FAKE_EPIC_EXE` environment variable.
