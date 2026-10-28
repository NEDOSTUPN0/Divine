{
  description = "Native GTK4 launcher for Epic Games, powered by Legendary";

  inputs = {
    nixpkgs.url = "github:nixos/nixpkgs/nixos-unstable";
  };

  outputs = { self, nixpkgs }:
    let
      systems = [ "x86_64-linux" "aarch64-linux" ];
      forAllSystems = f: nixpkgs.lib.genAttrs systems (system: f nixpkgs.legacyPackages.${system});
    in
    {
      packages = forAllSystems (pkgs: rec {
        divine = pkgs.callPackage ./package.nix { };
        epic-shim = pkgs.callPackage ./nix/epic-shim.nix { };
        default = divine;
      });

      overlays.default = final: prev: {
        divine = final.callPackage ./package.nix { };
      };

      devShells = forAllSystems (pkgs: {
        default = pkgs.mkShell {
          nativeBuildInputs = with pkgs; [
            cargo
            rustc
            rustfmt
            clippy
            rust-analyzer
            pkg-config
          ];

          buildInputs = with pkgs; [
            gtk4
            libadwaita
            glib
            # Trailers. gst-plugins-rs is the one that matters: it holds
            # gtk4paintablesink, which is how a pipeline hands frames to a
            # GTK widget. libav decodes the H.264 the store serves.
            gst_all_1.gstreamer
            gst_all_1.gst-plugins-base
            gst_all_1.gst-plugins-good
            gst_all_1.gst-plugins-bad
            gst_all_1.gst-plugins-rs
            gst_all_1.gst-libav
          ];

          # Divine shells out to these, so the shell can drive the app end to end.
          packages = with pkgs; [
            legendary-gl
            umu-launcher
          ];

          RUST_SRC_PATH = "${pkgs.rustPlatform.rustLibSrc}";
          # Outside a packaged install Divine has no share/divine to look in.
          DIVINE_EPIC_SHIM =
            "${pkgs.callPackage ./nix/epic-shim.nix { }}/EpicGamesLauncher.exe";

          # So the window and the About dialog find the icon before it is installed.
          shellHook = ''
            export XDG_DATA_DIRS="$PWD/data''${XDG_DATA_DIRS:+:$XDG_DATA_DIRS}"
          '';
        };
      });
    };
}
