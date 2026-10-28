{ lib, pkgsCross }:

# Built as a Windows binary because it runs inside the game's Wine prefix, not
# on the host. See vendor/heroic-epic-integration for what it does and why.
pkgsCross.mingwW64.stdenv.mkDerivation {
  pname = "divine-epic-launcher-shim";
  version = "1.0.0";

  src = ../vendor/heroic-epic-integration;

  # -municode belongs on the link step only, exactly as upstream's toolchain
  # file has it: it selects the wmain entry point. Passing it while compiling
  # would also define UNICODE, and SetEnvironmentVariable would then resolve to
  # the wide variant and silently fail to clear SteamAppId.
  buildPhase = ''
    runHook preBuild
    $CC -O2 -c -o main.o main.c
    $CC -municode -o EpicGamesLauncher.exe main.o -lshell32
    runHook postBuild
  '';

  installPhase = ''
    runHook preInstall
    install -Dm644 EpicGamesLauncher.exe $out/EpicGamesLauncher.exe
    runHook postInstall
  '';

  meta = {
    description = "Wrapper that makes Epic games see an Epic Games Launcher parent process";
    homepage = "https://github.com/Etaash-mathamsetty/heroic-epic-integration";
    license = lib.licenses.mit;
    platforms = lib.platforms.windows;
  };
}
