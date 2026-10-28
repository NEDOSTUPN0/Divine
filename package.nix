{ lib
, callPackage
, rustPlatform
, pkg-config
, wrapGAppsHook4
, gtk4
, libadwaita
, glib
, gst_all_1
, legendary-gl
, umu-launcher
}:

let
  epic-shim = callPackage ./nix/epic-shim.nix { };
in
rustPlatform.buildRustPackage {
  pname = "divine";
  version = "1.0.0";

  src = lib.cleanSourceWith {
    src = ./.;
    filter = path: type:
      let base = baseNameOf (toString path); in
      base != "target" && base != "result" && base != ".git";
  };

  cargoLock.lockFile = ./Cargo.lock;

  nativeBuildInputs = [ pkg-config wrapGAppsHook4 ];
  buildInputs = [
    gtk4
    libadwaita
    glib
    # Trailers. gst-plugins-rs holds gtk4paintablesink, which is what draws a
    # pipeline's frames into a GTK widget; libav decodes what the store serves.
    # wrapGAppsHook4 puts them all on GST_PLUGIN_SYSTEM_PATH_1_0.
    gst_all_1.gstreamer
    gst_all_1.gst-plugins-base
    gst_all_1.gst-plugins-good
    gst_all_1.gst-plugins-bad
    gst_all_1.gst-plugins-rs
    gst_all_1.gst-libav
  ];

  postInstall = ''
    install -Dm644 data/io.github.NEDOSTUPN0.Divine.desktop \
      $out/share/applications/io.github.NEDOSTUPN0.Divine.desktop
    cp -r data/icons $out/share/icons
    # What DBusActivatable in the desktop entry promises. GApplication answers
    # --gapplication-service by itself; the file only says where to find it,
    # and D-Bus wants that spelled out in full rather than looked up on PATH.
    install -Dm644 data/io.github.NEDOSTUPN0.Divine.service \
      $out/share/dbus-1/services/io.github.NEDOSTUPN0.Divine.service
    substituteInPlace $out/share/dbus-1/services/io.github.NEDOSTUPN0.Divine.service \
      --replace-fail "@bindir@" "$out/bin"

    # Looked up at launch time and copied into the game's Wine prefix.
    install -Dm644 ${epic-shim}/EpicGamesLauncher.exe \
      $out/share/divine/EpicGamesLauncher.exe
  '';

  # legendary and umu-run are looked up on $PATH, so a user-installed or
  # distro version keeps priority. Append the store paths only as a fallback.
  preFixup = ''
    gappsWrapperArgs+=(--suffix PATH : ${lib.makeBinPath [ legendary-gl umu-launcher ]})
  '';

  meta = {
    description = "Native GTK4 launcher for Epic Games, powered by Legendary";
    homepage = "https://github.com/NEDOSTUPN0/Divine";
    license = lib.licenses.gpl3Plus;
    platforms = lib.platforms.linux;
    mainProgram = "divine";
  };
}
