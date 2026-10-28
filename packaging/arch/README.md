# Arch Linux

The PKGBUILD for Arch, named `divine-launcher` so it can go into the AUR as it
is: the plain name `divine` belongs to the DiVinE model checker, which installs
a `/usr/bin/divine` of its own, so the two conflict.

It builds from the release tarball GitHub makes for a tag, so the checksum can
only be filled in once the tag exists. Here it stays `SKIP`; the copy pushed to
the AUR carries the real one.

## Publishing a release

1. Tag the release and push it, so `v$pkgver.tar.gz` exists on GitHub.
2. Here, set `pkgver` (and reset `pkgrel` to 1), then:

   ```
   updpkgsums
   makepkg --printsrcinfo > .SRCINFO
   makepkg -si
   ```

3. Copy `PKGBUILD` and `.SRCINFO` into a clone of
   `ssh://aur@aur.archlinux.org/divine-launcher.git`, commit and push.

`legendary` is in the AUR and `umu-launcher` in multilib, so `makepkg -s` can
install neither by itself: build `legendary` from the AUR first, and enable
multilib in `/etc/pacman.conf`. An AUR helper does both.
