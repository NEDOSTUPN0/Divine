# Arch Linux

The PKGBUILD for Arch, named `divine-launcher` so it can go into the AUR as it
is: the plain name `divine` belongs to the DiVinE model checker, which installs
a `/usr/bin/divine` of its own, so the two conflict.

It builds from the release tarball GitHub makes for a tag, so the checksum can
only be filled in once the tag exists. Here it stays `SKIP`; the copy pushed to
the AUR carries the real one.

`../arch-bin` is the same package prebuilt, `divine-launcher-bin`:
`.github/workflows/release.yml` runs this PKGBUILD on Arch for every tag and
attaches what it built to the release, and that PKGBUILD only unpacks it.

## Publishing a release

1. Set `pkgver` in both PKGBUILDs to the new version, and `pkgrel` back to 1.
   The workflow refuses a tag this one does not match.
2. Tag the release and push it. GitHub makes `v$pkgver.tar.gz`, and the
   workflow attaches `divine-$pkgver-x86_64.tar.gz` a few minutes later.
3. In each of the two directories:

   ```
   updpkgsums
   makepkg --printsrcinfo > .SRCINFO
   makepkg -si
   ```

4. Copy `PKGBUILD` and `.SRCINFO` into clones of
   `ssh://aur@aur.archlinux.org/divine-launcher.git` and
   `ssh://aur@aur.archlinux.org/divine-launcher-bin.git`, commit and push.

`legendary` is in the AUR and `umu-launcher` in multilib, so `makepkg -s` can
install neither by itself: build `legendary` from the AUR first, and enable
multilib in `/etc/pacman.conf`. An AUR helper does both.
